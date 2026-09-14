//! Bounded, typed messages between Loom and its isolated Signal client.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 3;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_PAGE_SIZE: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    Link {
        device_name: String,
    },
    CancelLink,
    Conversations,
    Identity {
        conversation_id: String,
        recipient_id: Option<String>,
        refresh: bool,
    },
    VerifyIdentity {
        conversation_id: String,
        recipient_id: String,
        review_id: String,
    },
    Workspaces {
        conversation_id: String,
    },
    UpdateWorkspace {
        conversation_id: String,
        expected_version: u64,
        workspace_id: uuid::Uuid,
        title: Option<String>,
    },
    Messages {
        conversation_id: String,
        before: Option<u64>,
        limit: usize,
    },
    Draft {
        conversation_id: String,
    },
    SaveDraft {
        conversation_id: String,
        expected_version: u64,
        text: String,
        pending: Option<SendAttempt>,
    },
    CheckSend {
        attempt: SendAttempt,
    },
    Send {
        conversation_id: String,
        text: String,
        timestamp: u64,
    },
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: Option<String>,
    pub event: Event,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Status {
        status: Status,
    },
    Link {
        url: String,
        qr_code: String,
    },
    Conversations {
        conversations: Vec<Conversation>,
    },
    Identity {
        conversation_id: String,
        members: Vec<IdentityMember>,
        review: Option<IdentityReview>,
    },
    Workspaces {
        conversation_id: String,
        links: WorkspaceLinks,
    },
    Messages {
        conversation_id: String,
        messages: Vec<Message>,
    },
    Draft {
        conversation_id: String,
        draft: Draft,
    },
    NotSent,
    Sent {
        conversation_id: String,
        timestamp: u64,
    },
    Changed {
        conversation_id: Option<String>,
    },
    Failure {
        code: String,
        message: String,
        retryable: bool,
    },
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub version: u32,
    pub phase: Phase,
    pub account_id: Option<String>,
    pub device_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Unlinked,
    Linking,
    Connecting,
    Connected,
    Offline,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub is_group: bool,
    pub disappearing: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMember {
    pub id: String,
    pub title: String,
}

/// Public comparison material only. Private keys and protocol sessions never
/// leave the isolated worker. A review ID authorizes only the displayed pair.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityReview {
    pub recipient_id: String,
    pub review_id: String,
    pub safety_number: String,
    pub qr_code: String,
    pub state: IdentityState,
    pub refreshed_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    Unverified,
    Verified,
    Changed,
    Pending,
}

/// A local bookmark, never an invitation or a filesystem capability.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: uuid::Uuid,
    pub title: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLinks {
    pub version: u64,
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub timestamp: u64,
    pub sender_id: String,
    pub sender_name: String,
    pub outgoing: bool,
    pub text: String,
    pub edited: bool,
    pub deleted: bool,
    pub ephemeral: bool,
    pub attachment_count: usize,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub version: u64,
    pub text: String,
    pub pending: Option<SendAttempt>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SendAttempt {
    pub id: String,
    pub conversation: String,
    pub text: String,
    pub timestamp: u64,
}

/// Length-delimited JSON. Never allocate according to an unchecked peer length.
pub async fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<Option<T>> {
    use tokio::io::AsyncReadExt;
    let mut prefix = [0_u8; 4];
    if reader.read(&mut prefix[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid Signal frame length",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Signal frame"))
}

pub async fn write_frame<T: Serialize>(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Signal frame exceeds limit",
        ));
    }
    let length = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}
