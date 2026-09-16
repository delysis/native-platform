//! Device-owned connection settings. A peer cannot change this policy through
//! a workspace document or a remembered endpoint address.
use std::collections::BTreeSet;

use iroh::{
    EndpointAddr, RelayMode, RelayUrl, TransportAddr,
    endpoint::{Builder, presets},
};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkMode {
    /// Public n0 discovery and community relays, plus direct peer connections.
    Internet {},
    /// Direct addresses only; no public discovery or relay connections.
    Direct {},
    /// Explicit home relays, without public discovery or community fallback.
    Relays { urls: Vec<RelayUrl> },
}

impl Default for NetworkMode {
    fn default() -> Self {
        Self::Internet {}
    }
}

impl NetworkMode {
    pub fn validate(&self) -> Result<()> {
        if let Self::Relays { urls } = self {
            let unique: BTreeSet<_> = urls.iter().collect();
            if urls.is_empty() || urls.len() > 4 || unique.len() != urls.len() {
                return Err(Error::Invalid("Choose one to four distinct relay URLs"));
            }
            for url in urls {
                if url.as_str().len() > 512
                    || url.scheme() != "https"
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || url.port() == Some(0)
                {
                    return Err(Error::Invalid(
                        "Relay URLs need an HTTPS host, with no credentials, path, query, or fragment",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn builder(&self) -> Result<Builder> {
        self.validate()?;
        Ok(match self {
            Self::Internet {} => iroh::Endpoint::builder(presets::N0),
            Self::Direct {} => {
                iroh::Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
            }
            Self::Relays { urls } => iroh::Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::custom(urls.clone())),
        })
    }

    /// Remembered addresses retain identity but cannot select a different relay
    /// policy. Common configured relays also locate a known device after either
    /// endpoint restarts and its saved IP/port has changed, without public DNS.
    pub(crate) fn address(&self, mut address: EndpointAddr) -> EndpointAddr {
        match self {
            Self::Internet {} => {}
            Self::Direct {} => address.addrs.retain(TransportAddr::is_ip),
            Self::Relays { urls } => {
                address.addrs.retain(TransportAddr::is_ip);
                address
                    .addrs
                    .extend(urls.iter().cloned().map(TransportAddr::Relay));
            }
        }
        address
    }
}

#[cfg(test)]
#[path = "network_relay_tests.rs"]
mod relay_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_settings_reject_ambient_authority_and_malformed_targets() {
        for value in [
            "http://relay.example",
            "https://me:secret@relay.example",
            "https://relay.example/other",
            "https://relay.example/?key=value",
            "https://relay.example/#fragment",
            "https://relay.example:0/",
        ] {
            let mode = NetworkMode::Relays {
                urls: vec![value.parse().expect("URL")],
            };
            assert!(mode.validate().is_err(), "accepted {value}");
        }
        assert!(NetworkMode::Relays { urls: vec![] }.validate().is_err());
        assert!(
            serde_json::from_str::<NetworkMode>(
                r#"{"mode":"direct","urls":["https://relay.example"]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn custom_and_direct_addresses_never_reuse_a_peers_other_relay_hint() {
        let identity = crate::Identity::generate().expect("identity");
        let untrusted = identity.public_key().into();
        let untrusted =
            EndpointAddr::with_relay_url(untrusted, "https://other.example".parse().expect("URL"))
                .with_ip_addr("127.0.0.1:3001".parse().expect("address"));
        let mode = NetworkMode::Relays {
            urls: vec!["https://our.example".parse().expect("URL")],
        };
        mode.validate().expect("valid config");
        let normalized = mode.address(untrusted.clone());
        assert_eq!(normalized.id, identity.public_key());
        assert_eq!(normalized.ip_addrs().count(), 1);
        assert_eq!(
            normalized
                .relay_urls()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["https://our.example/"]
        );
        assert_eq!(
            NetworkMode::Direct {}
                .address(untrusted.clone())
                .relay_urls()
                .count(),
            0
        );
        assert_eq!(
            NetworkMode::Internet {}.address(untrusted.clone()),
            untrusted
        );
        assert_eq!(
            mode.address(identity.public_key().into())
                .relay_urls()
                .count(),
            1
        );
    }
}
