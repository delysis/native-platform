//! Authored drafts stay inside the encrypted Signal database. A lost reply can
//! repeat the same write; an older editor cannot overwrite a newer draft.
use anyhow::{Result, ensure};
use loom_signal_protocol::{Draft, MAX_MESSAGE_BYTES, SendAttempt};
use sqlx::SqlitePool;

pub async fn load(database: &SqlitePool, conversation: &str) -> Result<Draft> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT body FROM loom_drafts_v1 WHERE conversation = ?")
            .bind(conversation)
            .fetch_optional(database)
            .await?;
    row.map_or_else(
        || Ok(Draft::default()),
        |body| Ok(serde_json::from_str(&body)?),
    )
}

pub async fn save(
    database: &SqlitePool,
    conversation: &str,
    request: &str,
    expected: u64,
    text: String,
    pending: Option<SendAttempt>,
) -> Result<Draft> {
    ensure!(
        conversation.len() <= 128 && !conversation.is_empty(),
        "Invalid conversation"
    );
    ensure!(text.len() <= MAX_MESSAGE_BYTES, "Draft is too large");
    if let Some(attempt) = &pending {
        ensure!(
            attempt.conversation == conversation
                && attempt.text.len() <= MAX_MESSAGE_BYTES
                && !attempt.id.is_empty()
                && attempt.id.len() <= 128
                && attempt.timestamp <= i64::MAX as u64,
            "Invalid pending send"
        );
    }
    let version = expected
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| anyhow::anyhow!("Draft version exhausted"))?;
    let next = Draft {
        version,
        text,
        pending,
    };
    let body = serde_json::to_string(&next)?;
    let mut transaction = database.begin().await?;
    let prior: Option<(String, String)> =
        sqlx::query_as("SELECT command, body FROM loom_drafts_v1 WHERE conversation = ?")
            .bind(conversation)
            .fetch_optional(&mut *transaction)
            .await?;
    if let Some((command, saved)) = &prior {
        if command == request {
            ensure!(saved == &body, "Draft command reused with different input");
            return Ok(next);
        }
        ensure!(
            serde_json::from_str::<Draft>(saved)?.version == expected,
            "Draft changed in another editor"
        );
    } else {
        ensure!(expected == 0, "Draft version is missing");
    }
    let (count, bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(length(CAST(body AS BLOB))), 0) FROM loom_drafts_v1",
    )
    .fetch_one(&mut *transaction)
    .await?;
    ensure!(prior.is_some() || count < 2000, "Draft storage is full");
    let previous_bytes = prior.as_ref().map_or(0, |(_, body)| body.len());
    ensure!(
        bytes as usize - previous_bytes + body.len() <= 8 * 1024 * 1024,
        "Draft storage is full"
    );
    sqlx::query("INSERT INTO loom_drafts_v1(conversation, command, body) VALUES (?, ?, ?) ON CONFLICT(conversation) DO UPDATE SET command = excluded.command, body = excluded.body")
        .bind(conversation).bind(request).bind(body).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lost_reply_retries_exactly_and_stale_editors_cannot_replace_prose() {
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE loom_drafts_v1(conversation TEXT PRIMARY KEY, command TEXT NOT NULL, body TEXT NOT NULL)").execute(&database).await.unwrap();
        let first = save(&database, "friend", "one", 0, "  hello 🌻\r\n".into(), None)
            .await
            .unwrap();
        assert_eq!(
            save(&database, "friend", "one", 0, first.text.clone(), None)
                .await
                .unwrap(),
            first
        );
        assert!(
            save(&database, "friend", "one", 0, "other".into(), None)
                .await
                .is_err()
        );
        assert!(
            save(&database, "friend", "old", 0, "stale".into(), None)
                .await
                .is_err()
        );
        assert_eq!(load(&database, "friend").await.unwrap(), first);
        database.close().await;
    }
}
