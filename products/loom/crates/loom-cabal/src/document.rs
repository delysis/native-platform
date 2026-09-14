use automerge::{
    ActorId, Automerge, Change, ChangeHash, ObjType, ROOT, ReadDoc, transaction::Transactable,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Identity, MAX_DOCUMENT_BYTES, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentView {
    pub id: Uuid,
    pub name: String,
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
pub struct EditResult {
    /// Heads of the submitted edit, before unseen remote changes were merged.
    /// A typing client with newer input uses these as its next basis.
    pub local_heads: Vec<String>,
    pub merged: DocumentView,
}

pub(crate) fn initial(
    identity: &Identity,
    client: Uuid,
    text: &str,
) -> Result<(Automerge, Change)> {
    check_text(text)?;
    let mut document = Automerge::new();
    document.set_actor(actor(identity, client));
    let mut transaction = document.transaction();
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

pub(crate) fn view(document: &Automerge, id: Uuid, name: String) -> Result<DocumentView> {
    if document.keys(ROOT).collect::<Vec<_>>() != ["text"] {
        return Err(Error::Invalid("Invalid shared document shape"));
    }
    let values = document.get_all(ROOT, "text")?;
    if values.len() != 1 || values[0].0 != automerge::Value::Object(ObjType::Text) {
        return Err(Error::Invalid("Invalid shared text object"));
    }
    let text = document.text(&values[0].1)?;
    check_text(&text)?;
    Ok(DocumentView {
        id,
        name,
        text,
        heads: encode_heads(document),
    })
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
