use loom_cabal::{
    ASSET_CHUNK_BYTES, AssetDescriptor, Cabal, Identity, MAX_ASSET_BYTES, Network, NetworkMode,
    Result,
};
use std::sync::{Arc, Mutex};

fn pair(path: &std::path::Path) -> Result<(Cabal, Cabal)> {
    let a = Identity::generate()?;
    let b = Identity::generate()?;
    let mut alice = Cabal::create(&path.join("alice.db"), a.clone(), "Garden", "Alice")?;
    let invitation = alice.invite(a.public_key().into())?;
    let roster = alice.admit(&invitation.token, b.public_key(), "Bob")?;
    let bob = Cabal::import(&path.join("bob.db"), b, roster)?;
    Ok((alice, bob))
}

#[test]
fn partial_download_survives_reopen_and_is_not_readable_or_relayed_until_verified() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let bytes = vec![17; ASSET_CHUNK_BYTES + 13];
    let asset = alice.publish_asset("voice.wav", &bytes)?;
    assert_eq!(bob.begin_asset(&asset)?, Some(0));
    let first = alice.asset_chunk_for(bob.identity().public_key(), &asset.sha256, 0)?;
    bob.accept_asset_chunk(&asset, 0, &first)?;
    assert!(bob.assets()?.is_empty());
    assert!(bob.asset_bytes(&asset.sha256).is_err());
    assert!(
        bob.asset_chunk_for(alice.identity().public_key(), &asset.sha256, 0)
            .is_err()
    );
    let identity = bob.identity().clone();
    drop(bob);
    let mut bob = Cabal::open(&directory.path().join("bob.db"), identity)?;
    assert_eq!(bob.begin_asset(&asset)?, Some(ASSET_CHUNK_BYTES as u64));
    assert!(!bob.accept_asset_chunk(&asset, 0, &first)?);
    let mut changed = first;
    changed[0] ^= 1;
    assert!(bob.accept_asset_chunk(&asset, 0, &changed).is_err());
    let tail = alice.asset_chunk_for(
        bob.identity().public_key(),
        &asset.sha256,
        ASSET_CHUNK_BYTES as u64,
    )?;
    bob.accept_asset_chunk(&asset, ASSET_CHUNK_BYTES as u64, &tail)?;
    assert_eq!(bob.begin_asset(&asset)?, None);
    assert_eq!(bob.asset_bytes(&asset.sha256)?, bytes);
    assert_eq!(alice.fingerprint()?, bob.fingerprint()?);
    assert_eq!(bob.publish_asset("local display alias.wav", &bytes)?, asset);
    Ok(())
}

#[test]
fn a_corrupt_transfer_discards_only_its_unverified_prefix_and_can_retry_from_another_peer()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let stable = bob.publish_asset("keep.txt", b"Keep this saved object")?;
    let bytes = vec![4; ASSET_CHUNK_BYTES + 1];
    let asset = alice.publish_asset("photo.png", &bytes)?;
    let mut corrupt = bytes[..ASSET_CHUNK_BYTES].to_vec();
    corrupt[0] ^= 1;
    bob.accept_asset_chunk(&asset, 0, &corrupt)?;
    assert!(
        bob.accept_asset_chunk(&asset, ASSET_CHUNK_BYTES as u64, &[4])
            .is_err()
    );
    assert_eq!(bob.begin_asset(&asset)?, Some(0));
    assert_eq!(bob.asset_bytes(&stable.sha256)?, b"Keep this saved object");
    for (index, chunk) in bytes.chunks(ASSET_CHUNK_BYTES).enumerate() {
        bob.accept_asset_chunk(&asset, (index * ASSET_CHUNK_BYTES) as u64, chunk)?;
    }
    assert_eq!(bob.asset_bytes(&asset.sha256)?, bytes);
    Ok(())
}

#[test]
fn pending_sizes_count_against_the_quota_and_invalid_ranges_cannot_create_holes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (_, mut bob) = pair(directory.path())?;
    let asset = AssetDescriptor {
        sha256: "a".repeat(64),
        name: "large.wav".into(),
        byte_count: MAX_ASSET_BYTES as u64,
    };
    let second = AssetDescriptor {
        sha256: "b".repeat(64),
        ..asset.clone()
    };
    assert_eq!(bob.begin_asset(&asset)?, Some(0));
    assert_eq!(bob.begin_asset(&second)?, Some(0));
    let over = AssetDescriptor {
        sha256: "c".repeat(64),
        byte_count: 1,
        ..asset.clone()
    };
    assert!(bob.begin_asset(&over).is_err());
    assert!(
        bob.begin_asset(&AssetDescriptor {
            byte_count: 1,
            ..asset.clone()
        })
        .is_err()
    );
    assert!(bob.accept_asset_chunk(&asset, 1, &[0]).is_err());
    assert!(
        bob.accept_asset_chunk(
            &asset,
            ASSET_CHUNK_BYTES as u64,
            &vec![0; ASSET_CHUNK_BYTES]
        )
        .is_err()
    );
    assert_eq!(bob.begin_asset(&asset)?, Some(0));
    assert!(
        bob.begin_asset(&AssetDescriptor {
            sha256: "../private".into(),
            ..asset
        })
        .is_err()
    );
    assert!(bob.assets()?.is_empty());
    Ok(())
}

#[test]
fn a_link_and_even_a_known_hash_never_publish_private_bytes_and_revocation_closes_serving()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, mut bob) = pair(directory.path())?;
    let private = b"Private source";
    let sha = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(private))
    };
    std::fs::write(directory.path().join("private.bin"), private)?;
    alice.create_document("attempt.md", &format!("[Media](loom-attachment:{sha})"))?;
    assert!(
        alice
            .asset_chunk_for(bob.identity().public_key(), &sha, 0)
            .is_err()
    );
    assert!(alice.assets()?.is_empty());
    let asset = alice.publish_asset("shared.txt", b"Shared source")?;
    assert!(
        alice
            .asset_chunk_for(Identity::generate()?.public_key(), &asset.sha256, 0)
            .is_err()
    );
    alice.revoke(bob.identity().public_key())?;
    bob.accept_roster(alice.roster().clone())?;
    assert!(
        alice
            .asset_chunk_for(bob.identity().public_key(), &asset.sha256, 0)
            .is_err()
    );
    assert!(bob.publish_asset("no.txt", b"no").is_err());
    assert!(bob.begin_asset(&asset).is_err());
    assert_eq!(
        std::fs::read(directory.path().join("private.bin"))?,
        private
    );
    Ok(())
}

#[tokio::test]
async fn actual_quic_carries_chunked_assets_in_the_existing_cabal_sync() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (mut alice, bob) = pair(directory.path())?;
    let bytes = vec![91; ASSET_CHUNK_BYTES + 53];
    let asset = alice.publish_asset("shared-image.png", &bytes)?;
    let a = Network::start(alice.identity(), NetworkMode::Direct {}).await?;
    let b = Network::start(bob.identity(), NetworkMode::Direct {}).await?;
    let alice_key = alice.identity().public_key();
    let alice = Arc::new(Mutex::new(alice));
    let bob = Arc::new(Mutex::new(bob));
    alice
        .lock()
        .expect("cabal owner")
        .remember_peer(&b.address())?;
    bob.lock()
        .expect("cabal owner")
        .remember_peer(&a.address())?;
    a.add(alice)?;
    // Keep the receiving cabal out of its supervisor so this test exercises
    // exactly the explicit synchronization request it is asserting about.
    b.sync_now(bob.clone(), alice_key).await?;
    assert_eq!(
        bob.lock()
            .expect("cabal owner")
            .asset_bytes(&asset.sha256)?,
        bytes
    );
    assert!(!b.sync_now(bob, alice_key).await?);
    a.shutdown().await?;
    b.shutdown().await?;
    Ok(())
}
