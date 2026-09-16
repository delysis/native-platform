//! Purge expired and view-once content, including records left by a crash
//! between Presage storing a message and notifying our receive loop.
use anyhow::Result;
use presage::proto::{self, DataMessage, sync_message};
use prost::Message;
use sqlx::{Row, SqlitePool};

pub async fn sweep(database: &SqlitePool) -> Result<bool> {
    loop {
        let rows = sqlx::query("SELECT m.thread_id, m.ts, m.server_ts, m.content_body, g.disappearing_messages_timer AS timer FROM thread_messages m LEFT JOIN threads t ON m.thread_id = t.id LEFT JOIN groups g ON t.group_master_key = g.master_key WHERE NOT EXISTS(SELECT 1 FROM loom_retention_v1 r WHERE r.thread_id = m.thread_id AND r.ts = m.ts) LIMIT 128")
            .fetch_all(database).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let thread: i64 = row.try_get("thread_id")?;
            let sent: i64 = row.try_get("ts")?;
            let bytes: Vec<u8> = row.try_get("content_body")?;
            let content = proto::Content::decode(bytes.as_slice())?;
            let timestamp: i64 = row.try_get::<Option<i64>, _>("server_ts")?.unwrap_or(sent);
            let timer: Option<i64> = row.try_get("timer")?;
            let expires = data(&content)
                .and_then(|message| expiration(message, timestamp, timer, crate::messages::now()));
            // The timer is captured before receiving the next Signal event.
            // Later group-setting changes cannot extend this message's life.
            sqlx::query("INSERT OR IGNORE INTO loom_retention_v1(thread_id, ts, expires_at) VALUES (?, ?, ?)")
                .bind(thread).bind(sent).bind(expires.map(|value| value as i64)).execute(database).await?;
        }
    }
    let removed = sqlx::query("DELETE FROM thread_messages WHERE EXISTS(SELECT 1 FROM loom_retention_v1 r WHERE r.thread_id = thread_messages.thread_id AND r.ts = thread_messages.ts AND r.expires_at <= ?)")
        .bind(crate::messages::now() as i64).execute(database).await?.rows_affected() > 0;
    sqlx::query("DELETE FROM loom_retention_v1 WHERE NOT EXISTS(SELECT 1 FROM thread_messages m WHERE m.thread_id = loom_retention_v1.thread_id AND m.ts = loom_retention_v1.ts)").execute(database).await?;
    Ok(removed)
}

fn data(content: &proto::Content) -> Option<&DataMessage> {
    match content.content.as_ref()? {
        proto::content::Content::DataMessage(message) => Some(message),
        proto::content::Content::EditMessage(edit) => edit.data_message.as_ref(),
        proto::content::Content::SyncMessage(sync) => match &sync.content {
            Some(sync_message::Content::Sent(sent)) => sent.message.as_ref().or_else(|| {
                sent.edit_message
                    .as_ref()
                    .and_then(|edit| edit.data_message.as_ref())
            }),
            _ => None,
        },
        _ => None,
    }
}

fn expiration(
    data: &DataMessage,
    timestamp: i64,
    group_timer: Option<i64>,
    first_seen: u64,
) -> Option<u64> {
    if data.is_view_once.unwrap_or(false) {
        return Some(0);
    }
    let timer = data
        .expire_timer
        .map(i64::from)
        .or(group_timer)
        .unwrap_or(0);
    (timer > 0).then(|| {
        u64::try_from(timestamp)
            .unwrap_or(0)
            .min(first_seen)
            .saturating_add(timer as u64 * 1000)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn crash_sweep_removes_expired_bodies_and_captures_group_timer_once() {
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE threads(id INTEGER PRIMARY KEY, group_master_key BLOB)",
            "CREATE TABLE groups(master_key BLOB PRIMARY KEY, disappearing_messages_timer INTEGER)",
            "CREATE TABLE thread_messages(thread_id INTEGER, ts INTEGER, server_ts INTEGER, content_body BLOB, PRIMARY KEY(thread_id, ts))",
            "CREATE TABLE loom_retention_v1(thread_id INTEGER, ts INTEGER, expires_at INTEGER, PRIMARY KEY(thread_id, ts))",
            "INSERT INTO groups VALUES (X'01', 60)",
            "INSERT INTO threads VALUES (1, X'01')",
        ] {
            sqlx::query(statement).execute(&database).await.unwrap();
        }
        let now = crate::messages::now() as i64;
        for (timestamp, message) in [
            (
                now - 120_000,
                DataMessage {
                    body: Some("expired before the crash".into()),
                    ..Default::default()
                },
            ),
            (
                now - 1_000,
                DataMessage {
                    body: Some("short-lived".into()),
                    ..Default::default()
                },
            ),
            (
                now,
                DataMessage {
                    body: Some("never show view-once".into()),
                    is_view_once: Some(true),
                    ..Default::default()
                },
            ),
        ] {
            let content = proto::Content {
                content: Some(proto::content::Content::DataMessage(message)),
                ..Default::default()
            };
            sqlx::query("INSERT INTO thread_messages VALUES (1, ?, ?, ?)")
                .bind(timestamp)
                .bind(timestamp)
                .bind(content.encode_to_vec())
                .execute(&database)
                .await
                .unwrap();
        }
        assert!(sweep(&database).await.unwrap());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM thread_messages")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let expiry: i64 = sqlx::query_scalar("SELECT expires_at FROM loom_retention_v1")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(expiry, now - 1_000 + 60_000);
        sqlx::query("UPDATE groups SET disappearing_messages_timer = 0")
            .execute(&database)
            .await
            .unwrap();
        assert!(!sweep(&database).await.unwrap());
        let retained: i64 = sqlx::query_scalar("SELECT expires_at FROM loom_retention_v1")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(retained, expiry);
        database.close().await;
    }

    #[test]
    fn reconnecting_never_extends_retention() {
        let message = DataMessage {
            expire_timer: Some(5),
            ..Default::default()
        };
        assert_eq!(expiration(&message, 1000, None, 5999), Some(6000));
        assert_eq!(expiration(&message, 1000, None, 100000), Some(6000));
    }
    #[test]
    fn view_once_never_enters_the_pane() {
        let message = DataMessage {
            is_view_once: Some(true),
            ..Default::default()
        };
        assert_eq!(expiration(&message, 1000, None, 1000), Some(0));
        assert_eq!(
            expiration(&DataMessage::default(), 1000, None, 100000),
            None
        );
        assert_eq!(
            expiration(&DataMessage::default(), 1000, Some(5), 6000),
            Some(6000)
        );
    }
}
