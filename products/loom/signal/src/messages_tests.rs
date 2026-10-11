use super::*;
use presage::libsignal_service::content::{Content, Metadata};
use presage_store_sqlite::{OnNewIdentity, SqliteConnectOptions};
use std::time::{Duration, UNIX_EPOCH};

struct Fixture {
    _directory: tempfile::TempDir,
    store: SqliteStore,
    database: SqlitePool,
    account: ServiceId,
    thread: Thread,
}

impl Fixture {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(directory.path().join("signal.db"))
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
        sqlx::query("CREATE TABLE loom_retention_v1(thread_id INTEGER NOT NULL, ts INTEGER NOT NULL, expires_at INTEGER, PRIMARY KEY(thread_id, ts))")
            .execute(&database).await.unwrap();
        let account = ServiceId::from(Aci::from(sqlx::types::Uuid::from_bytes([1; 16])));
        Self {
            _directory: directory,
            store,
            database,
            account,
            thread: Thread::Contact(account),
        }
    }

    async fn save(&mut self, timestamp: u64, body: DataMessage) {
        let time = UNIX_EPOCH + Duration::from_millis(timestamp);
        self.store
            .save_message(
                &self.thread,
                Content::from_body(
                    body,
                    Metadata {
                        sender: self.account,
                        destination: self.account,
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

    async fn page(&self, before: Option<u64>, limit: usize) -> Page {
        page(
            &self.store,
            &self.database,
            &self.thread,
            self.account,
            before,
            limit,
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn metadata_only_pages_keep_older_chat_reachable() {
    let mut fixture = Fixture::open().await;
    let start = now();
    fixture
        .save(
            start,
            DataMessage {
                body: Some("Still here beneath the updates".into()),
                ..Default::default()
            },
        )
        .await;
    for offset in 1..=MAX_PAGE_SIZE {
        fixture
            .save(start + offset as u64, DataMessage::default())
            .await;
    }
    let first = fixture.page(None, MAX_PAGE_SIZE).await;
    assert!(first.messages.is_empty());
    assert_eq!(first.next_before, Some(start + 1));
    let earlier = fixture.page(first.next_before, MAX_PAGE_SIZE).await;
    assert_eq!(earlier.messages.len(), 1);
    assert_eq!(earlier.messages[0].text, "Still here beneath the updates");
    assert_eq!(earlier.next_before, None);
    fixture.database.close().await;
}

#[tokio::test]
async fn byte_limited_pages_do_not_drop_the_first_message_that_did_not_fit() {
    let mut fixture = Fixture::open().await;
    let start = now();
    // JSON escaping expands this beyond its raw text length. Each wire page
    // must remain bounded while every exact message remains reachable.
    let text = "\u{0001}".repeat(MAX_MESSAGE_BYTES);
    for offset in 0..7 {
        fixture
            .save(
                start + offset,
                DataMessage {
                    body: Some(text.clone()),
                    ..Default::default()
                },
            )
            .await;
    }
    let mut before = None;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..7 {
        let page = fixture.page(before, MAX_PAGE_SIZE).await;
        assert!(!page.messages.is_empty());
        assert!(serde_json::to_vec(&page.messages).unwrap().len() <= 1024 * 1024);
        for message in page.messages {
            assert_eq!(message.text, text);
            assert!(seen.insert(message.timestamp), "duplicate message");
        }
        if let Some(next) = page.next_before {
            assert!(before.is_none_or(|previous| next < previous));
            before = Some(next);
        } else {
            break;
        }
    }
    assert_eq!(seen, (start..start + 7).collect());
    fixture.database.close().await;
}

#[tokio::test]
async fn page_limit_and_expiry_preserve_exclusive_cursor_ordering() {
    let mut fixture = Fixture::open().await;
    let start = now() - 30_000;
    for offset in 0..5 {
        fixture
            .save(
                start + offset,
                DataMessage {
                    body: Some(format!("Message {offset}")),
                    expire_timer: (offset == 3).then_some(1),
                    ..Default::default()
                },
            )
            .await;
    }
    let first = fixture.page(None, 2).await;
    assert_eq!(
        first
            .messages
            .iter()
            .map(|m| m.timestamp)
            .collect::<Vec<_>>(),
        [start + 2, start + 4]
    );
    assert_eq!(first.next_before, Some(start + 2));
    let earlier = fixture.page(first.next_before, 2).await;
    assert_eq!(
        earlier
            .messages
            .iter()
            .map(|m| m.timestamp)
            .collect::<Vec<_>>(),
        [start, start + 1]
    );
    assert_eq!(earlier.next_before, None);
    fixture.database.close().await;
}
