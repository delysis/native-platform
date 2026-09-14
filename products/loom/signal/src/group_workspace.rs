//! Durable, explicitly reviewed workspace announcements in Signal groups.
//! Group revision checks make retrying the exact metadata PATCH idempotent.
//! Its notification is a separate at-most-once send; Check never sends either.
use anyhow::{Result, ensure};
use loom_signal_protocol::{
    GroupNotificationState as Notice, GroupWorkspaceReview, GroupWorkspaceState as State, Workspace,
};
use presage::{
    Manager,
    libsignal_service::prelude::Uuid,
    libsignal_service::{
        groups_v2::{AccessRequired, Group, GroupOperations, Role},
        prelude::{GroupMasterKey, GroupSecretParams},
        protocol::Aci,
    },
    manager::Registered,
    proto::{DataMessage, GroupChange, GroupContextV2},
    store::{ContentsStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use prost::Message;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::time::Duration;

use crate::{identity, messages, workspaces};

/// The authenticated network boundary is replaceable in offline fault tests.
/// Timeouts live in the state machine, including for a stalled transport.
pub(crate) trait GroupClient {
    fn account(&self) -> Aci;
    async fn fetch(&self, key: &[u8; 32]) -> Result<Group>;
    async fn publish(
        &mut self,
        key: &[u8; 32],
        revision: u32,
        description: &[u8],
    ) -> Result<GroupChange>;
    async fn notify(&mut self, key: &[u8; 32], message: DataMessage, timestamp: u64) -> Result<()>;
}

impl GroupClient for Manager<SqliteStore, Registered> {
    fn account(&self) -> Aci {
        self.registration_data().service_ids.aci()
    }
    async fn fetch(&self, key: &[u8; 32]) -> Result<Group> {
        Ok(self.retrieve_group_details(key).await?)
    }
    async fn publish(
        &mut self,
        key: &[u8; 32],
        revision: u32,
        description: &[u8],
    ) -> Result<GroupChange> {
        let change = self
            .publish_group_description(key, revision, description)
            .await?;
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(*key));
        let configuration: presage::libsignal_service::configuration::ServiceConfiguration =
            (&self.registration_data().signal_servers).into();
        let actions = presage::proto::group_change::Actions {
            version: revision,
            modify_description: Some(
                presage::proto::group_change::actions::ModifyDescriptionAction {
                    description: description.to_vec(),
                },
            ),
            ..Default::default()
        };
        crate::group_workspace_crypto::verify_description_change(
            params,
            configuration.zkgroup_server_public_params,
            self.account(),
            actions,
            &change,
        )?;
        Ok(change)
    }
    async fn notify(&mut self, key: &[u8; 32], message: DataMessage, timestamp: u64) -> Result<()> {
        Ok(self.send_message_to_group(key, message, timestamp).await?)
    }
}

pub(crate) struct Sharing<'a> {
    pub store: &'a SqliteStore,
    pub database: &'a SqlitePool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    command: String,
    review: GroupWorkspaceReview,
    encrypted_description: Vec<u8>,
    group_change: Option<Vec<u8>>,
    notification_timestamp: Option<u64>,
}

pub async fn initialize(database: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_group_workspace_v1(id TEXT PRIMARY KEY, conversation TEXT NOT NULL, command TEXT NOT NULL UNIQUE, body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 32768))")
        .execute(database).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS loom_group_workspace_conversation_v1 ON loom_group_workspace_v1(conversation)").execute(database).await?;
    Ok(())
}

async fn load(database: &SqlitePool, conversation: &str) -> Result<Option<Record>> {
    let body: Option<String> = sqlx::query_scalar("SELECT body FROM loom_group_workspace_v1 WHERE conversation = ? ORDER BY rowid DESC LIMIT 1")
        .bind(conversation).fetch_optional(database).await?;
    body.map(|value| serde_json::from_str(&value).map_err(Into::into))
        .transpose()
}

async fn save(database: &SqlitePool, conversation: &str, record: &Record) -> Result<()> {
    let mut tx = database.begin().await?;
    let (count, bytes): (i64, i64) = sqlx::query_as("SELECT count(*), coalesce(sum(length(CAST(body AS BLOB))), 0) FROM loom_group_workspace_v1 WHERE id != ?")
        .bind(&record.review.id).fetch_one(&mut *tx).await?;
    let body = serde_json::to_string(record)?;
    ensure!(
        count < 2000 && bytes as usize + body.len() <= 8 * 1024 * 1024,
        "Group workspace storage is full"
    );
    // Keep old reviews and their uncertain notifications. Replacing the current
    // preview must neither erase a send receipt nor authorize an old review.
    sqlx::query("INSERT INTO loom_group_workspace_v1(id, conversation, command, body) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET body = excluded.body")
        .bind(&record.review.id).bind(conversation).bind(&record.command).bind(body).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn group_key(store: &SqliteStore, conversation: &str) -> Result<[u8; 32]> {
    let Thread::Group(key) = messages::resolve(store, conversation).await? else {
        anyhow::bail!("Only Signal groups have a shared description");
    };
    Ok(key)
}

async fn fresh(manager: &impl GroupClient, key: &[u8; 32]) -> Result<Group> {
    tokio::time::timeout(Duration::from_secs(10), manager.fetch(key)).await?
}

fn require_editor(group: &Group, account: Aci) -> Result<()> {
    ensure!(!group.terminated, "This Signal group has ended");
    let member = group
        .members
        .iter()
        .find(|member| member.aci == account)
        .ok_or_else(|| anyhow::anyhow!("You are no longer in this Signal group"))?;
    let permitted = match group
        .access_control
        .as_ref()
        .map(|access| access.attributes)
    {
        Some(AccessRequired::Any | AccessRequired::Member) => true,
        Some(AccessRequired::Administrator) => member.role == Role::Administrator,
        _ => false,
    };
    ensure!(
        permitted,
        "Only this group's allowed editors can update its description"
    );
    Ok(())
}

fn proposal(before: &str, workspace: &Workspace) -> Result<String> {
    let url = format!("loom://workspace/{}", workspace.id);
    if before.split_whitespace().any(|part| part == url) {
        return Ok(before.into());
    }
    let after = format!(
        "{before}{}{title}\n{url}",
        if before.is_empty() { "" } else { "\n\n" },
        title = workspace.title
    );
    // Conservative Loom limit, counted in UTF-16 units across clients. Never
    // trim someone else's description to fit a link.
    ensure!(
        after.len() <= 4096 && after.encode_utf16().count() <= 480,
        "The group description is full. Shorten it in Signal before adding this workspace."
    );
    Ok(after)
}

impl Sharing<'_> {
    pub async fn current(&self, conversation: &str) -> Result<Option<GroupWorkspaceReview>> {
        group_key(self.store, conversation).await?;
        Ok(load(self.database, conversation)
            .await?
            .map(|record| record.review))
    }

    pub async fn prepare(
        &self,
        manager: &impl GroupClient,
        conversation: &str,
        command: &str,
        workspace_id: Uuid,
    ) -> Result<GroupWorkspaceReview> {
        let key = group_key(self.store, conversation).await?;
        if let Some(old) = load(self.database, conversation).await? {
            if old.command == command {
                ensure!(
                    old.review.workspace.id == workspace_id,
                    "Workspace review ID was reused"
                );
                return Ok(old.review);
            }
            ensure!(
                old.review.state != State::Unconfirmed,
                "Check the previous sharing attempt first"
            );
        }
        let reused: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM loom_group_workspace_v1 WHERE command = ?)",
        )
        .bind(command)
        .fetch_one(self.database)
        .await?;
        ensure!(!reused, "Workspace review request is no longer current");
        let links = workspaces::load(self.database, conversation).await?;
        let workspace = links
            .workspaces
            .into_iter()
            .find(|item| item.id == workspace_id)
            .ok_or_else(|| anyhow::anyhow!("Choose a saved conversation workspace"))?;
        let group = fresh(manager, &key).await?;
        require_editor(&group, manager.account())?;
        let before = group.description_text.unwrap_or_default();
        let after = proposal(&before, &workspace)?;
        let already_present = before == after;
        let revision = if already_present {
            group.version
        } else {
            group
                .version
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Group revision exhausted"))?
        };
        let params = GroupSecretParams::derive_from_master_key(GroupMasterKey::new(key));
        let encrypted_description =
            GroupOperations::new(params).encrypt_description(Some(&after), &mut rand::rng());
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
        let record = Record {
            command: command.into(),
            review: GroupWorkspaceReview {
                id: hex::encode(nonce),
                workspace,
                before,
                after,
                revision,
                state: if already_present {
                    State::Published
                } else {
                    State::Prepared
                },
                notification: Notice::None,
            },
            encrypted_description,
            group_change: None,
            notification_timestamp: None,
        };
        save(self.database, conversation, &record).await?;
        Ok(record.review)
    }

    async fn reviewed(&self, conversation: &str, id: &str) -> Result<([u8; 32], Record)> {
        ensure!(id.len() == 64, "Invalid group workspace review");
        let key = group_key(self.store, conversation).await?;
        let record = load(self.database, conversation)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Preview this workspace link first"))?;
        ensure!(
            record.review.id == id,
            "The group workspace review changed. Preview it again."
        );
        Ok((key, record))
    }
}

fn observe(review: &mut GroupWorkspaceReview, group: &Group) {
    // A read cannot prove that an earlier request was never delivered. Leave
    // an unchanged server revision unconfirmed, so retry keeps the same PATCH.
    if group.version >= review.revision
        && group.description_text.as_deref().unwrap_or_default() == review.after
    {
        review.state = State::Published;
    } else if group.version >= review.revision
        || group.version.checked_add(1) != Some(review.revision)
        || group.description_text.as_deref().unwrap_or_default() != review.before
    {
        review.state = State::Conflict;
    }
}

impl Sharing<'_> {
    pub async fn check(
        &self,
        manager: &impl GroupClient,
        conversation: &str,
        id: &str,
    ) -> Result<GroupWorkspaceReview> {
        let (key, mut record) = self.reviewed(conversation, id).await?;
        let group = fresh(manager, &key).await?;
        observe(&mut record.review, &group);
        save(self.database, conversation, &record).await?;
        Ok(record.review)
    }

    pub async fn publish(
        &self,
        manager: &mut impl GroupClient,
        conversation: &str,
        id: &str,
    ) -> Result<GroupWorkspaceReview> {
        let (key, mut record) = self.reviewed(conversation, id).await?;
        ensure!(
            record.review.state != State::Conflict,
            "The description changed. Preview a new addition."
        );
        let mut group = fresh(manager, &key).await?;
        require_editor(&group, manager.account())?;
        observe(&mut record.review, &group);
        if matches!(record.review.state, State::Prepared | State::Unconfirmed) {
            record.review.state = State::Unconfirmed;
            save(self.database, conversation, &record).await?;
            let published = tokio::time::timeout(
                Duration::from_secs(15),
                manager.publish(&key, record.review.revision, &record.encrypted_description),
            )
            .await;
            if let Ok(Ok(change)) = published {
                record.group_change = Some(change.encode_to_vec());
                record.review.state = State::Published;
            }
        }
        save(self.database, conversation, &record).await?;
        if record.review.state == State::Published && group.version < record.review.revision {
            group.version = record.review.revision;
            group.description_text = Some(record.review.after.clone());
            self.store.save_group(key, group).await?;
        }
        Ok(record.review)
    }

    pub async fn notify(
        &self,
        manager: &mut impl GroupClient,
        conversation: &str,
        id: &str,
    ) -> Result<GroupWorkspaceReview> {
        let (key, mut record) = self.reviewed(conversation, id).await?;
        ensure!(
            record.review.state == State::Published,
            "Check the group description before notifying anyone"
        );
        if record.review.notification != Notice::None {
            return Ok(record.review);
        }
        let group = fresh(manager, &key).await?;
        require_editor(&group, manager.account())?;
        observe(&mut record.review, &group);
        if record.review.state != State::Published {
            save(self.database, conversation, &record).await?;
            return Ok(record.review);
        }
        identity::ensure_send_allowed(self.store, self.database, conversation).await?;
        let timestamp = messages::now();
        ensure!(timestamp <= i64::MAX as u64, "Invalid clock");
        record.notification_timestamp = Some(timestamp);
        record.review.notification = Notice::Unconfirmed;
        save(self.database, conversation, &record).await?;
        let message = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(key.to_vec()),
                revision: Some(group.version),
                // An intervening membership change needs the current group state. Do
                // not attach an old signed delta to a newer advertised revision.
                group_change: if group.version == record.review.revision {
                    record.group_change.clone()
                } else {
                    None
                },
            }),
            ..Default::default()
        };
        if matches!(
            tokio::time::timeout(
                Duration::from_secs(20),
                manager.notify(&key, message, timestamp)
            )
            .await,
            Ok(Ok(()))
        ) {
            record.review.notification = Notice::Sent;
            save(self.database, conversation, &record).await?;
        }
        Ok(record.review)
    }
}

#[cfg(test)]
#[path = "group_workspace_tests.rs"]
mod tests;
