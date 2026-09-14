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
    envelopes[0].payload.name = "forged.md".into();
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
