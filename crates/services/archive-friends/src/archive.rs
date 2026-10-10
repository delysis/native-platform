//! Read-only canonical snapshot adapter. No ingest, migrations or write API.
use crate::FriendsError;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Tweet,
    CommunityTweet,
    NoteTweet,
    LikedTweet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub message_id: String,
    pub tweet_id: Option<String>,
    pub message_type: MessageType,
    pub source_collection: String,
    pub archive_owner_account_id: Option<String>,
    pub account_id: Option<String>,
    pub username: Option<String>,
    pub account_display_name: Option<String>,
    pub created_at: Option<String>,
    pub text: String,
    pub lang: Option<String>,
    pub conversation_id: Option<String>,
    pub thread_id: Option<String>,
    pub depth: Option<i64>,
    pub reply_to_tweet_id: Option<String>,
    pub reply_to_user_id: Option<String>,
    pub reply_to_username: Option<String>,
    pub quoted_tweet_id: Option<String>,
    pub retweeted_tweet_id: Option<String>,
    pub liked_by_account_id: Option<String>,
    pub community_id: Option<String>,
    pub source_url: Option<String>,
    pub is_reply: bool,
    pub is_quote: bool,
    pub is_retweet: bool,
    pub favorite_count: Option<i64>,
    pub retweet_count: Option<i64>,
    pub archive_path: String,
    pub raw_json: Value,
}

#[derive(Debug)]
pub(crate) struct ArchiveDatabase {
    conn: Connection,
}
impl ArchiveDatabase {
    pub(crate) fn open_immutable_read_only(path: &Path) -> Result<Self, FriendsError> {
        let canonical = std::fs::canonicalize(path)?;
        let mut uri = url::Url::from_file_path(&canonical)
            .map_err(|()| crate::invalid("archive path cannot be represented as a file URI"))?;
        uri.query_pairs_mut()
            .append_pair("mode", "ro")
            .append_pair("immutable", "1");
        let conn = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")?;
        Ok(Self { conn })
    }
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }
    pub(crate) fn message_by_rowid(&self, rowid: i64) -> Result<Option<Message>, FriendsError> {
        self.conn
            .query_row(
                "SELECT * FROM messages_compat WHERE rowid = ?",
                [rowid],
                row_to_message,
            )
            .optional()
            .map_err(FriendsError::from)
    }
}

pub(crate) fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let message_type: String = row.get("message_type")?;
    Ok(Message {
        message_id: row.get("message_id")?,
        tweet_id: row.get("tweet_id")?,
        message_type: match message_type.as_str() {
            "tweet" => MessageType::Tweet,
            "community_tweet" => MessageType::CommunityTweet,
            "note_tweet" => MessageType::NoteTweet,
            "liked_tweet" => MessageType::LikedTweet,
            _ => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    row.as_ref().column_index("message_type")?,
                    rusqlite::types::Type::Text,
                    Box::new(crate::invalid("unsupported archive message type")),
                ));
            }
        },
        source_collection: row.get("source_collection")?,
        archive_owner_account_id: row.get("archive_owner_account_id")?,
        account_id: row.get("account_id")?,
        username: row.get("username")?,
        account_display_name: row.get("account_display_name")?,
        created_at: row.get("created_at")?,
        text: row.get("text")?,
        lang: row.get("lang")?,
        conversation_id: row.get("conversation_id")?,
        thread_id: row.get("thread_id")?,
        depth: row.get("depth")?,
        reply_to_tweet_id: row.get("reply_to_tweet_id")?,
        reply_to_user_id: row.get("reply_to_user_id")?,
        reply_to_username: row.get("reply_to_username")?,
        quoted_tweet_id: row.get("quoted_tweet_id")?,
        retweeted_tweet_id: row.get("retweeted_tweet_id")?,
        liked_by_account_id: row.get("liked_by_account_id")?,
        community_id: row.get("community_id")?,
        source_url: row.get("source_url")?,
        is_reply: row.get::<_, i64>("is_reply")? != 0,
        is_quote: row.get::<_, i64>("is_quote")? != 0,
        is_retweet: row.get::<_, i64>("is_retweet")? != 0,
        favorite_count: row.get("favorite_count")?,
        retweet_count: row.get("retweet_count")?,
        archive_path: row
            .get::<_, Option<String>>("archive_path")?
            .unwrap_or_default(),
        raw_json: Value::Null,
    })
}
