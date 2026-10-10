//! One authenticated envelope boundary for private payloads. Ordinary manuscript
//! projection deliberately continues through the unencrypted file owner.
use std::path::Path;

use desktop_vault::ProjectVault;

use crate::Result;
use crate::file_io::{atomic_replace_private, read_bounded_no_follow};

pub(crate) fn read(
    vault: Option<&ProjectVault>,
    path: &Path,
    namespace: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    if let Some(vault) = vault {
        let envelope = read_bounded_no_follow(path, desktop_vault::encrypted_max_len(max_bytes))?;
        Ok(vault.open_bytes(namespace, &envelope, max_bytes)?)
    } else {
        read_bounded_no_follow(path, max_bytes)
    }
}

pub(crate) fn write(
    vault: Option<&ProjectVault>,
    path: &Path,
    namespace: &str,
    bytes: &[u8],
) -> Result<()> {
    if let Some(vault) = vault {
        // Encrypt before creating any temporary file: an interrupted atomic
        // write can leave only ciphertext in the private directory.
        atomic_replace_private(path, &vault.seal(namespace, bytes)?)
    } else {
        atomic_replace_private(path, bytes)
    }
}
