use automerge::{ActorId, Automerge, Change, ReadDoc, transaction::Transactable};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use loom_cabal::{
    Cabal, ChangePayload, Edit, Identity, Network, NetworkMode, Result, SyncDocument, SyncState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

fn admit(owner: &mut Cabal, path: &std::path::Path, name: &str) -> Result<Cabal> {
    let identity = Identity::generate()?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    let roster = owner.admit(&invitation.token, identity.public_key(), name)?;
    Cabal::import(path, identity, roster)
}

fn catch_up(source: &Cabal, destination: &mut Cabal) -> Result<usize> {
    destination.accept_roster(source.roster().clone())?;
    for page in 0..100 {
        let changes = source.missing_causal(&destination.sync_state()?)?;
        assert!(changes.len() <= 128);
        if changes.is_empty() {
            assert_eq!(source.sync_state()?, destination.sync_state()?);
            return Ok(page);
        }
        assert!(destination.apply(changes)?);
    }
    panic!("Causal synchronization did not converge");
}

#[test]
fn late_join_recovers_removed_authors_history_across_partial_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut author = admit(&mut owner, &directory.path().join("author.db"), "Fern")?;
    let document = owner.create_document("Garden.md", "The pond")?;
    catch_up(&owner, &mut author)?;
    let client = Uuid::new_v4();
    for index in 0..400 {
        author.edit(&Edit {
            document: document.id,
            client,
            basis: author.view(document.id)?.heads,
            text: format!("The pond, revision {index}. 蜜蜂！"),
        })?;
    }
    catch_up(&author, &mut owner)?;
    owner.revoke(author.identity().public_key())?;
    assert_eq!(owner.roster().payload.sealed.len(), 1);
    assert_eq!(owner.roster().payload.sealed[&document.id].len(), 1);
    assert!(serde_json::to_vec(owner.roster())?.len() < 2048);
    let path = directory.path().join("newcomer.db");
    let mut newcomer = admit(&mut owner, &path, "Moss")?;
    let first = owner.missing_causal(&newcomer.sync_state()?)?;
    assert_eq!(first.len(), 128);
    newcomer.apply(first)?;
    assert!(
        newcomer.views()?.is_empty(),
        "Partial sealed history is not a manuscript"
    );
    let partial_state = newcomer.sync_state()?;
    assert!(!partial_state.documents[&document.id].need.is_empty());
    let identity = newcomer.identity().clone();
    drop(newcomer);
    let mut newcomer = Cabal::open(&path, identity)?;
    assert_eq!(newcomer.sync_state()?, partial_state);
    assert_eq!(catch_up(&owner, &mut newcomer)?, 3);
    assert_eq!(
        newcomer.view(document.id)?.text,
        "The pond, revision 399. 蜜蜂！"
    );
    assert_eq!(newcomer.hashes()?, owner.hashes()?);
    assert!(serde_json::to_vec(&newcomer.sync_state()?)?.len() < 512);
    Ok(())
}

#[test]
fn catching_up_to_a_seal_restores_proven_ancestors_without_losing_offline_edits() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut offline = admit(&mut owner, &directory.path().join("offline.db"), "Fern")?;
    let removed = admit(&mut owner, &directory.path().join("removed.db"), "Removed")?;
    let document = owner.create_document("Garden.md", "Pond\nWillow\n")?;
    catch_up(&owner, &mut offline)?;
    offline.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: document.heads.clone(),
        text: "Pond\nWillow and reeds\n".into(),
    })?;
    let client = Uuid::new_v4();
    for index in 0..260 {
        owner.edit(&Edit {
            document: document.id,
            client,
            basis: owner.view(document.id)?.heads,
            text: format!("Pond {index}\nWillow\n"),
        })?;
    }
    owner.revoke(removed.identity().public_key())?;
    offline.accept_roster(owner.roster().clone())?;
    assert!(offline.orphaned_changes()? > 0);
    catch_up(&owner, &mut offline)?;
    assert_eq!(
        offline.orphaned_changes()?,
        1,
        "Proven original history leaves quarantine"
    );
    let recovery = offline.orphaned_documents()?;
    assert_eq!(recovery.len(), 1);
    assert!(recovery[0].text.contains("Willow and reeds"));
    assert!(offline.view(document.id)?.text.contains("Pond 259"));
    assert!(!offline.view(document.id)?.text.contains("and reeds"));
    Ok(())
}

#[test]
fn a_current_members_dependency_does_not_authorize_a_removed_members_change() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut removed = admit(&mut owner, &directory.path().join("removed.db"), "Removed")?;
    let member = admit(&mut owner, &directory.path().join("member.db"), "Fern")?;
    let document = owner.create_document("Garden.md", "Shared")?;
    catch_up(&owner, &mut removed)?;
    removed.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: removed.view(document.id)?.heads,
        text: "Unseen before removal".into(),
    })?;
    let smuggled = removed.missing_causal(&owner.sync_state()?)?;
    assert_eq!(smuggled.len(), 1);
    let mut fork = Automerge::new();
    for envelope in removed.missing_causal(&SyncState::default())? {
        fork.apply_changes([Change::from_bytes(
            URL_SAFE_NO_PAD
                .decode(envelope.payload.change)
                .expect("signed change encoding"),
        )
        .expect("signed Automerge change")])?;
    }
    owner.revoke(removed.identity().public_key())?;
    let mut actor = member.identity().public_key().as_bytes().to_vec();
    actor.extend_from_slice(Uuid::new_v4().as_bytes());
    fork.set_actor(ActorId::from(actor));
    let (_, body) = fork
        .get(automerge::ROOT, "text")?
        .expect("shared text object");
    let mut transaction = fork.transaction();
    transaction.update_text(&body, "A current member builds on unseen history")?;
    transaction.commit();
    let child = fork.get_last_local_change().expect("new member change");
    let envelope = member.identity().sign(ChangePayload {
        schema: 2,
        cabal: owner.id(),
        epoch: owner.roster().payload.epoch,
        document: document.id,
        change: URL_SAFE_NO_PAD.encode(child.raw_bytes()),
    })?;
    owner.apply(vec![envelope])?;
    assert!(owner.apply(smuggled.clone()).is_err());
    // A member cannot make its own removal impossible by naming a dependency
    // no honest peer can supply. Incomplete new work is quarantined at removal.
    owner.revoke(member.identity().public_key())?;
    assert_eq!(owner.orphaned_changes()?, 1);
    assert_eq!(owner.view(document.id)?.text, "Shared");
    assert!(owner.apply(smuggled).is_err());
    Ok(())
}

#[test]
fn inventory_limits_and_holes_are_checked_before_building_a_reply() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let document = owner.create_document("Garden.md", "Shared")?;
    let head = document.heads[0].clone();
    let overlap = SyncState {
        documents: BTreeMap::from([(
            document.id,
            SyncDocument {
                heads: BTreeSet::from([head.clone()]),
                need: BTreeSet::from([head]),
            },
        )]),
    };
    assert!(owner.missing_causal(&overlap).is_err());
    let oversized = SyncState {
        documents: BTreeMap::from([(
            document.id,
            SyncDocument {
                heads: (0..257).map(|number| format!("{number:064x}")).collect(),
                need: BTreeSet::new(),
            },
        )]),
    };
    assert!(owner.missing_causal(&oversized).is_err());
    let mut malformed = SyncState::default();
    malformed
        .documents
        .entry(document.id)
        .or_default()
        .need
        .insert("not a hash".into());
    assert!(owner.missing_causal(&malformed).is_err());
    Ok(())
}

#[test]
fn incompatible_store_is_preserved_without_migration() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("old.db");
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch("PRAGMA user_version = 4; CREATE TABLE keep (body TEXT); INSERT INTO keep VALUES ('original');")?;
    drop(connection);
    let bytes = std::fs::read(&path)?;
    assert!(Cabal::open(&path, Identity::generate()?).is_err());
    assert_eq!(std::fs::read(path)?, bytes);
    Ok(())
}

#[test]
fn removal_preserves_earlier_seals_while_the_owner_is_catching_up() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        identity.clone(),
        "Garden",
        "Sage",
    )?;
    let first = admit(&mut owner, &directory.path().join("first.db"), "First")?;
    let document = owner.create_document("Garden.md", "Remember this history")?;
    owner.revoke(first.identity().public_key())?;
    let next = admit(&mut owner, &directory.path().join("next.db"), "Next")?;
    let mut partial = Cabal::import(
        &directory.path().join("partial.db"),
        identity,
        owner.roster().clone(),
    )?;
    assert!(partial.views()?.is_empty());
    partial.revoke(next.identity().public_key())?;
    assert_eq!(
        partial.roster().payload.sealed,
        owner.roster().payload.sealed
    );
    catch_up(&owner, &mut partial)?;
    assert_eq!(partial.view(document.id)?.text, "Remember this history");
    Ok(())
}

#[test]
fn sealed_changes_cannot_be_rebound_to_another_document() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut author = admit(&mut owner, &directory.path().join("author.db"), "Fern")?;
    author.create_document("Garden.md", "Original author")?;
    catch_up(&author, &mut owner)?;
    let mut payload = author
        .missing_causal(&SyncState::default())?
        .remove(0)
        .payload;
    owner.revoke(author.identity().public_key())?;
    payload.document = Uuid::new_v4();
    let replay = author.identity().sign(payload)?;
    replay.verify()?;
    assert!(owner.apply(vec![replay]).is_err());
    assert_eq!(owner.views()?.len(), 1);
    Ok(())
}

#[test]
fn failed_page_commit_keeps_the_previous_causal_and_authority_indexes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let path = directory.path().join("reader.db");
    let mut reader = admit(&mut owner, &path, "Fern")?;
    let document = owner.create_document("Garden.md", "Original")?;
    for index in 0..3 {
        owner.edit(&Edit {
            document: document.id,
            client: Uuid::new_v4(),
            basis: owner.view(document.id)?.heads,
            text: format!("Edit {index}"),
        })?;
    }
    let page = owner.missing_causal(&reader.sync_state()?)?;
    let mut hashes = page
        .iter()
        .map(|change| change.hash())
        .collect::<Result<Vec<_>>>()?;
    hashes.sort();
    let connection = rusqlite::Connection::open(&path)?;
    // The fourth INSERT fails after earlier INSERTs in the same transaction.
    connection.execute_batch(&format!("CREATE TRIGGER reject_page BEFORE INSERT ON changes WHEN NEW.hash = '{}' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;", hashes[3]))?;
    let before = reader.sync_state()?;
    assert!(reader.apply(page.clone()).is_err());
    assert_eq!(reader.sync_state()?, before);
    assert!(reader.hashes()?.is_empty());
    assert!(reader.views()?.is_empty());
    connection.execute_batch("DROP TRIGGER reject_page;")?;
    reader.apply(page)?;
    assert_eq!(reader.view(document.id)?.text, "Edit 2");
    let identity = reader.identity().clone();
    drop(reader);
    connection.execute(
        "UPDATE changes SET hash = ? WHERE hash = ?",
        ["0".repeat(64), hashes[0].clone()],
    )?;
    assert!(Cabal::open(&path, identity).is_err());
    Ok(())
}

#[tokio::test]
async fn quic_pages_a_sealed_history_to_a_new_member() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        identity.clone(),
        "Garden",
        "Sage",
    )?;
    let removed = admit(&mut owner, &directory.path().join("removed.db"), "Removed")?;
    let document = owner.create_document("Garden.md", "A pond")?;
    let client = Uuid::new_v4();
    for index in 0..150 {
        owner.edit(&Edit {
            document: document.id,
            client,
            basis: owner.view(document.id)?.heads,
            text: format!("A pond, revision {index}."),
        })?;
    }
    owner.revoke(removed.identity().public_key())?;
    let host = Network::start(&identity, NetworkMode::Direct {}).await?;
    let remote_identity = Identity::generate()?;
    let remote = Network::start(&remote_identity, NetworkMode::Direct {}).await?;
    let invitation = owner.invite(host.address())?;
    let owner = Arc::new(Mutex::new(owner));
    host.add(owner.clone())?;
    let roster = remote.join(&invitation, "Moss").await?;
    let reader = Cabal::import(&directory.path().join("reader.db"), remote_identity, roster)?;
    reader.remember_peer(&host.address())?;
    let reader = Arc::new(Mutex::new(reader));
    remote.add(reader.clone())?;
    let transferred = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            remote
                .sync_now(reader.clone(), identity.public_key())
                .await?;
            if reader.lock().expect("reader lock").hashes()?
                == owner.lock().expect("owner lock").hashes()?
            {
                return Ok::<(), loom_cabal::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    // Always join both owned endpoints, including a failed assertion path.
    remote.shutdown().await?;
    host.shutdown().await?;
    transferred.expect("sealed history transfer timed out")?;
    assert_eq!(
        reader.lock().expect("reader lock").view(document.id)?.text,
        "A pond, revision 149."
    );
    Ok(())
}
