use super::*;
use crate::{Cabal, Edit, Identity, SyncState};
use automerge::{
    ActorId, Automerge, ROOT,
    transaction::{CommitOptions, Transactable},
};

fn fixture(path: &std::path::Path) -> Result<(Cabal, Uuid, Vec<String>)> {
    let mut cabal = Cabal::create(path, Identity::generate()?, "Garden", "Sage")?;
    let document = cabal.create_document("Garden.md", "Pond\nWillow\n")?;
    Ok((cabal, document.id, document.heads))
}

fn author(cabal: &Cabal, document: Uuid) -> Automerge {
    let mut author = cabal.documents[&document].clone();
    let mut actor = cabal.identity().public_key().as_bytes().to_vec();
    actor.extend_from_slice(Uuid::new_v4().as_bytes());
    author.set_actor(ActorId::from(actor));
    author
}

fn rename(
    cabal: &Cabal,
    author: &mut Automerge,
    document: Uuid,
    index: usize,
    message: &str,
) -> Result<ChangeEnvelope> {
    let mut transaction = author.transaction();
    transaction.put(ROOT, "name", format!("Garden-{index}.md"))?;
    transaction.commit_with(CommitOptions::default().with_message(message));
    cabal.envelope(
        document,
        author
            .get_last_local_change()
            .expect("rename change")
            .clone(),
    )
}

fn edits(cabal: &mut Cabal, document: Uuid, count: usize) -> Result<()> {
    let mut author = author(cabal, document);
    let mut batch = Vec::new();
    for index in 0..count {
        batch.push(rename(cabal, &mut author, document, index, "")?);
        if batch.len() == 128 {
            cabal.apply(std::mem::take(&mut batch))?;
        }
    }
    cabal.apply(batch)?;
    Ok(())
}

#[test]
fn archived_history_exceeds_the_old_change_cap_and_keeps_an_offline_basis() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let (mut cabal, document, basis) = fixture(&path)?;
    let original = cabal.missing_causal(&SyncState::default())?[0].clone();
    edits(&mut cabal, document, 20_010)?;
    assert_eq!(cabal.hashes()?.len(), 20_011);
    let archived: u32 = cabal.database.query_row(
        "SELECT count(*) FROM changes WHERE archived = 1",
        [],
        |row| row.get(0),
    )?;
    assert!(archived > 19_000);
    assert!(cabal.history_usage.recent_count <= MAX_RECENT_COUNT);
    let hashes = cabal.hashes()?;
    let identity = cabal.identity().clone();
    drop(cabal);
    let mut cabal = Cabal::open(&path, identity)?;
    assert_eq!(cabal.hashes()?, hashes);
    assert_eq!(
        serde_json::to_vec(&read(&cabal.database, cabal.id(), &original.hash()?)?.envelope)?,
        serde_json::to_vec(&original)?
    );
    assert_eq!(cabal.view_at(document, &basis)?.text, "Pond\nWillow\n");
    cabal.edit(&Edit {
        document,
        basis,
        client: Uuid::new_v4(),
        text: "Pond\nWillow and reeds\n".into(),
    })?;
    assert_eq!(cabal.view(document)?.name, "Garden-20009.md");
    assert_eq!(cabal.view(document)?.text, "Pond\nWillow and reeds\n");
    Ok(())
}

#[test]
fn archived_payloads_exceed_the_old_byte_cap_without_a_full_history_buffer() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let (mut cabal, document, _) = fixture(&path)?;
    let message = "x".repeat(1024 * 1024);
    let mut author = author(&cabal, document);
    for index in 0..50 {
        let change = rename(&cabal, &mut author, document, index, &message)?;
        cabal.apply(vec![change])?;
    }
    assert!(cabal.history_usage.encoded_bytes > 64 * 1024 * 1024);
    assert!(cabal.history_usage.recent_bytes <= MAX_RECENT_BYTES);
    let stored: u32 =
        cabal
            .database
            .query_row("SELECT sum(length(body)) FROM changes", [], |row| {
                row.get(0)
            })?;
    assert!((stored as usize) < cabal.history_usage.encoded_bytes / 4);
    let identity = cabal.identity().clone();
    let hashes = cabal.hashes()?;
    drop(author);
    drop(cabal);
    let cabal = Cabal::open(&path, identity)?;
    assert_eq!(cabal.hashes()?, hashes);
    assert_eq!(cabal.view(document)?.name, "Garden-49.md");
    assert_eq!(cabal.view(document)?.text, "Pond\nWillow\n");
    Ok(())
}

#[test]
fn failed_archival_rolls_back_both_new_edits_and_existing_representations() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let (mut cabal, document, _) = fixture(&path)?;
    edits(&mut cabal, document, 1023)?;
    assert_eq!(cabal.history_usage.recent_count, MAX_RECENT_COUNT);
    let before = cabal.sync_state()?;
    let hashes = cabal.hashes()?;
    // Fail after multiple older rows have already changed representation.
    cabal.database.execute_batch(
        "CREATE TRIGGER fail_archive BEFORE UPDATE OF body ON changes
        WHEN (SELECT count(*) FROM changes WHERE archived = 1) = 3
        BEGIN SELECT RAISE(ABORT, 'injected archive failure'); END;",
    )?;
    let mut author = author(&cabal, document);
    let change = rename(&cabal, &mut author, document, 2000, "")?;
    assert!(cabal.apply(vec![change.clone()]).is_err());
    assert_eq!(cabal.sync_state()?, before);
    assert_eq!(cabal.hashes()?, hashes);
    assert_eq!(cabal.history_usage.recent_count, MAX_RECENT_COUNT);
    let archived: u32 = cabal.database.query_row(
        "SELECT count(*) FROM changes WHERE archived = 1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(archived, 0);
    cabal.database.execute_batch("DROP TRIGGER fail_archive")?;
    cabal.apply(vec![change])?;
    assert!(cabal.history_usage.recent_count <= RECENT_TARGET_COUNT);
    let identity = cabal.identity().clone();
    drop(cabal);
    assert_eq!(
        Cabal::open(&path, identity)?.view(document)?.name,
        "Garden-2000.md"
    );
    Ok(())
}

#[test]
fn archived_quarantine_keeps_exact_writing_and_retention_budget() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut owner, document, _) = fixture(&directory.path().join("owner.db"))?;
    let identity = Identity::generate()?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    let roster = owner.admit(&invitation.token, identity.public_key(), "Fern")?;
    let path = directory.path().join("peer.db");
    let mut peer = Cabal::import(&path, identity.clone(), roster)?;
    peer.apply(owner.missing_causal(&peer.sync_state()?)?)?;
    edits(&mut peer, document, 1100)?;
    peer.edit(&Edit {
        document,
        basis: peer.view(document)?.heads,
        client: Uuid::new_v4(),
        text: "Private offline reeds\n".into(),
    })?;
    let bytes = peer.history_usage.encoded_bytes;
    let count = peer.history_usage.count;
    owner.revoke(identity.public_key())?;
    peer.accept_roster(owner.roster().clone())?;
    assert_eq!(peer.orphaned_changes()?, 1101);
    assert_eq!(peer.history_usage.encoded_bytes, bytes);
    assert_eq!(peer.history_usage.count, count);
    drop(peer);
    let peer = Cabal::open(&path, identity)?;
    assert_eq!(
        peer.orphaned_documents()?[0].text,
        "Private offline reeds\n"
    );
    assert_eq!(peer.view(document)?.text, "Pond\nWillow\n");
    assert_eq!(peer.history_usage.encoded_bytes, bytes);
    assert_eq!(peer.history_usage.count, count);
    Ok(())
}

#[test]
fn archive_corruption_is_refused_on_read_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let (mut cabal, document, _) = fixture(&path)?;
    edits(&mut cabal, document, 1024)?;
    let hash: String = cabal.database.query_row(
        "SELECT hash FROM changes WHERE compressed = 1 LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let original: Vec<u8> =
        cabal
            .database
            .query_row("SELECT body FROM changes WHERE hash = ?", [&hash], |row| {
                row.get(0)
            })?;
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    cabal.database.execute(
        "UPDATE changes SET body = ? WHERE hash = ?",
        params![corrupt, hash],
    )?;
    assert!(read(&cabal.database, cabal.id(), &hash).is_err());
    let identity = cabal.identity().clone();
    drop(cabal);
    assert!(Cabal::open(&path, identity.clone()).is_err());
    let database = Connection::open(&path)?;
    let truncated = original[..original.len() - 1].to_vec();
    database.execute(
        "UPDATE changes SET body = ? WHERE hash = ?",
        params![truncated, hash],
    )?;
    assert!(Cabal::open(&path, identity.clone()).is_err());
    let mut trailing = original.clone();
    trailing.push(0);
    database.execute(
        "UPDATE changes SET body = ? WHERE hash = ?",
        params![trailing, hash],
    )?;
    assert!(Cabal::open(&path, identity.clone()).is_err());
    database.execute(
        "UPDATE changes SET body = ?, encoded_bytes = encoded_bytes - 1 WHERE hash = ?",
        params![original, hash],
    )?;
    assert!(Cabal::open(&path, identity).is_err());
    Ok(())
}

#[test]
fn archived_ancestors_return_from_quarantine_without_duplicate_storage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut owner, document, _) = fixture(&directory.path().join("owner.db"))?;
    let identity = Identity::generate()?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    let roster = owner.admit(&invitation.token, identity.public_key(), "Fern")?;
    let mut peer = Cabal::import(&directory.path().join("peer.db"), identity, roster)?;
    let removed = Identity::generate()?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    owner.admit(&invitation.token, removed.public_key(), "Moss")?;
    edits(&mut owner, document, 1100)?;
    peer.accept_roster(owner.roster().clone())?;
    for _ in 0..16 {
        let page = owner.missing_causal(&peer.sync_state()?)?;
        if page.is_empty() {
            break;
        }
        peer.apply(page)?;
    }
    assert_eq!(peer.hashes()?, owner.hashes()?);
    let archived: Vec<(String, Vec<u8>)> = peer
        .database
        .prepare("SELECT hash, body FROM changes WHERE archived = 1 ORDER BY hash")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    assert!(!archived.is_empty());
    edits(&mut owner, document, 1100)?;
    owner.revoke(removed.public_key())?;
    peer.accept_roster(owner.roster().clone())?;
    assert_eq!(peer.orphaned_changes()?, 1101);
    for _ in 0..32 {
        let page = owner.missing_causal(&peer.sync_state()?)?;
        if page.is_empty() {
            break;
        }
        peer.apply(page)?;
    }
    assert_eq!(peer.orphaned_changes()?, 0);
    assert_eq!(peer.hashes()?, owner.hashes()?);
    assert_eq!(peer.history_usage.count, owner.history_usage.count);
    assert_eq!(
        peer.history_usage.encoded_bytes,
        owner.history_usage.encoded_bytes
    );
    for (hash, bytes) in archived {
        let (body, compressed, orphaned): (Vec<u8>, bool, bool) = peer.database.query_row(
            "SELECT body, archived, orphaned FROM changes WHERE hash = ?",
            [hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(body, bytes);
        assert!(compressed);
        assert!(!orphaned);
    }
    Ok(())
}
