use super::*;
use crate::Identity;

fn owner(path: &std::path::Path) -> Result<Cabal> {
    Cabal::create(path, Identity::generate()?, "Garden", "Alice")
}

fn count(cabal: &Cabal) -> Result<i64> {
    Ok(cabal
        .database
        .query_row("SELECT count(*) FROM invitations", [], |row| row.get(0))?)
}

#[test]
fn expiry_reclaims_links_without_changing_membership_or_writing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let mut cabal = owner(&path)?;
    let identity = cabal.identity.clone();
    let bob = Identity::generate()?.public_key();
    let outsider = Identity::generate()?.public_key();
    let document = cabal.create_document("garden.md", "Our garden.")?;
    let used = cabal.invite_at(identity.public_key().into(), 100)?;
    let unused = cabal.invite_at(identity.public_key().into(), 100)?;
    let later = cabal.invite_at(identity.public_key().into(), 101)?;
    let roster = cabal.admit_at(&used.token, bob, "Bob", 102)?;
    assert!(
        cabal
            .admit_at(&used.token, outsider, "Mallory", 103)
            .is_err()
    );
    drop(cabal);

    let mut cabal = Cabal::open(&path, identity.clone())?;
    let expiry = 100 + INVITATION_LIFETIME_SECONDS;
    assert_eq!(
        cabal
            .admit_at(&used.token, bob, "Bob", expiry - 1)?
            .hash()?,
        roster.hash()?
    );
    assert!(
        cabal
            .admit_at(&unused.token, outsider, "Mallory", expiry)
            .is_err()
    );
    assert_eq!(count(&cabal)?, 1);
    assert!(cabal.admit_at(&used.token, bob, "Bob", expiry).is_err());
    assert!(cabal.is_member(bob));
    assert_eq!(cabal.roster().hash()?, roster.hash()?);
    assert_eq!(cabal.view(document.id)?.text, "Our garden.");
    // The exact expiry boundary does not remove a newer invitation.
    cabal.admit_at(&later.token, outsider, "Carol", expiry)?;
    assert!(cabal.is_member(outsider));
    Ok(())
}

#[test]
fn a_full_invitation_pool_becomes_available_after_expiry() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut cabal = owner(&directory.path().join("cabal.db"))?;
    let address: EndpointAddr = cabal.identity.public_key().into();
    let first = cabal.invite_at(address.clone(), 1)?;
    for _ in 1..MAX_INVITATIONS {
        cabal.invite_at(address.clone(), 1)?;
    }
    let roster = cabal.roster().hash()?;
    assert!(
        cabal
            .invite_at(address.clone(), INVITATION_LIFETIME_SECONDS)
            .is_err()
    );
    assert_eq!(count(&cabal)?, MAX_INVITATIONS);
    let fresh = cabal.invite_at(address, INVITATION_LIFETIME_SECONDS + 1)?;
    assert_eq!(count(&cabal)?, 1);
    let bob = Identity::generate()?.public_key();
    assert!(cabal.admit_at(&first.token, bob, "Bob", 1).is_err());
    assert_eq!(cabal.roster().hash()?, roster);
    cabal.admit_at(&fresh.token, bob, "Bob", INVITATION_LIFETIME_SECONDS + 2)?;
    Ok(())
}

#[test]
fn clock_rollback_after_reopen_does_not_revive_links_or_mint_expired_ones() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let mut cabal = owner(&path)?;
    let identity = cabal.identity.clone();
    let link = cabal.invite_at(identity.public_key().into(), 100)?;
    let expiry = 100 + INVITATION_LIFETIME_SECONDS;
    let bob = Identity::generate()?.public_key();
    assert!(cabal.admit_at(&link.token, bob, "Bob", expiry).is_err());
    drop(cabal);
    let mut cabal = Cabal::open(&path, identity.clone())?;
    assert!(cabal.admit_at(&link.token, bob, "Bob", 99).is_err());
    let fresh = cabal.invite_at(identity.public_key().into(), 99)?;
    let stored_expiry: i64 =
        cabal
            .database
            .query_row("SELECT expires_at FROM invitations", [], |row| row.get(0))?;
    assert_eq!(stored_expiry, expiry + INVITATION_LIFETIME_SECONDS);
    cabal.admit_at(&fresh.token, bob, "Bob", expiry)?;
    Ok(())
}

#[test]
fn failed_clock_persistence_rolls_back_cleanup_and_grants_nothing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut cabal = owner(&directory.path().join("cabal.db"))?;
    let address: EndpointAddr = cabal.identity.public_key().into();
    let link = cabal.invite_at(address.clone(), 100)?;
    let roster = cabal.roster().hash()?;
    cabal.database.execute_batch(
        "CREATE TRIGGER reject_clock BEFORE INSERT ON metadata WHEN NEW.key = 'invitation_clock' BEGIN SELECT RAISE(ABORT, 'clock fault'); END;",
    )?;
    let expiry = 100 + INVITATION_LIFETIME_SECONDS;
    let bob = Identity::generate()?.public_key();
    assert!(cabal.admit_at(&link.token, bob, "Bob", expiry).is_err());
    assert!(cabal.invite_at(address.clone(), expiry).is_err());
    assert_eq!(count(&cabal)?, 1);
    assert_eq!(cabal.roster().hash()?, roster);
    let clock: String = cabal.database.query_row(
        "SELECT value FROM metadata WHERE key = 'invitation_clock'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(clock, "100");
    cabal.database.execute_batch("DROP TRIGGER reject_clock")?;
    cabal.invite_at(address, expiry)?;
    assert_eq!(count(&cabal)?, 1);
    assert!(cabal.admit_at(&link.token, bob, "Bob", expiry).is_err());
    Ok(())
}

#[test]
fn guessed_tokens_do_not_write_to_storage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut cabal = owner(&directory.path().join("cabal.db"))?;
    cabal.invite_at(cabal.identity.public_key().into(), 100)?;
    let writes = cabal.database.total_changes();
    let guessed = URL_SAFE_NO_PAD.encode([0_u8; 32]);
    assert!(
        cabal
            .admit_at(&guessed, Identity::generate()?.public_key(), "Mallory", 200)
            .is_err()
    );
    assert_eq!(cabal.database.total_changes(), writes);
    Ok(())
}

#[test]
fn consumed_invitation_cannot_readmit_a_revoked_device() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut cabal = owner(&directory.path().join("cabal.db"))?;
    let link = cabal.invite_at(cabal.identity.public_key().into(), 100)?;
    let bob = Identity::generate()?.public_key();
    cabal.admit_at(&link.token, bob, "Bob", 101)?;
    cabal.revoke(bob)?;
    let revoked = cabal.roster().hash()?;
    assert!(cabal.admit_at(&link.token, bob, "Bob", 102).is_err());
    assert_eq!(cabal.roster().hash()?, revoked);
    assert!(!cabal.is_member(bob));
    Ok(())
}

#[test]
fn unsupported_store_is_preserved_and_malformed_links_fail_closed() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cabal.db");
    let mut cabal = owner(&path)?;
    let identity = cabal.identity.clone();
    let mut link = cabal.invite_at(identity.public_key().into(), 100)?;
    assert_eq!(Invitation::decode(&link.encode()?)?.token, link.token);
    link.token = "!".repeat(43);
    assert!(Invitation::decode(&link.encode()?).is_err());
    let bob = Identity::generate()?.public_key();
    assert!(cabal.admit_at(&link.token, bob, "Bob", 101).is_err());
    assert!(cabal.invite_at(identity.public_key().into(), -1).is_err());
    assert!(
        cabal
            .invite_at(identity.public_key().into(), i64::MAX)
            .is_err()
    );
    assert_eq!(count(&cabal)?, 1);
    cabal.database.pragma_update(None, "user_version", 3)?;
    drop(cabal);
    let before = std::fs::read(&path)?;
    assert!(Cabal::open(&path, identity).is_err());
    assert_eq!(std::fs::read(path)?, before);
    Ok(())
}

#[tokio::test]
async fn expired_invitation_rejects_quic_join_but_saved_membership_still_syncs() -> Result<()> {
    use crate::{Network, NetworkMode};
    use std::sync::{Arc, Mutex};

    let directory = tempfile::tempdir()?;
    let mut alice = owner(&directory.path().join("alice.db"))?;
    let bob_identity = Identity::generate()?;
    let alice_network = Network::start(&alice.identity, NetworkMode::Direct {}).await?;
    let bob_network = Network::start(&bob_identity, NetworkMode::Direct {}).await?;
    // Admission happened long before this connection, and its response was
    // retained. The same token is now expired on the owner's real clock.
    let invitation = alice.invite_at(alice_network.address(), 100)?;
    let roster = alice.admit_at(&invitation.token, bob_identity.public_key(), "Bob", 101)?;
    let document = alice.create_document("garden.md", "We are still paired.")?;
    let bob = Cabal::import(&directory.path().join("bob.db"), bob_identity, roster)?;
    bob.remember_peer(&alice_network.address())?;
    let alice = Arc::new(Mutex::new(alice));
    let bob = Arc::new(Mutex::new(bob));
    alice_network.add(alice.clone())?;
    bob_network.add(bob.clone())?;
    assert!(bob_network.join(&invitation, "Bob").await.is_err());
    assert_eq!(count(&alice.lock().expect("owner"))?, 0);
    bob_network
        .sync_now(bob.clone(), alice_network.address().id)
        .await?;
    assert_eq!(
        bob.lock().expect("peer").view(document.id)?.text,
        document.text
    );
    alice_network.shutdown().await?;
    bob_network.shutdown().await?;
    Ok(())
}
