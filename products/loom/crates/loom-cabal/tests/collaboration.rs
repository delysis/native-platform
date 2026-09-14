use loom_cabal::{Cabal, Edit, Identity, Network, NetworkMode, Result};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

fn pair(directory: &std::path::Path) -> Result<(Cabal, Cabal)> {
    let alice = Identity::generate()?;
    let bob = Identity::generate()?;
    let mut left = Cabal::create(
        &directory.join("alice.db"),
        alice.clone(),
        "Book club",
        "Alice",
    )?;
    let invitation = left.invite(alice.public_key().into())?;
    let roster = left.admit(&invitation.token, bob.public_key(), "Bob")?;
    let right = Cabal::import(&directory.join("bob.db"), bob, roster)?;
    Ok((left, right))
}

fn sync(left: &mut Cabal, right: &mut Cabal) -> Result<()> {
    right.accept_roster(left.roster().clone())?;
    left.accept_roster(right.roster().clone())?;
    for _ in 0..10 {
        let to_right = left.missing(&right.hashes()?)?;
        let to_left = right.missing(&left.hashes()?)?;
        if to_right.is_empty() && to_left.is_empty() {
            return Ok(());
        }
        left.apply(to_left)?;
        right.apply(to_right)?;
    }
    panic!("Synchronization did not converge");
}

#[test]
fn offline_unicode_edits_converge_and_survive_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let document = alice.create_document("garden.md", "🌱 Garden\n\nRain.\n")?;
    sync(&mut alice, &mut bob)?;
    let alice_view = alice.view(document.id)?;
    let bob_view = bob.view(document.id)?;
    alice.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: alice_view.heads,
        text: "🌱 Our garden\n\nRain.\n".into(),
    })?;
    bob.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: bob_view.heads,
        text: "🌱 Garden\n\nRain. 蜜蜂！\n".into(),
    })?;
    sync(&mut alice, &mut bob)?;
    let text = alice.view(document.id)?.text;
    assert_eq!(text, bob.view(document.id)?.text);
    assert!(text.contains("Our garden"));
    assert!(text.contains("蜜蜂！"));
    let identity = bob.identity().clone();
    drop(bob);
    let recovered = Cabal::open(&directory.path().join("bob.db"), identity)?;
    assert_eq!(recovered.view(document.id)?.text, text);
    assert_eq!(recovered.hashes()?, alice.hashes()?);
    Ok(())
}

#[test]
fn typing_during_a_remote_update_keeps_both_authors() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let document = alice.create_document("draft.md", "Start\nEnd\n")?;
    sync(&mut alice, &mut bob)?;
    let client = Uuid::new_v4();
    let first = alice.edit(&Edit {
        document: document.id,
        client,
        basis: document.heads,
        text: "Starting\nEnd\n".into(),
    })?;
    let basis = bob.view(document.id)?.heads;
    bob.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis,
        text: "Start\nEnding\n".into(),
    })?;
    sync(&mut alice, &mut bob)?;
    // Alice continued typing before receiving the merged reply. Her next edit
    // uses local_heads, not a snapshot she has never seen.
    let second = alice.edit(&Edit {
        document: document.id,
        client,
        basis: first.local_heads,
        text: "Starting now\nEnd\n".into(),
    })?;
    assert_eq!(second.merged.text, "Starting now\nEnding\n");
    sync(&mut alice, &mut bob)?;
    assert_eq!(bob.view(document.id)?.text, second.merged.text);
    Ok(())
}

#[test]
fn signatures_and_membership_are_not_editable_document_data() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    alice.create_document("spell.md", "Honest words")?;
    let mut envelopes = alice.missing(&BTreeSet::new())?;
    envelopes[0].payload.document = Uuid::new_v4();
    assert!(bob.apply(envelopes).is_err());
    assert!(bob.views()?.is_empty());
    let outsider = Identity::generate()?;
    let mut fake_roster = alice.roster().payload.clone();
    fake_roster.owner = outsider.public_key();
    fake_roster.revision += 1;
    assert!(bob.accept_roster(outsider.sign(fake_roster)?).is_err());
    assert_eq!(alice.roster().hash()?, bob.roster().hash()?);
    Ok(())
}

#[test]
fn revocation_seals_history_and_preserves_unmerged_edits_as_orphans() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let document = alice.create_document("story.md", "Together")?;
    sync(&mut alice, &mut bob)?;
    let old_basis = bob.view(document.id)?.heads;
    bob.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: old_basis,
        text: "Together, edited offline".into(),
    })?;
    let offline_changes = bob.missing(&alice.hashes()?)?;
    alice.revoke(bob.identity().public_key())?;
    assert!(alice.apply(offline_changes).is_err());
    bob.accept_roster(alice.roster().clone())?;
    assert_eq!(bob.orphaned_changes()?, 1);
    assert_eq!(bob.view(document.id)?.text, "Together");
    assert!(
        bob.edit(&Edit {
            document: document.id,
            client: Uuid::new_v4(),
            basis: bob.view(document.id)?.heads,
            text: "After revocation".into()
        })
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn real_quic_pairing_sync_and_invitation_retry_are_bound_to_device() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let alice_key = Identity::generate()?;
    let bob_key = Identity::generate()?;
    let mallory_key = Identity::generate()?;
    let alice_network = Network::start(&alice_key, NetworkMode::Local).await?;
    let bob_network = Network::start(&bob_key, NetworkMode::Local).await?;
    let mallory_network = Network::start(&mallory_key, NetworkMode::Local).await?;
    let alice = Arc::new(Mutex::new(Cabal::create(
        &directory.path().join("a.db"),
        alice_key.clone(),
        "Cabal",
        "Alice",
    )?));
    alice_network.add(alice.clone())?;
    let invitation = alice
        .lock()
        .expect("alice lock")
        .invite(alice_network.address())?;
    let roster = bob_network.join(&invitation, "Bob").await?;
    assert_eq!(
        roster.hash()?,
        bob_network.join(&invitation, "Bob").await?.hash()?
    );
    assert!(mallory_network.join(&invitation, "Mallory").await.is_err());
    let bob = Arc::new(Mutex::new(Cabal::import(
        &directory.path().join("b.db"),
        bob_key,
        roster,
    )?));
    bob.lock()
        .expect("bob lock")
        .remember_peer(&alice_network.address())?;
    bob_network.add(bob.clone())?;
    let document = alice
        .lock()
        .expect("alice lock")
        .create_document("hello.md", "Actual encrypted transport")?;
    bob_network
        .sync_now(bob.clone(), alice_key.public_key())
        .await?;
    assert_eq!(
        bob.lock().expect("bob lock").view(document.id)?.text,
        document.text
    );
    alice_network.shutdown().await?;
    bob_network.shutdown().await?;
    mallory_network.shutdown().await?;
    Ok(())
}

#[test]
fn rename_delete_and_offline_typing_keep_one_document_and_all_words() -> Result<()> {
    use loom_cabal::{MetadataEdit, TextKind};
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let document =
        alice.create_document_with_kind("poems/Rain.md", "Rain\n  falls\n", TextKind::Verse)?;
    sync(&mut alice, &mut bob)?;
    let rename = MetadataEdit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: document.heads.clone(),
        name: "poems/Silver rain.md".into(),
        deleted: false,
    };
    alice.edit_metadata(&rename)?;
    bob.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: document.heads.clone(),
        text: "Rain\n  falls softly\n".into(),
    })?;
    // Bob deletes from the same old metadata snapshot without knowing the rename.
    bob.edit_metadata(&MetadataEdit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: bob.view(document.id)?.heads,
        name: document.name,
        deleted: true,
    })?;
    sync(&mut alice, &mut bob)?;
    let merged = alice.view(document.id)?;
    assert_eq!(merged.name, "poems/Silver rain.md");
    assert_eq!(merged.text, "Rain\n  falls softly\n");
    assert_eq!(merged.kind, TextKind::Verse);
    assert!(merged.deleted);
    assert_eq!(merged.heads, bob.view(document.id)?.heads);
    // Retrying the rename must not resurrect the now-deleted document.
    let hashes = alice.hashes()?;
    alice.edit_metadata(&rename)?;
    assert!(alice.view(document.id)?.deleted);
    assert_eq!(alice.hashes()?, hashes);
    Ok(())
}

#[test]
fn a_lost_creation_reply_retries_after_reconnect_and_membership_change() -> Result<()> {
    use loom_cabal::{Create, TextKind};
    let directory = tempfile::tempdir()?;
    let (mut alice, bob) = pair(directory.path())?;
    let request = Create {
        document: Uuid::new_v4(),
        client: Uuid::new_v4(),
        name: "Notebook.md".into(),
        kind: TextKind::Prose,
        text: "An exact first version".into(),
    };
    let created = alice.create_document_idempotent(&request)?;
    alice.revoke(bob.identity().public_key())?;
    let hashes = alice.hashes()?;
    let identity = alice.identity().clone();
    drop(alice);
    let mut reopened = Cabal::open(&directory.path().join("alice.db"), identity)?;
    let retried = reopened.create_document_idempotent(&request)?;
    assert_eq!(retried.local_heads, created.local_heads);
    assert_eq!(reopened.hashes()?, hashes);
    let mut changed = request;
    changed.text.push_str(" with a conflicting reuse");
    assert!(reopened.create_document_idempotent(&changed).is_err());
    assert_eq!(reopened.view(changed.document)?.text, created.merged.text);
    Ok(())
}

#[test]
fn unprojectable_paths_are_rejected_before_entering_the_log() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, _) = pair(directory.path())?;
    for name in [
        "../Escape.md",
        "/Absolute.md",
        "notes/.loom/secret.md",
        "Runs/Result.md",
        "recovery/draft.md",
        "CON.md",
        "a\\b.md",
        "x//y.md",
        "bad:stream.md",
        "trailing. /a.md",
    ] {
        assert!(alice.create_document(name, "preserved").is_err(), "{name}");
    }
    assert!(alice.hashes()?.is_empty());
    alice.create_document("Poems/雨.md", "Rain")?;
    assert_eq!(alice.views()?.len(), 1);
    Ok(())
}
