use std::{
    collections::BTreeSet,
    fs,
    net::{Ipv4Addr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};

use loom_cabal::{Cabal, Edit, Identity, Network, NetworkMode, Result};
use uuid::Uuid;

fn ports(network: &Network) -> BTreeSet<u16> {
    network
        .address()
        .ip_addrs()
        .map(|address| address.port())
        .collect()
}

#[tokio::test]
async fn direct_profiles_merge_offline_edits_after_both_processes_restart() -> Result<()> {
    let root = tempfile::tempdir()?;
    let alice_directory = root.path().join("alice");
    let bob_directory = root.path().join("bob");
    let alice_key = Identity::open(&alice_directory)?;
    let bob_key = Identity::open(&bob_directory)?;
    let alice_path = alice_directory.join("cabal.db");
    let bob_path = bob_directory.join("cabal.db");
    let document_id;
    let alice_ports;
    let bob_ports;
    {
        let alice_network =
            Network::start_persistent(&alice_key, NetworkMode::Direct {}, &alice_directory).await?;
        let bob_network =
            Network::start_persistent(&bob_key, NetworkMode::Direct {}, &bob_directory).await?;
        alice_ports = ports(&alice_network);
        bob_ports = ports(&bob_network);
        assert!(!alice_ports.is_empty() && !bob_ports.is_empty());
        let mut alice = Cabal::create(&alice_path, alice_key.clone(), "Garden", "Alice")?;
        document_id = alice.create_document("Garden.md", "Pond\n\nWillow\n")?.id;
        let invitation = alice.invite(alice_network.address())?;
        let alice = Arc::new(Mutex::new(alice));
        alice_network.add(alice.clone())?;
        let roster = bob_network.join(&invitation, "Bob").await?;
        let bob = Arc::new(Mutex::new(Cabal::import(
            &bob_path,
            bob_key.clone(),
            roster,
        )?));
        bob.lock()
            .expect("bob")
            .remember_peer(&alice_network.address())?;
        bob_network.add(bob.clone())?;
        bob_network
            .sync_now(bob.clone(), alice_key.public_key())
            .await?;
        assert_eq!(
            bob.lock().expect("bob").view(document_id)?.text,
            "Pond\n\nWillow\n"
        );
        alice_network.shutdown().await?;
        bob_network.shutdown().await?;
    }
    let alice = Arc::new(Mutex::new(Cabal::open(
        &alice_path,
        Identity::open(&alice_directory)?,
    )?));
    let bob = Arc::new(Mutex::new(Cabal::open(
        &bob_path,
        Identity::open(&bob_directory)?,
    )?));
    for (cabal, text) in [
        (&alice, "Pond with lilies\n\nWillow\n"),
        (&bob, "Pond\n\nWillow with lanterns\n"),
    ] {
        let mut cabal = cabal.lock().expect("cabal");
        let basis = cabal.view(document_id)?.heads;
        cabal.edit(&Edit {
            document: document_id,
            client: Uuid::new_v4(),
            basis,
            text: text.into(),
        })?;
    }
    let alice_network =
        Network::start_persistent(&alice_key, NetworkMode::Direct {}, &alice_directory).await?;
    let bob_network =
        Network::start_persistent(&bob_key, NetworkMode::Direct {}, &bob_directory).await?;
    assert_eq!(ports(&alice_network), alice_ports);
    assert_eq!(ports(&bob_network), bob_ports);
    assert_eq!(alice_network.address().relay_urls().count(), 0);
    assert_eq!(bob_network.address().relay_urls().count(), 0);
    // Only persisted peer hints are available. No new invitation, manual sync,
    // or address replacement may repair the restarted endpoints for this test.
    alice_network.add(alice.clone())?;
    bob_network.add(bob.clone())?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let expected = "Pond with lilies\n\nWillow with lanterns\n";
            let alice_text = alice.lock().expect("alice").view(document_id)?.text;
            let bob_text = bob.lock().expect("bob").view(document_id)?.text;
            if alice_text == expected && bob_text == expected {
                return Ok::<(), loom_cabal::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    alice_network.shutdown().await?;
    bob_network.shutdown().await?;
    result.expect("saved direct pairing did not reconnect")?;
    Ok(())
}

#[tokio::test]
async fn a_busy_saved_port_preserves_the_pairing_address_and_can_be_retried() -> Result<()> {
    let root = tempfile::tempdir()?;
    let identity = Identity::open(root.path())?;
    let network = Network::start_persistent(&identity, NetworkMode::Direct {}, root.path()).await?;
    let path = root.path().join("listen-ports.json");
    let saved = fs::read(&path)?;
    let value: serde_json::Value = serde_json::from_slice(&saved)?;
    let port = u16::try_from(value["ipv4"].as_u64().expect("port")).expect("u16");
    network.shutdown().await?;
    drop(network);
    let occupied = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    assert!(
        Network::start_persistent(&identity, NetworkMode::Direct {}, root.path())
            .await
            .is_err()
    );
    assert_eq!(fs::read(&path)?, saved);
    drop(occupied);
    let network = Network::start_persistent(&identity, NetworkMode::Direct {}, root.path()).await?;
    assert!(ports(&network).contains(&port));
    network.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn invalid_saved_ports_are_preserved_instead_of_replaced() -> Result<()> {
    let root = tempfile::tempdir()?;
    let identity = Identity::open(root.path())?;
    let network = Network::start_persistent(&identity, NetworkMode::Direct {}, root.path()).await?;
    network.shutdown().await?;
    let path = root.path().join("listen-ports.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let mut wrong_owner = value.clone();
    wrong_owner["device"] = serde_json::to_value(Identity::generate()?.public_key())?;
    let mut wrong_schema = value.clone();
    wrong_schema["schema"] = 2.into();
    let mut zero_port = value;
    zero_port["ipv4"] = 0.into();
    for bytes in [
        b"corrupt".to_vec(),
        vec![b'x'; 4097],
        serde_json::to_vec(&wrong_owner)?,
        serde_json::to_vec(&wrong_schema)?,
        serde_json::to_vec(&zero_port)?,
    ] {
        fs::write(&path, &bytes)?;
        assert!(
            Network::start_persistent(&identity, NetworkMode::Direct {}, root.path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path)?, bytes);
    }
    #[cfg(unix)]
    {
        let target = root.path().join("other");
        fs::write(&target, b"preserve")?;
        fs::remove_file(&path)?;
        std::os::unix::fs::symlink(&target, &path)?;
        assert!(
            Network::start_persistent(&identity, NetworkMode::Direct {}, root.path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(target)?, b"preserve");
    }
    Ok(())
}
