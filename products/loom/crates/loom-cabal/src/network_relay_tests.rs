//! Real local relay, with an explicit test CA and direct UDP disabled. This
//! proves relay routing and durable re-pairing, not Internet NAT acceptance.
use std::{
    net::Ipv4Addr,
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh_relay::{
    server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig},
    tls::CaTlsConfig,
};

use super::*;
use crate::{Cabal, Edit, Identity, Network};

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn endpoint(
    identity: &Identity,
    mode: &NetworkMode,
    tls: CaTlsConfig,
) -> TestResult<Network> {
    let endpoint = mode
        .builder()?
        .secret_key(identity.secret_key())
        .clear_ip_transports()
        .ca_tls_config(tls)
        .bind()
        .await?;
    tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await?;
    Ok(Network::attach(identity, mode.clone(), endpoint)?)
}

async fn converge(left: &Arc<Mutex<Cabal>>, right: &Arc<Mutex<Cabal>>) -> TestResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let a = left.lock().expect("left").fingerprint()?;
        let b = right.lock().expect("right").fingerprint()?;
        if a == b {
            return Ok(());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "relay peers did not converge"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn an_owned_tls_relay_resumes_saved_membership_and_offline_edits_without_another_invite()
-> TestResult {
    let (certificates, server_config) =
        iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config },
    ));
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    let server = Server::spawn(config).await?;
    let mode = NetworkMode::Relays {
        urls: vec![format!("https://{}", server.https_addr().expect("HTTPS listener")).parse()?],
    };
    let tls = CaTlsConfig::custom_roots(certificates);
    let root = tempfile::tempdir()?;
    let alice_identity = Identity::open(&root.path().join("alice-identity"))?;
    let bob_identity = Identity::open(&root.path().join("bob-identity"))?;
    let a = endpoint(&alice_identity, &mode, tls.clone()).await?;
    let b = endpoint(&bob_identity, &mode, tls.clone()).await?;
    assert_eq!(
        a.address().ip_addrs().count(),
        0,
        "test cannot bypass its relay"
    );
    let mut alice = Cabal::create(
        &root.path().join("alice.db"),
        alice_identity,
        "Garden",
        "Alice",
    )?;
    let document = alice.create_document("garden.md", "Our garden 🌱")?;
    let attachment = alice.publish_asset("shared.txt", b"Shared through our relay")?;
    let invitation = alice.invite(a.address())?;
    let alice = Arc::new(Mutex::new(alice));
    a.add(alice.clone())?;
    let roster = b.join(&invitation, "Bob").await?;
    let bob = Arc::new(Mutex::new(Cabal::import(
        &root.path().join("bob.db"),
        bob_identity,
        roster,
    )?));
    b.add(bob.clone())?;
    converge(&alice, &bob).await?;
    assert_eq!(
        bob.lock().expect("bob").asset_bytes(&attachment.sha256)?,
        b"Shared through our relay"
    );
    b.shutdown().await?;
    drop(b);
    {
        let mut value = bob.lock().expect("bob");
        let view = value.view(document.id)?;
        value.edit(&Edit {
            document: view.id,
            client: uuid::Uuid::new_v4(),
            basis: view.heads,
            text: "Our garden 🌱\nBob's offline words".into(),
        })?;
        // There is no live address or public discovery to rescue this saved
        // endpoint. The configured shared relay supplies the known identity.
        value.remember_peer(
            &EndpointAddr::new(a.address().id)
                .with_ip_addr("127.0.0.1:9".parse()?)
                .with_relay_url("https://unselected.invalid/".parse()?),
        )?;
    }
    drop(bob);
    {
        let mut value = alice.lock().expect("alice");
        let view = value.view(document.id)?;
        value.edit(&Edit {
            document: view.id,
            client: uuid::Uuid::new_v4(),
            basis: view.heads,
            text: "Our garden 🌱\nAlice's live words".into(),
        })?;
    }
    let identity = Identity::open(&root.path().join("bob-identity"))?;
    let bob = Arc::new(Mutex::new(Cabal::open(
        &root.path().join("bob.db"),
        identity.clone(),
    )?));
    let b = endpoint(&identity, &mode, tls).await?;
    b.add(bob.clone())?;
    converge(&alice, &bob).await?;
    let text = bob.lock().expect("bob").view(document.id)?.text;
    assert!(text.contains("Bob's offline words"), "{text}");
    assert!(text.contains("Alice's live words"), "{text}");
    assert_eq!(
        alice.lock().expect("alice").roster().payload.members.len(),
        2
    );
    a.shutdown().await?;
    b.shutdown().await?;
    server.shutdown().await?;
    Ok(())
}
