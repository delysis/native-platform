//! Direct connections retain their local ports across process restarts.
//! This is reachability state, never membership or discovery authority.
use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr},
    num::NonZeroU16,
    path::Path,
};

use iroh::{Endpoint, PublicKey, endpoint::BindOpts};
use serde::{Deserialize, Serialize};

use crate::{Error, Identity, NetworkMode, Result};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ports {
    schema: u32,
    device: PublicKey,
    ipv4: NonZeroU16,
    ipv6: Option<NonZeroU16>,
}

fn read(path: &Path, identity: &Identity) -> Result<Option<Ports>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err(Error::Invalid(
            "Invalid cabal listen ports; the file was preserved",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::Invalid("Cabal listen ports exceed their limit"));
    }
    let ports: Ports = serde_json::from_slice(&bytes)?;
    if ports.schema != 1 || ports.device != identity.public_key() {
        return Err(Error::Invalid(
            "Cabal listen ports do not belong to this device",
        ));
    }
    Ok(Some(ports))
}

pub(super) async fn bind(identity: &Identity, directory: &Path) -> Result<Endpoint> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Invalid("Cabal profile is not an ordinary directory"));
    }
    let path = directory.join("listen-ports.json");
    let saved = read(&path, identity)?;
    let builder = NetworkMode::Direct {}.builder()?;
    let builder = if let Some(ports) = &saved {
        builder
            .bind_addr((Ipv4Addr::UNSPECIFIED, ports.ipv4.get()))
            .map_err(super::network_error)?
            .bind_addr_with_opts(
                (Ipv6Addr::UNSPECIFIED, ports.ipv6.map_or(0, NonZeroU16::get)),
                BindOpts::default().set_is_required(false),
            )
            .map_err(super::network_error)?
    } else {
        builder
    };
    // A busy saved IPv4 port is an explicit failure. Silently selecting another
    // port would strand every offline peer that only knows the saved address.
    let endpoint = builder
        .secret_key(identity.secret_key())
        .bind()
        .await
        .map_err(super::network_error)?;
    let result = (|| {
        let sockets = endpoint.bound_sockets();
        let ipv4 = sockets
            .iter()
            .find(|address| address.is_ipv4())
            .and_then(|address| NonZeroU16::new(address.port()))
            .ok_or(Error::Invalid("Cabal endpoint has no IPv4 listen port"))?;
        let ipv6 = sockets
            .iter()
            .find(|address| address.is_ipv6())
            .and_then(|address| NonZeroU16::new(address.port()));
        // Preserve the IPv6 port if that family is temporarily unavailable.
        let ipv6 = saved.as_ref().and_then(|ports| ports.ipv6).or(ipv6);
        if saved
            .as_ref()
            .is_some_and(|ports| ports.ipv4 == ipv4 && ports.ipv6 == ipv6)
        {
            return Ok(());
        }
        let mut file = atomic_write_file::AtomicWriteFile::open(&path)?;
        file.write_all(&serde_json::to_vec(&Ports {
            schema: 1,
            device: identity.public_key(),
            ipv4,
            ipv6,
        })?)?;
        file.commit()?;
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        endpoint.close().await;
        return Err(error);
    }
    Ok(endpoint)
}
