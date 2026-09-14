//! Safety-number reviews bind both people's exact public keys. Accepting a
//! review is journaled, then applied by the next worker BEFORE any networking.
//! The process boundary prevents an in-flight decrypt or upstream sync task
//! from restoring an old identity/session after its replacement.
use anyhow::{Result, ensure};
use base64::Engine;
use loom_signal_protocol::{Event, IdentityMember, IdentityReview, IdentityState};
use presage::{
    Manager,
    libsignal_service::{
        prelude::Uuid,
        protocol::{Aci, Fingerprint, IdentityKey, IdentityKeyPair, ServiceId},
    },
    manager::{Registered, RegistrationData},
    store::{ContentsStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{messages, vault::Vault};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Context {
    local_aci: String,
    local_key: Vec<u8>,
    // The recipient's ACI can be addressed through either of our protocol
    // stores. Bind both records, and retire both sets of device sessions.
    trusted_aci: Option<Vec<u8>>,
    trusted_pni: Option<Vec<u8>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Review {
    context: Context,
    candidate: Vec<u8>,
    id: String,
    refreshed_at: Option<u64>,
    verification: Verification,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verification {
    Unverified,
    Approved,
    Verified,
}

pub async fn initialize(database: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_identity_v1(recipient TEXT PRIMARY KEY, body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 4096))")
        .execute(database).await?;
    Ok(())
}

async fn context(connection: &mut SqliteConnection, recipient: &str) -> Result<Context> {
    // These are the schemas of the pinned presage-store-sqlite revision. Read
    // registration and the keypair inside the SAME transaction as the review.
    let registration: Vec<u8> =
        sqlx::query_scalar("SELECT CAST(value AS BLOB) FROM kv WHERE key = 'registration'")
            .fetch_one(&mut *connection)
            .await?;
    let registration: RegistrationData = serde_json::from_slice(&registration)?;
    let pair: Vec<u8> =
        sqlx::query_scalar("SELECT value FROM kv WHERE key = 'identity_keypair_aci'")
            .fetch_one(&mut *connection)
            .await?;
    let pair = IdentityKeyPair::try_from(pair.as_slice())?;
    let trusted_aci =
        sqlx::query_scalar("SELECT record FROM identities WHERE address = ? AND identity = 'aci'")
            .bind(recipient)
            .fetch_optional(&mut *connection)
            .await?;
    let trusted_pni =
        sqlx::query_scalar("SELECT record FROM identities WHERE address = ? AND identity = 'pni'")
            .bind(recipient)
            .fetch_optional(&mut *connection)
            .await?;
    Ok(Context {
        local_aci: ServiceId::from(registration.service_ids.aci()).service_id_string(),
        local_key: pair.identity_key().serialize().into_vec(),
        trusted_aci,
        trusted_pni,
    })
}

async fn load(connection: &mut SqliteConnection, recipient: &str) -> Result<Option<Review>> {
    let body: Option<String> =
        sqlx::query_scalar("SELECT body FROM loom_identity_v1 WHERE recipient = ?")
            .bind(recipient)
            .fetch_optional(connection)
            .await?;
    body.map(|body| serde_json::from_str(&body).map_err(Into::into))
        .transpose()
}

async fn save(connection: &mut SqliteConnection, recipient: &str, review: &Review) -> Result<()> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM loom_identity_v1 WHERE recipient != ?")
            .bind(recipient)
            .fetch_one(&mut *connection)
            .await?;
    ensure!(
        count < messages::MAX_CONVERSATIONS as i64,
        "Safety-number storage is full"
    );
    sqlx::query("INSERT INTO loom_identity_v1(recipient, body) VALUES (?, ?) ON CONFLICT(recipient) DO UPDATE SET body = excluded.body")
        .bind(recipient).bind(serde_json::to_string(review)?).execute(connection).await?;
    Ok(())
}

async fn members(store: &SqliteStore, conversation: &str) -> Result<Vec<IdentityMember>> {
    let known = messages::conversations(store).await?;
    let (_, thread) = known
        .iter()
        .find(|(item, _)| item.id == conversation)
        .ok_or_else(|| anyhow::anyhow!("Unknown Signal conversation"))?;
    let people: Vec<Aci> = match thread {
        Thread::Contact(ServiceId::Aci(aci)) => vec![*aci],
        Thread::Group(key) => store
            .group(*key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Unknown Signal group"))?
            .members
            .into_iter()
            .map(|member| member.aci)
            .collect(),
        Thread::Contact(_) => anyhow::bail!("Safety numbers require an ACI"),
    };
    ensure!(
        people.len() <= messages::MAX_CONVERSATIONS,
        "Signal group is too large"
    );
    let mut result: Vec<_> = people
        .into_iter()
        .map(|aci| {
            let id = ServiceId::from(aci).service_id_string();
            let title = known
                .iter()
                .find(|(_, thread)| *thread == Thread::Contact(aci.into()))
                .map_or_else(|| id.clone(), |(item, _)| item.title.clone());
            IdentityMember { id, title }
        })
        .collect();
    result.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    result.dedup_by(|a, b| a.id == b.id);
    Ok(result)
}

fn recipient(members: &[IdentityMember], requested: Option<&str>) -> Result<Option<String>> {
    match requested {
        Some(id) => {
            ensure!(
                members.iter().any(|person| person.id == id),
                "Select a current conversation member"
            );
            Ok(Some(id.into()))
        }
        None if members.len() == 1 => Ok(Some(members[0].id.clone())),
        None => Ok(None),
    }
}

pub async fn inspect(
    vault: &Vault,
    manager: &mut Manager<SqliteStore, Registered>,
    conversation: &str,
    requested: Option<&str>,
    refresh: bool,
) -> Result<Event> {
    let members = members(&vault.store, conversation).await?;
    let Some(recipient) = recipient(&members, requested)? else {
        return Ok(Event::Identity {
            conversation_id: conversation.into(),
            members,
            review: None,
        });
    };
    let candidate = if refresh {
        let aci = Aci::from(recipient.parse::<Uuid>()?);
        Some(
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                manager.retrieve_identity_key(aci),
            )
            .await??
            .serialize()
            .into_vec(),
        )
    } else {
        None
    };
    let review = inspect_saved(&vault.database, &recipient, candidate).await?;
    Ok(Event::Identity {
        conversation_id: conversation.into(),
        members,
        review,
    })
}

async fn inspect_saved(
    database: &SqlitePool,
    recipient: &str,
    candidate: Option<Vec<u8>>,
) -> Result<Option<IdentityReview>> {
    let mut tx = database.begin().await?;
    let current = context(&mut tx, recipient).await?;
    let old = load(&mut tx, recipient).await?;
    if candidate.is_none()
        && let Some(old) = &old
        && old.context == current
    {
        return Ok(Some(present(recipient, old)?));
    }
    let refreshed_at = candidate.as_ref().map(|_| messages::now());
    let Some(candidate) = candidate
        .or_else(|| current.trusted_aci.clone())
        .or_else(|| current.trusted_pni.clone())
    else {
        return Ok(None);
    };
    IdentityKey::decode(&candidate)?;
    let verified = old.as_ref().is_some_and(|old| {
        old.verification == Verification::Verified
            && old.context == current
            && old.candidate == candidate
    });
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
    let review = Review {
        context: current,
        candidate,
        id: hex::encode(nonce),
        refreshed_at,
        verification: if verified {
            Verification::Verified
        } else {
            Verification::Unverified
        },
    };
    let response = present(recipient, &review)?;
    save(&mut tx, recipient, &review).await?;
    tx.commit().await?;
    Ok(Some(response))
}

pub async fn approve(
    vault: &Vault,
    conversation: &str,
    recipient_id: &str,
    review_id: &str,
) -> Result<Event> {
    let members = members(&vault.store, conversation).await?;
    recipient(&members, Some(recipient_id))?;
    let review = approve_saved(&vault.database, recipient_id, review_id).await?;
    Ok(Event::Identity {
        conversation_id: conversation.into(),
        members,
        review: Some(review),
    })
}

async fn approve_saved(
    database: &SqlitePool,
    recipient: &str,
    review_id: &str,
) -> Result<IdentityReview> {
    ensure!(review_id.len() == 64, "Invalid safety-number review");
    let mut tx = database.begin().await?;
    let mut review = load(&mut tx, recipient)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Review this safety number first"))?;
    ensure!(
        review.id == review_id && review.context == context(&mut tx, recipient).await?,
        "Safety number changed. Review it again."
    );
    if review.verification != Verification::Verified {
        review.verification = Verification::Approved;
    }
    let response = present(recipient, &review)?;
    save(&mut tx, recipient, &review).await?;
    tx.commit().await?;
    Ok(response)
}

pub async fn ensure_send_allowed(
    store: &SqliteStore,
    database: &SqlitePool,
    conversation: &str,
) -> Result<()> {
    let members = members(store, conversation).await?;
    let mut tx = database.begin().await?;
    for member in members {
        if let Some(review) = load(&mut tx, &member.id).await? {
            let current = context(&mut tx, &member.id).await?;
            ensure!(
                review.verification != Verification::Approved
                    && review.context == current
                    && ![&current.trusted_aci, &current.trusted_pni]
                        .into_iter()
                        .flatten()
                        .any(|key| key != &review.candidate),
                "Review this contact's changed safety number first"
            );
        }
    }
    Ok(())
}

/// Called only during startup under Vault's exclusive process lease, before
/// creating any registered manager or receiver. All changes commit together.
pub async fn apply_approved(database: &SqlitePool) -> Result<()> {
    let mut tx = database.begin().await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT recipient, body FROM loom_identity_v1 LIMIT 2001")
            .fetch_all(&mut *tx)
            .await?;
    ensure!(
        rows.len() <= messages::MAX_CONVERSATIONS,
        "Safety-number storage is full"
    );
    for (recipient, body) in rows {
        let mut review: Review = serde_json::from_str(&body)?;
        if review.verification != Verification::Approved {
            continue;
        }
        let current = context(&mut tx, &recipient).await?;
        review.verification = Verification::Unverified;
        if review.context == current {
            IdentityKey::decode(&review.candidate)?;
            // No live task can retain or rewrite these old device or group
            // sender sessions. Accept the recipient's ACI in both local stores.
            for namespace in ["aci", "pni"] {
                sqlx::query("INSERT INTO identities(address, identity, record) VALUES (?, ?, ?) ON CONFLICT(address, identity) DO UPDATE SET record = excluded.record")
                    .bind(&recipient).bind(namespace).bind(&review.candidate).execute(&mut *tx).await?;
            }
            if current.trusted_aci.as_ref() != Some(&review.candidate)
                || current
                    .trusted_pni
                    .as_ref()
                    .is_some_and(|key| key != &review.candidate)
            {
                sqlx::query("DELETE FROM sessions WHERE address = ?")
                    .bind(&recipient)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM sender_keys WHERE address = ?")
                    .bind(&recipient)
                    .execute(&mut *tx)
                    .await?;
            }
            review.context.trusted_aci = Some(review.candidate.clone());
            review.context.trusted_pni = Some(review.candidate.clone());
            review.verification = Verification::Verified;
        } else {
            // A contact sync or another identity update won the race before
            // the old worker exited. Never extend the user's approval to it.
            review.verification = Verification::Unverified;
        }
        save(&mut tx, &recipient, &review).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn present(recipient: &str, review: &Review) -> Result<IdentityReview> {
    let local: Uuid = review.context.local_aci.parse()?;
    let remote: Uuid = recipient.parse()?;
    // Signal Desktop safetyNumber.preload.ts at 6aea489bd04f31f6070538ffdbcea9fcf492479d:
    // version 2, 5200 iterations, and the two raw ACI UUIDs (not their text).
    let fingerprint = Fingerprint::new(
        2,
        5200,
        local.as_bytes(),
        &IdentityKey::decode(&review.context.local_key)?,
        remote.as_bytes(),
        &IdentityKey::decode(&review.candidate)?,
    )
    .map_err(|_| anyhow::anyhow!("Invalid safety-number keys"))?;
    let code = qrcode::QrCode::new(
        fingerprint
            .scannable
            .serialize()
            .map_err(|_| anyhow::anyhow!("Invalid safety-number code"))?,
    )?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(232, 232)
        .build();
    let changed = [&review.context.trusted_aci, &review.context.trusted_pni]
        .into_iter()
        .flatten()
        .any(|key| key != &review.candidate);
    Ok(IdentityReview {
        recipient_id: recipient.into(),
        review_id: review.id.clone(),
        safety_number: fingerprint
            .display_string()
            .map_err(|_| anyhow::anyhow!("Invalid safety number"))?,
        qr_code: format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        ),
        state: if review.verification == Verification::Approved {
            IdentityState::Pending
        } else if changed {
            IdentityState::Changed
        } else if review.verification == Verification::Verified {
            IdentityState::Verified
        } else {
            IdentityState::Unverified
        },
        refreshed_at: review.refreshed_at,
    })
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
