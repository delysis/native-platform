use super::*;
use presage::{
    libsignal_service::{
        groups_v2::{AccessControl, Member, Timer},
        prelude::ProfileKey,
    },
    store::ContentsStore,
};
use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions};
use std::cell::RefCell;

const KEY: [u8; 32] = [9; 32];
fn account() -> Aci {
    Aci::from(Uuid::from_bytes([1; 16]))
}
fn workspace() -> Workspace {
    Workspace {
        id: Uuid::from_bytes([2; 16]),
        title: "Our garden".into(),
    }
}
fn group() -> Group {
    Group {
        title: "Friends".into(),
        avatar: "unchanged-avatar".into(),
        disappearing_messages_timer: Some(Timer { duration: 300 }),
        access_control: Some(AccessControl {
            attributes: AccessRequired::Member,
            members: AccessRequired::Administrator,
            add_from_invite_link: AccessRequired::Unsatisfiable,
            member_label: AccessRequired::Member,
        }),
        version: 7,
        members: vec![Member {
            aci: account(),
            role: Role::Default,
            profile_key: ProfileKey::create([0; 32]),
            joined_at_version: 1,
            label: None,
            label_emoji: None,
        }],
        members_pending_profile_key: vec![],
        members_pending_admin_approval: vec![],
        invite_link_password: vec![],
        description_text: Some("Keep this description.\n🌱".into()),
        announcements_only: false,
        members_banned: vec![],
        terminated: false,
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    store: SqliteStore,
    database: SqlitePool,
}
impl Fixture {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (store, database) = Self::connect(directory.path()).await;
        store.save_group(KEY, group()).await.unwrap();
        workspaces::update(
            &database,
            &messages::id(&Thread::Group(KEY)),
            "bookmark",
            0,
            workspace(),
            false,
        )
        .await
        .unwrap();
        Self {
            directory,
            store,
            database,
        }
    }
    async fn connect(path: &std::path::Path) -> (SqliteStore, SqlitePool) {
        let options = SqliteConnectOptions::new()
            .filename(path.join("signal.db"))
            .create_if_missing(true)
            .pragma("key", "'fixture-only'");
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let store = SqliteStore::open_with_pool(database.clone(), OnNewIdentity::Reject)
            .await
            .unwrap();
        initialize(&database).await.unwrap();
        workspaces::initialize(&database).await.unwrap();
        identity::initialize(&database).await.unwrap();
        (store, database)
    }
    fn sharing(&self) -> Sharing<'_> {
        Sharing {
            store: &self.store,
            database: &self.database,
        }
    }
    fn server(&self) -> Server {
        Server {
            group: RefCell::new(group()),
            database: self.database.clone(),
            patches: vec![],
            notices: vec![],
            accept: true,
            lose_reply: false,
            after: proposal(group().description_text.as_deref().unwrap(), &workspace()).unwrap(),
        }
    }
    async fn reopen(self) -> Self {
        self.database.close().await;
        drop(self.store);
        let (store, database) = Self::connect(self.directory.path()).await;
        Self {
            directory: self.directory,
            store,
            database,
        }
    }
    async fn close(self) {
        self.database.close().await;
    }
}

struct Server {
    group: RefCell<Group>,
    database: SqlitePool,
    patches: Vec<(u32, Vec<u8>)>,
    notices: Vec<DataMessage>,
    accept: bool,
    lose_reply: bool,
    after: String,
}
impl GroupClient for Server {
    fn account(&self) -> Aci {
        account()
    }
    async fn fetch(&self, key: &[u8; 32]) -> Result<Group> {
        assert_eq!(key, &KEY);
        Ok(self.group.borrow().clone())
    }
    async fn publish(
        &mut self,
        key: &[u8; 32],
        revision: u32,
        description: &[u8],
    ) -> Result<GroupChange> {
        let saved = load(&self.database, &messages::id(&Thread::Group(*key)))
            .await?
            .unwrap();
        assert_eq!(
            saved.review.state,
            State::Unconfirmed,
            "uncertainty is durable before sending"
        );
        assert_eq!(saved.encrypted_description, description);
        self.patches.push((revision, description.to_vec()));
        let mut group = self.group.borrow_mut();
        ensure!(
            self.accept && group.version.checked_add(1) == Some(revision),
            "not applied"
        );
        group.version = revision;
        group.description_text = Some(self.after.clone());
        ensure!(!self.lose_reply, "reply lost after commit");
        Ok(GroupChange::default()) // Crypto verification is tested at the Presage boundary.
    }
    async fn notify(&mut self, key: &[u8; 32], message: DataMessage, timestamp: u64) -> Result<()> {
        let saved = load(&self.database, &messages::id(&Thread::Group(*key)))
            .await?
            .unwrap();
        assert_eq!(saved.review.notification, Notice::Unconfirmed);
        assert_eq!(saved.notification_timestamp, Some(timestamp));
        self.notices.push(message);
        ensure!(!self.lose_reply, "notification reply lost");
        Ok(())
    }
}

#[tokio::test]
async fn lost_publish_reply_is_checked_after_reopen_without_resending_or_changing_other_attributes()
{
    let fixture = Fixture::open().await;
    let mut server = fixture.server();
    server.lose_reply = true;
    let conversation = messages::id(&Thread::Group(KEY));
    let review = fixture
        .sharing()
        .prepare(&server, &conversation, "prepare-one", workspace().id)
        .await
        .unwrap();
    assert!(review.after.starts_with(&review.before));
    let event = serde_json::to_string(&review).unwrap();
    assert!(!event.contains("encrypted_description") && !event.contains("group_change"));
    assert_eq!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Unconfirmed
    );
    assert!(
        fixture
            .sharing()
            .prepare(&server, &conversation, "prepare-two", workspace().id)
            .await
            .is_err()
    );
    let fixture = fixture.reopen().await;
    server.database = fixture.database.clone();
    assert_eq!(
        fixture
            .sharing()
            .check(&server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Published
    );
    assert_eq!(server.patches.len(), 1);
    assert!(server.notices.is_empty());
    fixture
        .sharing()
        .publish(&mut server, &conversation, &review.id)
        .await
        .unwrap();
    assert_eq!(server.patches.len(), 1);
    let mut expected = group();
    expected.version = 8;
    expected.description_text = Some(review.after.clone());
    assert_eq!(*server.group.borrow(), expected);
    assert_eq!(
        fixture
            .sharing()
            .notify(&mut server, &conversation, &review.id)
            .await
            .unwrap()
            .notification,
        Notice::Unconfirmed
    );
    fixture
        .sharing()
        .notify(&mut server, &conversation, &review.id)
        .await
        .unwrap();
    assert_eq!(server.notices.len(), 1);
    let notice = &server.notices[0];
    assert!(
        notice.body.is_none() && notice.expire_timer.is_none() && notice.attachments.is_empty()
    );
    assert_eq!(notice.group_v2.as_ref().unwrap().revision, Some(8));
    // A settled edit with an uncertain notification can be superseded. Its old
    // receipt survives, and its old ID cannot send or change anything again.
    let next = fixture
        .sharing()
        .prepare(&server, &conversation, "prepare-two", workspace().id)
        .await
        .unwrap();
    assert_ne!(next.id, review.id);
    assert!(
        fixture
            .sharing()
            .notify(&mut server, &conversation, &review.id)
            .await
            .is_err()
    );
    assert!(
        fixture
            .sharing()
            .prepare(&server, &conversation, "prepare-one", workspace().id)
            .await
            .is_err()
    );
    let old: String = sqlx::query_scalar("SELECT body FROM loom_group_workspace_v1 WHERE id = ?")
        .bind(&review.id)
        .fetch_one(&fixture.database)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Record>(&old)
            .unwrap()
            .review
            .notification,
        Notice::Unconfirmed
    );
    fixture.close().await;
}

#[tokio::test]
async fn undelivered_update_reuses_exact_ciphertext_and_rejects_a_later_description_or_permission_change()
 {
    let fixture = Fixture::open().await;
    let mut server = fixture.server();
    server.accept = false;
    let conversation = messages::id(&Thread::Group(KEY));
    let review = fixture
        .sharing()
        .prepare(&server, &conversation, "one", workspace().id)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Unconfirmed
    );
    assert_eq!(
        fixture
            .sharing()
            .check(&server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Unconfirmed
    );
    server.accept = true;
    assert_eq!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Published
    );
    assert_eq!(server.patches[0], server.patches[1]);
    server.group.borrow_mut().version += 1;
    server.group.borrow_mut().description_text = Some("Someone else edited this.".into());
    assert_eq!(
        fixture
            .sharing()
            .notify(&mut server, &conversation, &review.id)
            .await
            .unwrap()
            .state,
        State::Conflict
    );
    assert!(server.notices.is_empty());
    let next = fixture
        .sharing()
        .prepare(&server, &conversation, "two", workspace().id)
        .await
        .unwrap();
    server
        .group
        .borrow_mut()
        .access_control
        .as_mut()
        .unwrap()
        .attributes = AccessRequired::Administrator;
    assert!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &next.id)
            .await
            .is_err()
    );
    assert_eq!(server.patches.len(), 2);
    fixture.close().await;
}

#[tokio::test]
async fn stale_preview_and_failed_durable_reservation_never_reach_the_transport() {
    let fixture = Fixture::open().await;
    let mut server = fixture.server();
    let conversation = messages::id(&Thread::Group(KEY));
    let first = fixture
        .sharing()
        .prepare(&server, &conversation, "one", workspace().id)
        .await
        .unwrap();
    let second = fixture
        .sharing()
        .prepare(&server, &conversation, "two", workspace().id)
        .await
        .unwrap();
    assert_ne!(first.id, second.id);
    assert!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &first.id)
            .await
            .is_err()
    );
    sqlx::query("CREATE TRIGGER disk_full BEFORE UPDATE ON loom_group_workspace_v1 BEGIN SELECT RAISE(ABORT, 'fixture disk full'); END").execute(&fixture.database).await.unwrap();
    assert!(
        fixture
            .sharing()
            .publish(&mut server, &conversation, &second.id)
            .await
            .is_err()
    );
    assert!(server.patches.is_empty());
    assert_eq!(
        fixture
            .sharing()
            .current(&conversation)
            .await
            .unwrap()
            .unwrap()
            .state,
        State::Prepared
    );
    fixture.close().await;
}

#[test]
fn permissions_and_description_budget_preserve_existing_content() {
    let mut group = group();
    assert!(require_editor(&group, account()).is_ok());
    group.terminated = true;
    assert!(require_editor(&group, account()).is_err());
    group.terminated = false;
    group.access_control.as_mut().unwrap().attributes = AccessRequired::Administrator;
    assert!(require_editor(&group, account()).is_err());
    group.members[0].role = Role::Administrator;
    assert!(require_editor(&group, account()).is_ok());
    group.members.clear();
    assert!(require_editor(&group, account()).is_err());
    let existing = format!("Existing description\nloom://workspace/{}", workspace().id);
    assert_eq!(proposal(&existing, &workspace()).unwrap(), existing);
    assert!(proposal(&"🌱".repeat(240), &workspace()).is_err());
    let mut long_title = workspace();
    long_title.title = "x".repeat(512);
    assert!(proposal("", &long_title).is_err());
}

#[tokio::test]
async fn group_metadata_notifications_do_not_become_empty_chat_messages() {
    use presage::libsignal_service::content::{Content, Metadata};
    use presage::proto::AttachmentPointer;
    let fixture = Fixture::open().await;
    sqlx::query("CREATE TABLE loom_retention_v1(thread_id INTEGER NOT NULL, ts INTEGER NOT NULL, expires_at INTEGER, PRIMARY KEY(thread_id, ts))").execute(&fixture.database).await.unwrap();
    let thread = Thread::Group(KEY);
    let now = messages::now();
    for (offset, body) in [
        DataMessage {
            body: Some("Hello from our garden".into()),
            ..Default::default()
        },
        DataMessage {
            attachments: vec![AttachmentPointer::default()],
            ..Default::default()
        },
        DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let time = std::time::UNIX_EPOCH + Duration::from_millis(now + offset as u64);
        fixture
            .store
            .save_message(
                &thread,
                Content::from_body(
                    body,
                    Metadata {
                        sender: account().into(),
                        destination: account().into(),
                        sender_device: 1_u32.try_into().unwrap(),
                        pni_verified: None,
                        client_timestamp: time.into(),
                        server_timestamp: time.into(),
                        needs_receipt: false,
                        unidentified_sender: false,
                        was_plaintext: false,
                        server_guid: None,
                    },
                ),
            )
            .await
            .unwrap();
    }
    let page = messages::page(
        &fixture.store,
        &fixture.database,
        &thread,
        account().into(),
        None,
        100,
    )
    .await
    .unwrap();
    assert_eq!(
        page.len(),
        2,
        "metadata belongs to the group state, not the chat transcript"
    );
    assert_eq!(page[0].text, "Hello from our garden");
    assert_eq!(page[1].attachment_count, 1);
    fixture.close().await;
}

#[tokio::test]
async fn group_cache_preserves_current_avatars_and_invalidates_them_when_the_path_changes() {
    let fixture = Fixture::open().await;
    fixture
        .store
        .save_group_avatar(KEY, &vec![1, 2, 3])
        .await
        .unwrap();
    let mut snapshot = group();
    snapshot.version += 1;
    fixture
        .store
        .save_group(KEY, snapshot.clone())
        .await
        .unwrap();
    assert_eq!(
        fixture.store.group_avatar(KEY).await.unwrap(),
        Some(vec![1, 2, 3])
    );
    let mut stale = group();
    stale.avatar = "stale-avatar".into();
    fixture.store.save_group(KEY, stale).await.unwrap();
    assert_eq!(
        fixture.store.group_avatar(KEY).await.unwrap(),
        Some(vec![1, 2, 3])
    );
    snapshot.version += 1;
    snapshot.avatar = "new-avatar".into();
    fixture.store.save_group(KEY, snapshot).await.unwrap();
    assert!(fixture.store.group_avatar(KEY).await.unwrap().is_none());
    fixture.close().await;
}
