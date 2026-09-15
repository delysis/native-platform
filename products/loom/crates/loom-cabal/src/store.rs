mod assets;
mod invitations;
pub use assets::{ASSET_CHUNK_BYTES, AssetDescriptor, MAX_ASSET_BYTES};
pub use invitations::Invitation;

use automerge::{Automerge, Change, ReadDoc};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::{EndpointAddr, PublicKey};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use uuid::Uuid;

use crate::{
    Error, Identity, MAX_CHANGE_BYTES, MAX_CHANGES, MAX_DOCUMENTS, MAX_MEMBERS, Result, Signed,
    document::{self, Create, DocumentView, Edit, EditResult, MetadataEdit, TextKind},
};

const STORE_VERSION: i64 = 4;
const MAX_STORED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub key: PublicKey,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub schema: u32,
    pub cabal: Uuid,
    pub owner: PublicKey,
    pub name: String,
    pub revision: u64,
    pub epoch: u64,
    pub members: Vec<Member>,
    /// At revocation, the owner seals accepted history. Unknown changes from
    /// an earlier epoch cannot be backdated by a removed device.
    pub sealed: BTreeSet<String>,
}

pub type Roster = Signed<Membership>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePayload {
    pub schema: u32,
    pub cabal: Uuid,
    pub epoch: u64,
    pub document: Uuid,
    pub change: String,
}

pub type ChangeEnvelope = Signed<ChangePayload>;

pub struct Cabal {
    identity: Identity,
    database: Connection,
    roster: Roster,
    documents: BTreeMap<Uuid, Automerge>,
    stored_bytes: usize,
}

impl std::fmt::Debug for Cabal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cabal")
            .field("id", &self.id())
            .field("identity", &self.identity)
            .field("documents", &self.documents.len())
            .finish()
    }
}

impl Cabal {
    pub fn create(path: &Path, identity: Identity, name: &str, member_name: &str) -> Result<Self> {
        validate_name(name)?;
        validate_name(member_name)?;
        let roster = identity.sign(Membership {
            schema: 1,
            cabal: Uuid::new_v4(),
            owner: identity.public_key(),
            name: name.into(),
            revision: 0,
            epoch: 0,
            members: vec![Member {
                key: identity.public_key(),
                name: member_name.into(),
            }],
            sealed: BTreeSet::new(),
        })?;
        Self::import(path, identity, roster)
    }

    pub fn import(path: &Path, identity: Identity, roster: Roster) -> Result<Self> {
        validate_roster(&roster)?;
        if path.exists() {
            return Err(Error::Invalid("Cabal storage already exists"));
        }
        let database = open_database(path)?;
        database.execute(
            "INSERT INTO metadata(key, value) VALUES ('roster', ?)",
            [serde_json::to_string(&roster)?],
        )?;
        Ok(Self {
            identity,
            database,
            roster,
            documents: BTreeMap::new(),
            stored_bytes: 0,
        })
    }

    pub fn open(path: &Path, identity: Identity) -> Result<Self> {
        if !path.exists() {
            return Err(Error::Invalid("Cabal storage does not exist"));
        }
        let database = open_database(path)?;
        let encoded: String = database.query_row(
            "SELECT value FROM metadata WHERE key = 'roster'",
            [],
            |row| row.get(0),
        )?;
        let roster: Roster = serde_json::from_str(&encoded)?;
        validate_roster(&roster)?;
        let stored_bytes: i64 = database.query_row(
            "SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM changes",
            [],
            |row| row.get(0),
        )?;
        let stored_bytes = usize::try_from(stored_bytes)
            .map_err(|_| Error::Invalid("Cabal storage exceeds limit"))?;
        if stored_bytes > MAX_STORED_BYTES {
            return Err(Error::Invalid("Cabal storage exceeds limit"));
        }
        let mut cabal = Self {
            identity,
            database,
            roster,
            documents: BTreeMap::new(),
            stored_bytes,
        };
        let envelopes = cabal.envelopes()?;
        for envelope in &envelopes {
            validate_envelope(envelope, &cabal.roster)?;
        }
        cabal.documents = build_documents(&envelopes)?;
        Ok(cabal)
    }

    pub fn id(&self) -> Uuid {
        self.roster.payload.cabal
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn roster(&self) -> &Roster {
        &self.roster
    }
    pub fn is_member(&self, key: PublicKey) -> bool {
        self.roster
            .payload
            .members
            .iter()
            .any(|member| member.key == key)
    }

    pub fn remember_peer(&self, address: &EndpointAddr) -> Result<()> {
        if !self.is_member(address.id) {
            return Err(Error::Invalid("Unknown cabal peer"));
        }
        self.database.execute("INSERT INTO metadata(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("peer:{}", address.id), serde_json::to_string(address)?])?;
        Ok(())
    }

    pub fn peer_address(&self, key: PublicKey) -> Result<EndpointAddr> {
        use rusqlite::OptionalExtension;
        let value: Option<String> = self
            .database
            .query_row(
                "SELECT value FROM metadata WHERE key = ?",
                [format!("peer:{key}")],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(EndpointAddr::from(key)))
    }

    pub fn views(&self) -> Result<Vec<DocumentView>> {
        self.documents
            .iter()
            .filter(|(_, document)| document.get_missing_deps(&[]).is_empty())
            .map(|(id, value)| document::view(value, *id))
            .collect()
    }

    pub fn view(&self, id: Uuid) -> Result<DocumentView> {
        let document = self
            .documents
            .get(&id)
            .ok_or(Error::Invalid("Unknown shared document"))?;
        document::view(document, id)
    }

    /// Read exactly the caller's observed basis before deciding which media
    /// references the caller introduced, excluding unseen remote references.
    pub fn view_at(&self, id: Uuid, heads: &[String]) -> Result<DocumentView> {
        let document = self
            .documents
            .get(&id)
            .ok_or(Error::Invalid("Unknown shared document"))?;
        let heads = document::decode_heads(heads)?;
        if heads.is_empty() {
            return Err(Error::Invalid("An edit needs its document basis"));
        }
        document::view(&document.fork_at(&heads)?, id)
    }

    pub fn create_document(&mut self, name: &str, text: &str) -> Result<DocumentView> {
        self.create_document_with_kind(name, text, TextKind::Prose)
    }

    pub fn create_document_with_kind(
        &mut self,
        name: &str,
        text: &str,
        kind: TextKind,
    ) -> Result<DocumentView> {
        Ok(self
            .create_document_idempotent(&Create {
                document: Uuid::new_v4(),
                client: Uuid::new_v4(),
                name: name.into(),
                kind,
                text: text.into(),
            })?
            .merged)
    }

    pub fn create_document_idempotent(&mut self, create: &Create) -> Result<EditResult> {
        self.require_member()?;
        document::check_name(&create.name)?;
        let (local, change) = document::initial(
            &self.identity,
            create.client,
            &create.name,
            create.kind,
            &create.text,
        )?;
        let local_heads = document::encode_heads(&local);
        if self
            .documents
            .get(&create.document)
            .is_some_and(|document| document.get_change_by_hash(&change.hash()).is_none())
        {
            return Err(Error::Invalid("Document creation identity was reused"));
        }
        self.apply_local_change(create.document, change)?;
        Ok(EditResult {
            local_heads,
            merged: self.view(create.document)?,
        })
    }

    pub fn edit_metadata(&mut self, edit: &MetadataEdit) -> Result<EditResult> {
        self.require_member()?;
        let document = self
            .documents
            .get(&edit.document)
            .ok_or(Error::Invalid("Unknown shared document"))?;
        let (local, change) = document::edit_metadata(document, &self.identity, edit)?;
        let local_heads = document::encode_heads(&local);
        if let Some(change) = change {
            self.apply_local_change(edit.document, change)?;
        }
        Ok(EditResult {
            local_heads,
            merged: self.view(edit.document)?,
        })
    }

    pub fn edit(&mut self, edit: &Edit) -> Result<EditResult> {
        self.require_member()?;
        let document = self
            .documents
            .get(&edit.document)
            .ok_or(Error::Invalid("Unknown shared document"))?;
        let (local, change) = document::edit(document, &self.identity, edit)?;
        let local_heads = document::encode_heads(&local);
        if let Some(change) = change {
            self.apply_local_change(edit.document, change)?;
        }
        Ok(EditResult {
            local_heads,
            merged: self.view(edit.document)?,
        })
    }

    fn apply_local_change(&mut self, id: Uuid, change: Change) -> Result<()> {
        // A retry after membership advances must not create another envelope
        // for the same causal change under a new epoch.
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.get_change_by_hash(&change.hash()).is_some())
        {
            return Ok(());
        }
        let envelope = self.envelope(id, change)?;
        self.apply(vec![envelope])?;
        Ok(())
    }

    fn envelope(&self, id: Uuid, change: Change) -> Result<ChangeEnvelope> {
        self.identity.sign(ChangePayload {
            schema: 2,
            cabal: self.id(),
            epoch: self.roster.payload.epoch,
            document: id,
            change: URL_SAFE_NO_PAD.encode(change.raw_bytes()),
        })
    }

    pub fn hashes(&self) -> Result<BTreeSet<String>> {
        let mut statement = self
            .database
            .prepare("SELECT hash FROM changes ORDER BY hash")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }

    pub fn fingerprint(&self) -> Result<String> {
        let mut digest = Sha256::new();
        digest.update(self.roster.hash()?.as_bytes());
        for hash in self.hashes()? {
            digest.update(hash.as_bytes());
        }
        digest.update(b"\0assets\0");
        for asset in self.assets()? {
            digest.update(asset.sha256.as_bytes());
        }
        Ok(hex::encode(digest.finalize()))
    }

    /// Private projection bookkeeping. These records are never synchronized.
    pub fn local_record<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        use rusqlite::OptionalExtension;
        let value: Option<String> = self
            .database
            .query_row(
                "SELECT value FROM metadata WHERE key = ?",
                [format!("local:{key}")],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn set_local_record<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let encoded = serde_json::to_string(value)?;
        if key.len() > 128 || encoded.len() > 4 * 1024 * 1024 {
            return Err(Error::Invalid("Projection record exceeds limit"));
        }
        self.database.execute("INSERT INTO metadata(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![format!("local:{key}"), encoded])?;
        Ok(())
    }

    pub fn orphaned_documents(&self) -> Result<Vec<DocumentView>> {
        let mut all = self.envelopes()?;
        let mut affected = BTreeSet::new();
        let mut statement = self
            .database
            .prepare("SELECT body FROM orphaned ORDER BY rowid")?;
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            let envelope: ChangeEnvelope = serde_json::from_str(&row?)?;
            envelope.verify()?;
            affected.insert(envelope.payload.document);
            all.push(envelope);
        }
        build_documents(&all)?
            .into_iter()
            .filter(|(id, _)| affected.contains(id))
            .map(|(id, document)| document::view(&document, id))
            .collect()
    }

    pub fn missing(&self, known: &BTreeSet<String>) -> Result<Vec<ChangeEnvelope>> {
        if known.len() > MAX_CHANGES {
            return Err(Error::Invalid("Too many advertised changes"));
        }
        let mut result = Vec::new();
        let mut bytes = 0;
        let budget = crate::MAX_FRAME_BYTES
            .saturating_sub(
                serde_json::to_vec(&self.roster)?.len()
                    + serde_json::to_vec(&self.assets()?)?.len()
                    + 1024,
            )
            .min(3 * 1024 * 1024);
        let mut statement = self
            .database
            .prepare("SELECT hash, body FROM changes ORDER BY rowid")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if known.contains(&row.get::<_, String>(0)?) {
                continue;
            }
            let body: String = row.get(1)?;
            let size = body.len();
            if !result.is_empty() && bytes + size > budget {
                break;
            }
            bytes += size;
            result.push(serde_json::from_str(&body)?);
            if result.len() == 128 {
                break;
            }
        }
        Ok(result)
    }

    fn envelopes(&self) -> Result<Vec<ChangeEnvelope>> {
        let mut statement = self
            .database
            .prepare("SELECT body FROM changes ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn apply(&mut self, envelopes: Vec<ChangeEnvelope>) -> Result<bool> {
        if envelopes.len() > 128 {
            return Err(Error::Invalid("Cabal batch exceeds limit"));
        }
        let known = self.hashes()?;
        let mut pending = BTreeMap::new();
        for envelope in envelopes {
            let hash = envelope.hash()?;
            if known.contains(&hash) {
                continue;
            }
            let change = validate_envelope(&envelope, &self.roster)?;
            let body = serde_json::to_string(&envelope)?;
            pending.insert(hash, (envelope, change, body));
        }
        if pending.is_empty() {
            return Ok(false);
        }
        if known.len() + pending.len() > MAX_CHANGES {
            return Err(Error::Invalid("Cabal change limit reached"));
        }
        let bytes: usize = pending.values().map(|(_, _, body)| body.len()).sum();
        if self.stored_bytes + bytes > MAX_STORED_BYTES {
            return Err(Error::Invalid("Cabal storage limit reached"));
        }
        let mut documents = BTreeMap::new();
        for (envelope, change, _) in pending.values() {
            let id = envelope.payload.document;
            let document = documents
                .entry(id)
                .or_insert_with(|| self.documents.get(&id).cloned().unwrap_or_default());
            document.apply_changes([change.clone()])?;
        }
        if self
            .documents
            .keys()
            .chain(documents.keys())
            .collect::<BTreeSet<_>>()
            .len()
            > MAX_DOCUMENTS
        {
            return Err(Error::Invalid("Cabal document limit reached"));
        }
        for (id, value) in &documents {
            if value.get_missing_deps(&[]).is_empty() {
                document::view(value, *id)?;
            }
        }
        let transaction = self.database.transaction()?;
        for (hash, (_, _, body)) in pending {
            transaction.execute(
                "INSERT INTO changes(hash, body) VALUES (?, ?)",
                params![hash, body],
            )?;
        }
        transaction.commit()?;
        self.documents.extend(documents);
        self.stored_bytes += bytes;
        Ok(true)
    }

    pub fn revoke(&mut self, key: PublicKey) -> Result<()> {
        self.require_owner()?;
        if key == self.roster.payload.owner {
            return Err(Error::Invalid("The cabal owner cannot revoke itself"));
        }
        if !self.is_member(key) {
            return Ok(());
        }
        let mut membership = self.roster.payload.clone();
        membership.members.retain(|member| member.key != key);
        membership.revision += 1;
        membership.epoch += 1;
        membership.sealed = self.hashes()?;
        self.accept_roster(self.identity.sign(membership)?)?;
        Ok(())
    }

    pub fn accept_roster(&mut self, roster: Roster) -> Result<bool> {
        validate_roster(&roster)?;
        if roster.payload.owner != self.roster.payload.owner || roster.payload.cabal != self.id() {
            return Err(Error::Invalid("Membership belongs to another cabal"));
        }
        if roster.payload.revision < self.roster.payload.revision {
            return Ok(false);
        }
        if roster.payload.revision == self.roster.payload.revision {
            if roster.hash()? != self.roster.hash()? {
                return Err(Error::Invalid("Conflicting cabal membership decisions"));
            }
            return Ok(false);
        }
        if roster.payload.epoch < self.roster.payload.epoch {
            return Err(Error::Invalid("Cabal epoch moved backwards"));
        }
        let mut accepted = Vec::new();
        let mut orphaned = Vec::new();
        for envelope in self.envelopes()? {
            if validate_envelope(&envelope, &roster).is_ok() {
                accepted.push(envelope);
            } else {
                orphaned.push(envelope);
            }
        }
        let documents = build_documents(&accepted)?;
        let transaction = self.database.transaction()?;
        for envelope in orphaned {
            let hash = envelope.hash()?;
            transaction.execute(
                "INSERT OR IGNORE INTO orphaned(hash, body) VALUES (?, ?)",
                params![hash, serde_json::to_string(&envelope)?],
            )?;
            transaction.execute("DELETE FROM changes WHERE hash = ?", [hash])?;
        }
        transaction.execute(
            "UPDATE metadata SET value = ? WHERE key = 'roster'",
            [serde_json::to_string(&roster)?],
        )?;
        transaction.commit()?;
        self.roster = roster;
        self.documents = documents;
        self.stored_bytes = accepted
            .iter()
            .map(|envelope| serde_json::to_vec(envelope).map(|bytes| bytes.len()))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .sum();
        Ok(true)
    }

    pub fn orphaned_changes(&self) -> Result<usize> {
        let count: u32 = self
            .database
            .query_row("SELECT count(*) FROM orphaned", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn require_member(&self) -> Result<()> {
        if !self.is_member(self.identity.public_key()) {
            return Err(Error::Invalid("This device is no longer a cabal member"));
        }
        Ok(())
    }
    fn require_owner(&self) -> Result<()> {
        if self.identity.public_key() != self.roster.payload.owner {
            return Err(Error::Invalid(
                "Only the cabal founder can admit or remove devices",
            ));
        }
        Ok(())
    }
}

fn open_database(path: &Path) -> Result<Connection> {
    let existing = path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(Error::Invalid("Cabal store cannot be a symbolic link"));
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if existing && version != STORE_VERSION {
        return Err(Error::Invalid(
            "Unsupported cabal store version; storage was preserved",
        ));
    }
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS changes(hash TEXT PRIMARY KEY, body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS orphaned(hash TEXT PRIMARY KEY, body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS invitations(hash TEXT PRIMARY KEY, member TEXT, expires_at INTEGER NOT NULL CHECK(expires_at > 0)) STRICT;")?;
    assets::initialize(&connection)?;
    connection.pragma_update(None, "user_version", STORE_VERSION)?;
    Ok(connection)
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "Cabal names must contain 1–128 bytes without control characters",
        ));
    }
    Ok(())
}

fn validate_roster(roster: &Roster) -> Result<()> {
    roster.verify()?;
    let value = &roster.payload;
    validate_name(&value.name)?;
    if value.schema != 1
        || value.owner != roster.signer
        || value.members.is_empty()
        || value.members.len() > MAX_MEMBERS
        || value.sealed.len() > MAX_CHANGES
        || !value.members.iter().any(|member| member.key == value.owner)
    {
        return Err(Error::Invalid("Invalid cabal membership"));
    }
    let mut keys = BTreeSet::new();
    for member in &value.members {
        validate_name(&member.name)?;
        if !keys.insert(member.key) {
            return Err(Error::Invalid("Duplicate cabal member"));
        }
    }
    Ok(())
}

fn validate_envelope(envelope: &ChangeEnvelope, roster: &Roster) -> Result<Change> {
    if envelope.payload.change.len() > MAX_CHANGE_BYTES * 4 / 3 + 4 {
        return Err(Error::Invalid("CRDT change exceeds limit"));
    }
    envelope.verify()?;
    let payload = &envelope.payload;
    if payload.schema != 2 || payload.cabal != roster.payload.cabal {
        return Err(Error::Invalid("Change belongs to another cabal"));
    }
    let sealed = roster.payload.sealed.contains(&envelope.hash()?);
    if !sealed
        && (payload.epoch != roster.payload.epoch
            || !roster
                .payload
                .members
                .iter()
                .any(|member| member.key == envelope.signer))
    {
        return Err(Error::Invalid("Change is outside current cabal membership"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&payload.change)
        .map_err(|_| Error::Invalid("Invalid CRDT change"))?;
    // Our pinned Automerge wire format has a 4-byte magic, 4-byte checksum,
    // then chunk type 1 for raw changes. Reject compressed/doc/bundle chunks
    // before the upstream decoder can allocate inflated data.
    if bytes.len() > MAX_CHANGE_BYTES || bytes.get(8) != Some(&1) {
        return Err(Error::Invalid(
            "Expected a bounded uncompressed CRDT change",
        ));
    }
    let change = Change::from_bytes(bytes).map_err(|_| Error::Invalid("Invalid CRDT change"))?;
    let actor = change.actor_id().to_bytes();
    if actor.len() != 48 || &actor[..32] != envelope.signer.as_bytes() {
        return Err(Error::Invalid("CRDT actor does not belong to its signer"));
    }
    Ok(change)
}

fn build_documents(envelopes: &[ChangeEnvelope]) -> Result<BTreeMap<Uuid, Automerge>> {
    let mut documents: BTreeMap<Uuid, Automerge> = BTreeMap::new();
    for envelope in envelopes {
        let payload = &envelope.payload;
        let document = documents.entry(payload.document).or_default();
        let bytes = URL_SAFE_NO_PAD
            .decode(&payload.change)
            .map_err(|_| Error::Invalid("Invalid CRDT change"))?;
        let change =
            Change::from_bytes(bytes).map_err(|_| Error::Invalid("Invalid CRDT change"))?;
        document.apply_changes([change])?;
    }
    if documents.len() > MAX_DOCUMENTS {
        return Err(Error::Invalid("Cabal document limit reached"));
    }
    for (id, value) in &documents {
        if value.get_missing_deps(&[]).is_empty() {
            document::view(value, *id)?;
        }
    }
    Ok(documents)
}
