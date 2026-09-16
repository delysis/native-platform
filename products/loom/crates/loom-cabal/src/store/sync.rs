//! Bounded causal inventories. A head commits to its entire dependency graph;
//! `need` marks holes in a partially received graph, not permission to edit it.
use automerge::{Automerge, Change, ChangeHash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use super::{Cabal, ChangeEnvelope, Roster};
use crate::{Error, MAX_DOCUMENTS, Result};

pub(super) const MAX_FRONTIER: usize = 256;
pub(super) type ChangeKey = (Uuid, ChangeHash);
pub(super) type ChangeIndex = BTreeMap<ChangeKey, IndexedChange>;

#[derive(Clone)]
pub(super) struct IndexedChange {
    pub envelope: String,
    pub dependencies: Vec<ChangeHash>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncState {
    pub documents: BTreeMap<Uuid, SyncDocument>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncDocument {
    /// Tips of the stored graph, including changes waiting on dependencies.
    pub heads: BTreeSet<String>,
    /// All missing direct dependencies and missing owner-sealed heads.
    pub need: BTreeSet<String>,
}

impl SyncState {
    pub(super) fn validate(&self) -> Result<()> {
        if self.documents.len() > MAX_DOCUMENTS {
            return Err(Error::Invalid("Too many advertised documents"));
        }
        for document in self.documents.values() {
            validate_hashes(&document.heads)?;
            validate_hashes(&document.need)?;
            if !document.heads.is_disjoint(&document.need) {
                return Err(Error::Invalid("A stored head cannot also be missing"));
            }
        }
        Ok(())
    }
}

pub(super) fn validate_hashes(hashes: &BTreeSet<String>) -> Result<()> {
    if hashes.len() > MAX_FRONTIER {
        return Err(Error::Invalid("Cabal causal frontier exceeds limit"));
    }
    for hash in hashes {
        let parsed: ChangeHash = hash
            .parse()
            .map_err(|_| Error::Invalid("Invalid document change hash"))?;
        if parsed.to_string() != *hash {
            return Err(Error::Invalid("Document change hash is not canonical"));
        }
    }
    Ok(())
}

pub(super) fn index_change(
    index: &mut ChangeIndex,
    envelope: &ChangeEnvelope,
    change: &Change,
) -> Result<()> {
    let key = (envelope.payload.document, change.hash());
    let hash = envelope.hash()?;
    if index.get(&key).is_some_and(|old| old.envelope != hash) {
        return Err(Error::Invalid(
            "A causal change has conflicting signed envelopes",
        ));
    }
    if change.deps().len() > MAX_FRONTIER {
        return Err(Error::Invalid("CRDT change has too many dependencies"));
    }
    index.insert(
        key,
        IndexedChange {
            envelope: hash,
            dependencies: change.deps().to_vec(),
        },
    );
    Ok(())
}

/// Follow only hashes committed by the owner's seal. An admitted member's new
/// change cannot authorize an old, unsealed change merely by depending on it.
pub(super) fn sealed_history(roster: &Roster, index: &ChangeIndex) -> Result<BTreeSet<ChangeKey>> {
    let mut sealed = BTreeSet::new();
    for (id, heads) in &roster.payload.sealed {
        for head in heads {
            sealed.insert((
                *id,
                head.parse()
                    .map_err(|_| Error::Invalid("Invalid sealed head"))?,
            ));
        }
    }
    extend_sealed(&mut sealed, index);
    Ok(sealed)
}

/// New arrivals may fill a hole in the sealed graph. Already-present ancestors
/// were expanded when admitted; only this batch's edges need to be followed.
pub(super) fn extend_sealed(sealed: &mut BTreeSet<ChangeKey>, incoming: &ChangeIndex) {
    let mut queue: Vec<_> = incoming
        .keys()
        .filter(|key| sealed.contains(key))
        .copied()
        .collect();
    let mut visited = BTreeSet::new();
    while let Some(key) = queue.pop() {
        if !visited.insert(key) {
            continue;
        }
        if let Some(change) = incoming.get(&key) {
            for dependency in &change.dependencies {
                let parent = (key.0, *dependency);
                sealed.insert(parent);
                queue.push(parent);
            }
        }
    }
}

pub(super) fn state(
    roster: &Roster,
    index: &ChangeIndex,
    incoming: &ChangeIndex,
) -> Result<SyncState> {
    let mut result = SyncState::default();
    for &(id, hash) in index.keys().chain(incoming.keys()) {
        result
            .documents
            .entry(id)
            .or_default()
            .heads
            .insert(hash.to_string());
    }
    for ((id, _), change) in index.iter().chain(incoming.iter()) {
        let document = result.documents.entry(*id).or_default();
        for dependency in &change.dependencies {
            document.heads.remove(&dependency.to_string());
            if !index.contains_key(&(*id, *dependency))
                && !incoming.contains_key(&(*id, *dependency))
            {
                document.need.insert(dependency.to_string());
            }
        }
    }
    for (id, heads) in &roster.payload.sealed {
        let document = result.documents.entry(*id).or_default();
        for head in heads {
            let hash = head
                .parse()
                .map_err(|_| Error::Invalid("Invalid sealed head"))?;
            if !index.contains_key(&(*id, hash)) && !incoming.contains_key(&(*id, hash)) {
                document.need.insert(head.clone());
            }
        }
    }
    result.validate()?;
    Ok(result)
}

/// Seal complete causal history, retaining any earlier sealed roots that have
/// not arrived yet. Unresolved new changes must never prevent an owner from
/// removing a device, nor make their unknown dependencies authoritative.
pub(super) fn seal_frontier(
    roster: &Roster,
    documents: &BTreeMap<Uuid, Automerge>,
    incoming: &BTreeMap<Uuid, Automerge>,
) -> Result<BTreeMap<Uuid, BTreeSet<String>>> {
    let mut sealed = roster.payload.sealed.clone();
    for (id, document) in documents
        .iter()
        .filter(|entry| !incoming.contains_key(entry.0))
        .chain(incoming.iter())
    {
        let heads = sealed.entry(*id).or_default();
        let mut next: BTreeSet<_> = document
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect();
        for head in heads.iter() {
            let hash = head
                .parse()
                .map_err(|_| Error::Invalid("Invalid sealed head"))?;
            if document.get_change_meta_by_hash(&hash).is_none() {
                next.insert(head.clone());
            }
        }
        validate_hashes(&next)?;
        *heads = next;
    }
    sealed.retain(|_, heads| !heads.is_empty());
    Ok(sealed)
}

impl Cabal {
    pub fn sync_state(&self) -> Result<SyncState> {
        state(&self.roster, &self.change_index, &ChangeIndex::new())
    }

    /// Send children before their dependencies. The recipient can then verify
    /// old-epoch ancestors against the owner's sealed heads, one bounded page
    /// at a time, even when it joined after the original author was removed.
    pub fn missing_causal(&self, known: &SyncState) -> Result<Vec<ChangeEnvelope>> {
        known.validate()?;
        let mut have = BTreeSet::new();
        for (id, document) in &known.documents {
            let mut queue = document.heads.iter().cloned().collect::<Vec<_>>();
            while let Some(hash) = queue.pop() {
                if document.need.contains(&hash) {
                    continue;
                }
                let key = (
                    *id,
                    hash.parse().map_err(|_| Error::Invalid("Invalid head"))?,
                );
                if !have.insert(key) {
                    continue;
                }
                if let Some(change) = self.change_index.get(&key) {
                    queue.extend(change.dependencies.iter().map(ToString::to_string));
                }
            }
        }
        let local = self.sync_state()?;
        let mut roots = Vec::new();
        // Seals come first: they are the authority for old-epoch dependencies.
        for (id, heads) in &self.roster.payload.sealed {
            roots.extend(heads.iter().map(|hash| (*id, hash.clone())));
        }
        for (id, document) in &known.documents {
            roots.extend(document.need.iter().map(|hash| (*id, hash.clone())));
        }
        for (id, document) in &local.documents {
            roots.extend(document.heads.iter().map(|hash| (*id, hash.clone())));
        }
        let budget = crate::MAX_FRAME_BYTES
            .saturating_sub(
                serde_json::to_vec(&self.roster)?.len()
                    + serde_json::to_vec(&self.assets()?)?.len()
                    + 1024,
            )
            .min(3 * 1024 * 1024);
        let mut bytes = 0;
        let mut result = Vec::new();
        let mut sent = BTreeSet::new();
        for (id, root) in roots {
            let mut queue = vec![root.parse().map_err(|_| Error::Invalid("Invalid head"))?];
            while let Some(hash) = queue.pop() {
                let key = (id, hash);
                if have.contains(&key) || !sent.insert(key) {
                    continue;
                }
                let Some(change) = self.change_index.get(&key) else {
                    continue;
                };
                let body: String = self.database.query_row(
                    "SELECT body FROM changes WHERE hash = ?",
                    [&change.envelope],
                    |row| row.get(0),
                )?;
                if bytes + body.len() + 1 > budget {
                    if result.is_empty() {
                        return Err(Error::Invalid(
                            "Signed change exceeds synchronization budget",
                        ));
                    }
                    return Ok(result);
                }
                bytes += body.len() + 1;
                result.push(serde_json::from_str(&body)?);
                if result.len() == 128 {
                    return Ok(result);
                }
                queue.extend(change.dependencies.iter().rev());
            }
        }
        Ok(result)
    }
}
