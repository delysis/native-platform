use loom_cabal::{Cabal, Edit, Identity, Network, NetworkMode, Result};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

fn admit(owner: &mut Cabal, path: &Path, name: &str) -> Result<Cabal> {
    let key = Identity::generate()?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    let roster = owner.admit(&invitation.token, key.public_key(), name)?;
    Cabal::import(path, key, roster)
}

fn sync(source: &Cabal, destination: &mut Cabal) -> Result<()> {
    destination.accept_roster(source.roster().clone())?;
    for _ in 0..10 {
        let changes = source.missing_causal(&destination.sync_state()?)?;
        if changes.is_empty() {
            return Ok(());
        }
        if !destination.apply(changes)? {
            return Ok(());
        }
    }
    panic!("fixture did not converge");
}

#[test]
fn handoff_keeps_offline_writing_and_pairing_but_moves_admission_authority() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let path = directory.path().join("peer.db");
    let mut peer = admit(&mut owner, &path, "Fern")?;
    let document = owner.create_document("Garden.md", "Pond\nWillow\n")?;
    sync(&owner, &mut peer)?;
    let invitation = owner.invite(owner.identity().public_key().into())?;
    peer.edit(&Edit {
        document: document.id,
        client: Uuid::new_v4(),
        basis: document.heads,
        text: "Pond\nWillow and reeds\n".into(),
    })?;
    let epoch = owner.roster().payload.epoch;
    owner.transfer_owner(peer.identity().public_key(), &owner.roster().hash()?)?;
    assert_eq!(owner.roster().payload.owner, peer.identity().public_key());
    assert_eq!(owner.roster().payload.epoch, epoch);
    sync(&owner, &mut peer)?;
    assert_eq!(peer.orphaned_changes()?, 0);
    sync(&peer, &mut owner)?;
    assert_eq!(owner.view(document.id)?.text, "Pond\nWillow and reeds\n");
    assert!(owner.invite(owner.identity().public_key().into()).is_err());
    assert!(
        owner
            .admit(
                &invitation.token,
                Identity::generate()?.public_key(),
                "Late"
            )
            .is_err()
    );
    assert!(owner.revoke(peer.identity().public_key()).is_err());
    let identity = peer.identity().clone();
    drop(peer);
    let mut peer = Cabal::open(&path, identity)?;
    let newcomer = admit(&mut peer, &directory.path().join("new.db"), "Moss")?;
    assert!(peer.is_member(newcomer.identity().public_key()));
    peer.revoke(owner.identity().public_key())?;
    owner.accept_roster(peer.roster().clone())?;
    assert!(!owner.is_member(owner.identity().public_key()));
    assert_eq!(owner.view(document.id)?.text, "Pond\nWillow and reeds\n");
    Ok(())
}

#[test]
fn a_review_cannot_handoff_after_membership_changes_or_to_an_unknown_device() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let peer = admit(&mut owner, &directory.path().join("peer.db"), "Fern")?;
    let review = owner.roster().hash()?;
    admit(&mut owner, &directory.path().join("other.db"), "Moss")?;
    let current = owner.roster().hash()?;
    assert!(
        owner
            .transfer_owner(peer.identity().public_key(), &review)
            .is_err()
    );
    assert!(
        owner
            .transfer_owner(owner.identity().public_key(), &current)
            .is_err()
    );
    assert!(
        owner
            .transfer_owner(Identity::generate()?.public_key(), &current)
            .is_err()
    );
    assert_eq!(owner.roster().hash()?, current);
    Ok(())
}

#[test]
fn former_owner_cannot_replay_a_high_revision_or_alter_the_exact_handoff() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        identity.clone(),
        "Garden",
        "Sage",
    )?;
    let mut peer = admit(&mut owner, &directory.path().join("peer.db"), "Fern")?;
    let mut stale = owner.roster().payload.clone();
    owner.transfer_owner(peer.identity().public_key(), &owner.roster().hash()?)?;
    peer.accept_roster(owner.roster().clone())?;
    let accepted = peer.roster().hash()?;
    stale.revision = 1_000_000;
    stale.epoch = 1_000_000;
    assert!(!peer.accept_roster(identity.sign(stale)?)?);
    let mut altered = owner.roster().payload.clone();
    altered.name = "A different decision".into();
    assert!(peer.accept_roster(identity.sign(altered)?).is_err());
    let mut forged = owner.roster().payload.clone();
    forged.revision += 1;
    assert!(peer.accept_roster(identity.sign(forged)?).is_err());
    assert_eq!(peer.roster().hash()?, accepted);
    Ok(())
}

#[test]
fn ownership_forks_and_rebinding_fail_while_an_offline_member_accepts_a_chain() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        identity.clone(),
        "Garden",
        "Sage",
    )?;
    let mut fern = admit(&mut owner, &directory.path().join("fern.db"), "Fern")?;
    let mut moss = admit(&mut owner, &directory.path().join("moss.db"), "Moss")?;
    let mut offline = admit(&mut owner, &directory.path().join("offline.db"), "Willow")?;
    let original = owner.roster().clone();
    let mut fork = Cabal::import(
        &directory.path().join("fork.db"),
        identity.clone(),
        original,
    )?;
    owner.transfer_owner(fern.identity().public_key(), &owner.roster().hash()?)?;
    fern.accept_roster(owner.roster().clone())?;
    fork.transfer_owner(moss.identity().public_key(), &fork.roster().hash()?)?;
    assert!(fern.accept_roster(fork.roster().clone()).is_err());
    let mut rebound = owner.roster().payload.clone();
    rebound.cabal = Uuid::new_v4();
    assert!(
        Cabal::import(
            &directory.path().join("rebound.db"),
            identity.clone(),
            identity.sign(rebound)?
        )
        .is_err()
    );
    fern.transfer_owner(moss.identity().public_key(), &fern.roster().hash()?)?;
    moss.accept_roster(fern.roster().clone())?;
    offline.accept_roster(moss.roster().clone())?;
    assert_eq!(offline.roster().payload.authority.len(), 2);
    assert_eq!(offline.roster().payload.owner, moss.identity().public_key());
    assert!(offline.is_member(offline.identity().public_key()));
    assert!(!offline.accept_roster(owner.roster().clone())?);
    Ok(())
}

#[test]
fn failed_handoff_commit_preserves_the_current_owner_and_signed_roster() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("owner.db");
    let identity = Identity::generate()?;
    let mut owner = Cabal::create(&path, identity.clone(), "Garden", "Sage")?;
    let peer = admit(&mut owner, &directory.path().join("peer.db"), "Fern")?;
    let before = owner.roster().hash()?;
    let database = rusqlite::Connection::open(&path)?;
    database.execute_batch(
        "CREATE TRIGGER refuse_handoff BEFORE UPDATE ON metadata WHEN NEW.key = 'roster'
        BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )?;
    assert!(
        owner
            .transfer_owner(peer.identity().public_key(), &before)
            .is_err()
    );
    assert_eq!(owner.roster().hash()?, before);
    drop(owner);
    let mut owner = Cabal::open(&path, identity)?;
    assert_eq!(owner.roster().hash()?, before);
    database.execute_batch("DROP TRIGGER refuse_handoff")?;
    owner.transfer_owner(peer.identity().public_key(), &before)?;
    assert_eq!(owner.roster().payload.owner, peer.identity().public_key());
    Ok(())
}

#[tokio::test]
async fn a_removed_original_owner_receives_the_current_authority_over_quic() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut peer = admit(&mut owner, &directory.path().join("peer.db"), "Fern")?;
    let old_roster = owner.roster().clone();
    let owner_key = owner.identity().public_key();
    let network = Network::start(peer.identity(), NetworkMode::Direct {}).await?;
    let old_network = Network::start(owner.identity(), NetworkMode::Direct {}).await?;
    owner.transfer_owner(peer.identity().public_key(), &owner.roster().hash()?)?;
    peer.accept_roster(owner.roster().clone())?;
    peer.revoke(owner_key)?;
    let expected = peer.roster().hash()?;
    // The original owner's replacement profile knows only its old signed
    // membership; a peer must still deliver the new chain and its revocation.
    let stale = Cabal::import(
        &directory.path().join("stale.db"),
        owner.identity().clone(),
        old_roster,
    )?;
    stale.remember_peer(&network.address())?;
    let stale = Arc::new(Mutex::new(stale));
    network.add(Arc::new(Mutex::new(peer)))?;
    old_network.add(stale.clone())?;
    // Observe the supervisor's delivery. A competing manual sync can begin
    // after revocation is already committed and correctly refuse membership.
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if stale.lock().expect("old owner lock").roster().hash()? == expected {
                return Ok::<(), loom_cabal::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    old_network.shutdown().await?;
    network.shutdown().await?;
    result.expect("authority synchronization timed out")?;
    let stale = stale.lock().expect("old owner lock");
    assert_eq!(stale.roster().hash()?, expected);
    assert!(!stale.is_member(owner_key));
    Ok(())
}

#[tokio::test]
async fn the_new_owner_can_issue_a_working_invitation_after_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let path = directory.path().join("peer.db");
    let mut peer = admit(&mut owner, &path, "Fern")?;
    owner.transfer_owner(peer.identity().public_key(), &owner.roster().hash()?)?;
    peer.accept_roster(owner.roster().clone())?;
    let identity = peer.identity().clone();
    drop(peer);
    let mut peer = Cabal::open(&path, identity.clone())?;
    let network = Network::start(&identity, NetworkMode::Direct {}).await?;
    let remote = Network::start(&Identity::generate()?, NetworkMode::Direct {}).await?;
    let invitation = peer.invite(network.address())?;
    network.add(Arc::new(Mutex::new(peer)))?;
    let joined = remote.join(&invitation, "Moss").await;
    remote.shutdown().await?;
    network.shutdown().await?;
    let roster = joined?;
    assert_eq!(roster.signer, identity.public_key());
    assert_eq!(roster.payload.owner, identity.public_key());
    assert_eq!(roster.payload.authority.len(), 1);
    Ok(())
}

#[test]
fn ownership_chain_is_bounded_without_resetting_its_trust_root() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut sage = Cabal::create(
        &directory.path().join("sage.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let mut fern = admit(&mut sage, &directory.path().join("fern.db"), "Fern")?;
    let original = sage.identity().public_key();
    for index in 0..64 {
        let (owner, recipient) = if index % 2 == 0 {
            (&mut sage, &mut fern)
        } else {
            (&mut fern, &mut sage)
        };
        owner.transfer_owner(recipient.identity().public_key(), &owner.roster().hash()?)?;
        recipient.accept_roster(owner.roster().clone())?;
    }
    assert_eq!(sage.roster().payload.authority.len(), 64);
    assert_eq!(sage.roster().payload.authority[0].signer, original);
    let before = sage.roster().hash()?;
    assert!(
        sage.transfer_owner(fern.identity().public_key(), &before)
            .is_err()
    );
    assert_eq!(sage.roster().hash()?, before);
    sage.create_document("Still writing.md", "The keys have travelled.")?;
    Ok(())
}

#[test]
fn exhausted_counters_refuse_admission_removal_and_handoff_without_changing_membership()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut owner = Cabal::create(
        &directory.path().join("owner.db"),
        Identity::generate()?,
        "Garden",
        "Sage",
    )?;
    let peer = admit(&mut owner, &directory.path().join("peer.db"), "Fern")?;
    let mut membership = owner.roster().payload.clone();
    membership.revision = u64::MAX;
    owner.accept_roster(owner.identity().sign(membership)?)?;
    let before = owner.roster().hash()?;
    assert!(
        owner
            .transfer_owner(peer.identity().public_key(), &before)
            .is_err()
    );
    assert!(owner.revoke(peer.identity().public_key()).is_err());
    let invitation = owner.invite(owner.identity().public_key().into())?;
    assert!(
        owner
            .admit(
                &invitation.token,
                Identity::generate()?.public_key(),
                "Moss"
            )
            .is_err()
    );
    assert_eq!(owner.roster().hash()?, before);
    Ok(())
}
