//! Local conversation bookmarks live with the encrypted account. They contain
//! public cabal IDs, never admission tokens, group keys, or filesystem paths.
use anyhow::{Result, ensure};
use loom_signal_protocol::{Workspace, WorkspaceLinks};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

pub async fn initialize(database: &SqlitePool) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS loom_workspaces_v1(conversation TEXT PRIMARY KEY, command TEXT NOT NULL, fingerprint TEXT NOT NULL, body TEXT NOT NULL)")
        .execute(database).await?;
    Ok(())
}

pub async fn load(database: &SqlitePool, conversation: &str) -> Result<WorkspaceLinks> {
    let body: Option<String> =
        sqlx::query_scalar("SELECT body FROM loom_workspaces_v1 WHERE conversation = ?")
            .bind(conversation)
            .fetch_optional(database)
            .await?;
    body.map_or_else(
        || Ok(WorkspaceLinks::default()),
        |body| Ok(serde_json::from_str(&body)?),
    )
}

pub async fn update(
    database: &SqlitePool,
    conversation: &str,
    request: &str,
    expected: u64,
    workspace: Workspace,
    remove: bool,
) -> Result<WorkspaceLinks> {
    ensure!(
        !conversation.is_empty() && conversation.len() <= 128,
        "Invalid conversation"
    );
    ensure!(!workspace.id.is_nil(), "Invalid workspace");
    ensure!(
        remove
            || (!workspace.title.trim().is_empty()
                && workspace.title.len() <= 512
                && !workspace.title.chars().any(char::is_control)),
        "Invalid workspace title"
    );
    let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&(
        expected, &workspace, remove,
    ))?));
    let mut transaction = database.begin().await?;
    let prior: Option<(String, String, String)> = sqlx::query_as(
        "SELECT command, fingerprint, body FROM loom_workspaces_v1 WHERE conversation = ?",
    )
    .bind(conversation)
    .fetch_optional(&mut *transaction)
    .await?;
    let mut links = match &prior {
        Some((command, saved_fingerprint, body)) => {
            let links: WorkspaceLinks = serde_json::from_str(body)?;
            if command == request {
                ensure!(
                    *saved_fingerprint == fingerprint,
                    "Workspace command reused with different input"
                );
                return Ok(links);
            }
            links
        }
        None => WorkspaceLinks::default(),
    };
    ensure!(
        links.version == expected,
        "Workspace links changed. Refresh this conversation first."
    );
    links.version = expected
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| anyhow::anyhow!("Workspace version exhausted"))?;
    links.workspaces.retain(|item| item.id != workspace.id);
    if !remove {
        links.workspaces.push(workspace);
    }
    links.workspaces.sort_by_key(|item| item.id);
    ensure!(
        links.workspaces.len() <= 16,
        "This conversation has 16 workspaces already"
    );
    let body = serde_json::to_string(&links)?;
    let (count, bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(length(CAST(body AS BLOB))), 0) FROM loom_workspaces_v1",
    )
    .fetch_one(&mut *transaction)
    .await?;
    ensure!(
        prior.is_some() || count < 2000,
        "Workspace bookmark storage is full"
    );
    ensure!(
        bytes as usize - prior.as_ref().map_or(0, |(_, _, body)| body.len()) + body.len()
            <= 8 * 1024 * 1024,
        "Workspace bookmark storage is full"
    );
    sqlx::query("INSERT INTO loom_workspaces_v1(conversation, command, fingerprint, body) VALUES (?, ?, ?, ?) ON CONFLICT(conversation) DO UPDATE SET command = excluded.command, fingerprint = excluded.fingerprint, body = excluded.body")
        .bind(conversation).bind(request).bind(fingerprint).bind(body).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        serde_json::from_str(
            r#"{"id":"5c5b144e-1619-476a-bf58-e624d1f402cc","title":"The garden"}"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn bookmarks_survive_reopen_and_stale_retries_cannot_restore_removed_links() {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("encrypted.db"))
            .create_if_missing(true)
            .pragma("key", "'test-key'");
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        initialize(&database).await.unwrap();
        let linked = update(&database, "alice", "one", 0, workspace(), false)
            .await
            .unwrap();
        assert_eq!(
            linked,
            update(&database, "alice", "one", 0, workspace(), false)
                .await
                .unwrap()
        );
        assert!(
            update(&database, "alice", "one", 0, workspace(), true)
                .await
                .is_err()
        );
        assert_eq!(
            load(&database, "bob").await.unwrap(),
            WorkspaceLinks::default()
        );
        database.close().await;
        let bytes = std::fs::read(directory.path().join("encrypted.db")).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        initialize(&database).await.unwrap();
        assert_eq!(load(&database, "alice").await.unwrap(), linked);
        let removed = update(&database, "alice", "two", 1, workspace(), true)
            .await
            .unwrap();
        assert!(removed.workspaces.is_empty());
        assert!(
            update(&database, "alice", "one", 0, workspace(), false)
                .await
                .is_err()
        );
        assert_eq!(load(&database, "alice").await.unwrap(), removed);
        database.close().await;
    }

    #[tokio::test]
    async fn bounded_bookmarks_do_not_replace_existing_links_on_failure() {
        let database = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        initialize(&database).await.unwrap();
        let mut expected = WorkspaceLinks::default();
        for index in 0..16 {
            let mut item = workspace();
            item.id = format!("00000000-0000-0000-0000-{:012x}", index + 1)
                .parse()
                .unwrap();
            expected = update(&database, "alice", &index.to_string(), index, item, false)
                .await
                .unwrap();
        }
        assert!(
            update(&database, "alice", "full", 16, workspace(), false)
                .await
                .is_err()
        );
        let mut item = workspace();
        item.title = "x".repeat(513);
        assert!(
            update(&database, "alice", "oversize", 16, item, false)
                .await
                .is_err()
        );
        assert_eq!(load(&database, "alice").await.unwrap(), expected);
        database.close().await;
    }
}
