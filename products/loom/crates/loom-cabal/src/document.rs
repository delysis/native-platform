use automerge::{
    ActorId, Automerge, Change, ChangeHash, ObjType, ROOT, ReadDoc, transaction::Transactable,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Identity, MAX_DOCUMENT_BYTES, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextKind {
    Prose,
    Verse,
}

impl TextKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Verse => "verse",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentView {
    pub id: Uuid,
    pub name: String,
    pub kind: TextKind,
    pub deleted: bool,
    pub text: String,
    pub heads: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub document: Uuid,
    pub client: Uuid,
    pub basis: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataEdit {
    pub document: Uuid,
    pub client: Uuid,
    pub basis: Vec<String>,
    pub name: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub document: Uuid,
    pub client: Uuid,
    pub name: String,
    pub kind: TextKind,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditResult {
    /// Heads of the submitted edit, before unseen remote changes were merged.
    /// A typing client with newer input uses these as its next basis.
    pub local_heads: Vec<String>,
    pub merged: DocumentView,
}

pub(crate) fn initial(
    identity: &Identity,
    client: Uuid,
    name: &str,
    kind: TextKind,
    text: &str,
) -> Result<(Automerge, Change)> {
    check_text(text)?;
    check_name(name)?;
    let mut document = Automerge::new();
    document.set_actor(actor(identity, client));
    let mut transaction = document.transaction();
    transaction.put(ROOT, "name", name)?;
    transaction.put(ROOT, "kind", kind.as_str())?;
    transaction.put(ROOT, "deleted", false)?;
    let body = transaction.put_object(ROOT, "text", ObjType::Text)?;
    transaction.splice_text(&body, 0, 0, text)?;
    transaction.commit();
    let change = document
        .get_last_local_change()
        .ok_or(Error::Invalid("Document creation produced no change"))?
        .clone();
    Ok((document, change))
}

pub(crate) fn edit(
    document: &Automerge,
    identity: &Identity,
    edit: &Edit,
) -> Result<(Automerge, Option<Change>)> {
    check_text(&edit.text)?;
    let heads = decode_heads(&edit.basis)?;
    if heads.is_empty() {
        return Err(Error::Invalid("An edit needs its document basis"));
    }
    let mut local = document.fork_at(&heads)?;
    local.set_actor(actor(identity, edit.client));
    let (_, text) = local
        .get(ROOT, "text")?
        .ok_or(Error::Invalid("Missing document text"))?;
    let mut transaction = local.transaction();
    transaction.update_text(&text, &edit.text)?;
    let (change, _) = transaction.commit();
    let change = change.and_then(|hash| local.get_change_by_hash(&hash));
    Ok((local, change))
}

pub(crate) fn edit_metadata(
    document: &Automerge,
    identity: &Identity,
    edit: &MetadataEdit,
) -> Result<(Automerge, Option<Change>)> {
    check_name(&edit.name)?;
    let heads = decode_heads(&edit.basis)?;
    if heads.is_empty() {
        return Err(Error::Invalid("A document action needs its basis"));
    }
    let mut local = document.fork_at(&heads)?;
    local.set_actor(actor(identity, edit.client));
    let old = view(&local, edit.document)?;
    let mut transaction = local.transaction();
    // Only fields the caller changed are written. A deletion from an older
    // snapshot must not undo a concurrent rename it has never seen.
    if old.name != edit.name {
        transaction.put(ROOT, "name", edit.name.as_str())?;
    }
    if old.deleted != edit.deleted {
        transaction.put(ROOT, "deleted", edit.deleted)?;
    }
    let (change, _) = transaction.commit();
    let change = change.and_then(|hash| local.get_change_by_hash(&hash));
    Ok((local, change))
}

pub(crate) fn view(document: &Automerge, id: Uuid) -> Result<DocumentView> {
    if document.keys(ROOT).collect::<Vec<_>>() != ["deleted", "kind", "name", "text"] {
        return Err(Error::Invalid("Invalid shared document shape"));
    }
    let values = document.get_all(ROOT, "text")?;
    if values.len() != 1 || values[0].0 != automerge::Value::Object(ObjType::Text) {
        return Err(Error::Invalid("Invalid shared text object"));
    }
    let text = document.text(&values[0].1)?;
    check_text(&text)?;
    // Concurrent scalar assignments use Automerge's deterministic winner.
    // Validate every conflicting value as well as the selected one.
    for (value, _) in document.get_all(ROOT, "name")? {
        check_name(
            value
                .to_str()
                .ok_or(Error::Invalid("Invalid document name"))?,
        )?;
    }
    for (value, _) in document.get_all(ROOT, "kind")? {
        if !matches!(value.to_str(), Some("prose" | "verse")) {
            return Err(Error::Invalid("Invalid shared document kind"));
        }
    }
    for (value, _) in document.get_all(ROOT, "deleted")? {
        if value.to_bool().is_none() {
            return Err(Error::Invalid("Invalid shared document deletion"));
        }
    }
    let name = document
        .get(ROOT, "name")?
        .and_then(|(value, _)| value.to_str().map(str::to_owned))
        .ok_or(Error::Invalid("Missing document name"))?;
    let kind = match document
        .get(ROOT, "kind")?
        .and_then(|(value, _)| value.to_str().map(str::to_owned))
        .as_deref()
    {
        Some("prose") => TextKind::Prose,
        Some("verse") => TextKind::Verse,
        _ => return Err(Error::Invalid("Missing document kind")),
    };
    let deleted = document
        .get(ROOT, "deleted")?
        .and_then(|(value, _)| value.to_bool())
        .ok_or(Error::Invalid("Missing document deletion"))?;
    Ok(DocumentView {
        id,
        name,
        kind,
        deleted,
        text,
        heads: encode_heads(document),
    })
}

/// A shared name must be an ordinary portable workspace path before it enters
/// the durable log, so one admitted bad path cannot poison every later sync.
pub(crate) fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
        return Err(Error::Invalid("Invalid shared document path"));
    }
    for (index, part) in name.split('/').enumerate() {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
            });
        if part.is_empty()
            || part.starts_with('.')
            || part.ends_with(['.', ' '])
            || part.len() > 255
            || part.contains(['\\', '<', '>', ':', '"', '|', '?', '*'])
            || reserved
            || (index == 0
                && (part.eq_ignore_ascii_case("Runs") || part.eq_ignore_ascii_case("Recovery")))
        {
            return Err(Error::Invalid(
                "A shared document must name an ordinary workspace file",
            ));
        }
    }
    Ok(())
}

pub(crate) fn encode_heads(document: &Automerge) -> Vec<String> {
    document
        .get_heads()
        .into_iter()
        .map(|hash| hash.to_string())
        .collect()
}

fn decode_heads(heads: &[String]) -> Result<Vec<ChangeHash>> {
    if heads.len() > 256 {
        return Err(Error::Invalid("Too many document heads"));
    }
    heads
        .iter()
        .map(|head| {
            head.parse()
                .map_err(|_| Error::Invalid("Invalid document head"))
        })
        .collect()
}

fn actor(identity: &Identity, client: Uuid) -> ActorId {
    let mut bytes = identity.public_key().as_bytes().to_vec();
    bytes.extend_from_slice(client.as_bytes());
    ActorId::from(bytes)
}

fn check_text(text: &str) -> Result<()> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::Invalid("Shared document exceeds one MiB"));
    }
    Ok(())
}
