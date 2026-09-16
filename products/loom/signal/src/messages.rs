//! Bounded projections of Signal records. Group keys never cross the worker boundary.
use anyhow::{Result, bail};
use loom_signal_protocol::{Conversation, MAX_MESSAGE_BYTES, MAX_PAGE_SIZE, Message};
use presage::{
    libsignal_service::{
        content::ContentBody,
        protocol::{Aci, ServiceId},
    },
    proto::{DataMessage, sync_message},
    store::{ContentsStore, StateStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

pub const MAX_CONVERSATIONS: usize = 2000;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

pub fn id(thread: &Thread) -> String {
    match thread {
        Thread::Contact(id) => format!("contact:{}", id.service_id_string()),
        Thread::Group(key) => format!("group:{}", hex::encode(Sha256::digest(key))),
    }
}

pub async fn conversations(store: &SqliteStore) -> Result<Vec<(Conversation, Thread)>> {
    let mut result = Vec::new();
    let own = store
        .load_registration_data()
        .await?
        .map(|data| ServiceId::from(data.service_ids.aci()));
    if let Some(account) = own {
        let thread = Thread::Contact(account);
        let timer = store
            .expire_timer(&thread)
            .await?
            .map_or(0, |(timer, _)| timer);
        result.push((
            Conversation {
                id: id(&thread),
                title: "Note to Self".into(),
                is_group: false,
                disappearing: timer > 0,
                description: None,
            },
            thread,
        ));
    }
    for contact in store.contacts().await?.take(MAX_CONVERSATIONS + 1) {
        let contact = contact?;
        let thread = Thread::Contact(Aci::from(contact.uuid).into());
        if own == Some(Aci::from(contact.uuid).into()) {
            continue;
        }
        let title = if contact.name.is_empty() {
            contact.uuid.to_string()
        } else {
            clipped(&contact.name, 512)
        };
        result.push((
            Conversation {
                id: id(&thread),
                title,
                is_group: false,
                disappearing: contact.expire_timer > 0,
                description: None,
            },
            thread,
        ));
    }
    for group in store.groups().await?.take(MAX_CONVERSATIONS + 1) {
        let (key, group) = group?;
        let thread = Thread::Group(key);
        result.push((
            Conversation {
                id: id(&thread),
                title: clipped(&group.title, 512),
                is_group: true,
                disappearing: group
                    .disappearing_messages_timer
                    .is_some_and(|timer| timer.duration > 0),
                description: group
                    .description
                    .as_deref()
                    .map(|value| clipped(value, 4096)),
            },
            thread,
        ));
    }
    anyhow::ensure!(
        result.len() <= MAX_CONVERSATIONS,
        "Signal conversation limit exceeded"
    );
    result.sort_by(|a, b| {
        a.0.title
            .to_lowercase()
            .cmp(&b.0.title.to_lowercase())
            .then(a.0.id.cmp(&b.0.id))
    });
    Ok(result)
}

pub async fn resolve(store: &SqliteStore, conversation: &str) -> Result<Thread> {
    conversations(store)
        .await?
        .into_iter()
        .find(|(item, _)| item.id == conversation)
        .map(|(_, thread)| thread)
        .ok_or_else(|| anyhow::anyhow!("Unknown Signal conversation"))
}

pub fn data(body: &ContentBody) -> Option<&DataMessage> {
    match body {
        ContentBody::DataMessage(message) => Some(message),
        ContentBody::EditMessage(message) => message.data_message.as_ref(),
        ContentBody::SynchronizeMessage(message) => match &message.content {
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

pub async fn page(
    store: &SqliteStore,
    database: &SqlitePool,
    thread: &Thread,
    account: ServiceId,
    before: Option<u64>,
    limit: usize,
) -> Result<Vec<Message>> {
    if limit == 0 || limit > MAX_PAGE_SIZE {
        bail!("Invalid Signal page size");
    }
    let (group, contact) = match thread {
        Thread::Group(key) => (Some(key.to_vec()), None),
        Thread::Contact(id) => (None, Some(id.raw_uuid().as_bytes().to_vec())),
    };
    let before = i64::try_from(before.unwrap_or(i64::MAX as u64))?;
    // Presage's messages() materializes the full range. Select a bounded set of
    // timestamps here, then let Presage decode each exact record.
    crate::retention::sweep(database).await?;
    let rows = sqlx::query("SELECT m.ts, r.expires_at FROM thread_messages m JOIN loom_retention_v1 r ON r.ts = m.ts AND r.thread_id = m.thread_id WHERE m.thread_id = (SELECT id FROM threads WHERE group_master_key = ? OR recipient_id = ?) AND m.ts < ? ORDER BY m.ts DESC LIMIT ?")
        .bind(group).bind(contact).bind(before).bind(limit as i64).fetch_all(database).await?;
    let mut messages = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let timestamp = u64::try_from(row.try_get::<i64, _>("ts")?)?;
        let Some(content) = store.message(thread, timestamp).await? else {
            continue;
        };
        let expiry = row
            .try_get::<Option<i64>, _>("expires_at")?
            .map(|value| value as u64);
        if expiry.is_some_and(|value| value <= now()) {
            store.clone().delete_message(thread, timestamp).await?;
            continue;
        }
        let Some(body) = data(&content.body) else {
            continue;
        };
        // Group updates, reactions, and timer changes are protocol events,
        // not empty messages to display or feed into an AI reply draft.
        if body.body.as_deref().is_none_or(str::is_empty)
            && body.attachments.is_empty()
            && body.delete.is_none()
        {
            continue;
        }
        let text = clipped(body.body.as_deref().unwrap_or_default(), MAX_MESSAGE_BYTES);
        bytes += text.len();
        if bytes > 1024 * 1024 {
            break;
        }
        let sender = content.metadata.sender;
        let sender_name = store
            .contact_by_id(&sender)
            .await?
            .map(|contact| contact.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| sender.service_id_string());
        messages.push(Message {
            id: format!("{}:{timestamp}:{}", id(thread), sender.service_id_string()),
            timestamp,
            sender_id: sender.service_id_string(),
            sender_name: clipped(&sender_name, 512),
            outgoing: sender == account,
            text,
            edited: matches!(content.body, ContentBody::EditMessage(_)),
            deleted: body.delete.is_some(),
            ephemeral: expiry.is_some(),
            expires_at: expiry,
            attachment_count: body.attachments.len(),
        });
    }
    messages.reverse();
    Ok(messages)
}

fn clipped(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
