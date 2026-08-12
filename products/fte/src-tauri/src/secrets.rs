//! OS-backed provider credential authority.

use keyring::{Entry, Error as KeyringError};
use std::fmt;

const SERVICE: &str = "dev.delysis.free-token-energy.provider";

pub trait CredentialStore: Send + Sync {
    fn write(&self, provider_id: &str, secret: &[u8]) -> Result<(), SecretStoreError>;
    fn read(&self, provider_id: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    fn delete(&self, provider_id: &str) -> Result<bool, SecretStoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStoreError {
    detail: String,
}

impl SecretStoreError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for SecretStoreError {}

#[derive(Debug, Default)]
pub struct OsCredentialStore;

impl OsCredentialStore {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn entry(provider_id: &str) -> Result<Entry, SecretStoreError> {
        validate_provider_id(provider_id)?;
        Entry::new(SERVICE, provider_id).map_err(|error| keyring_error("open", error))
    }
}

impl CredentialStore for OsCredentialStore {
    fn write(&self, provider_id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        if secret.is_empty() {
            return Err(SecretStoreError::new("provider secret must not be empty"));
        }
        Self::entry(provider_id)?
            .set_secret(secret)
            .map_err(|error| keyring_error("write", error))
    }

    fn read(&self, provider_id: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        match Self::entry(provider_id)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error("read", error)),
        }
    }

    fn delete(&self, provider_id: &str) -> Result<bool, SecretStoreError> {
        match Self::entry(provider_id)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(keyring_error("delete", error)),
        }
    }
}

fn validate_provider_id(provider_id: &str) -> Result<(), SecretStoreError> {
    if provider_id.is_empty()
        || provider_id.len() > 128
        || !provider_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
        })
    {
        return Err(SecretStoreError::new(
            "provider ID is invalid for secret storage",
        ));
    }
    Ok(())
}

fn keyring_error(action: &str, error: KeyringError) -> SecretStoreError {
    SecretStoreError::new(format!("OS credential {action} failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::validate_provider_id;

    #[test]
    fn provider_ids_are_bounded_and_namespace_safe() {
        assert!(validate_provider_id("openrouter").is_ok());
        assert!(validate_provider_id("local.native-v2").is_ok());
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id("../escape").is_err());
        assert!(validate_provider_id("MixedCase").is_err());
    }
}
