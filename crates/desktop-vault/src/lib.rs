#![forbid(unsafe_code)]

//! Authenticated private payloads with one OS-protected installation key.
//! Manuscripts and authored configuration remain ordinary files. The public
//! vault header contains only a random identity and a wrapped random data key.
//! File names, lengths, and filesystem timestamps are not concealed.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
#[cfg(unix)]
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(unix)]
const HEADER_NAME: &str = "vault.json";
#[cfg(unix)]
const MAX_HEADER_BYTES: u64 = 4096;
const MAX_CACHED_PROJECTS: usize = 1024;
const MAGIC: &[u8; 8] = b"MINEENC\x01";
const NONCE_BYTES: usize = 24;
const ENVELOPE_OVERHEAD: u64 = 48;
const WRAP_DOMAIN: &[u8] = b"mine-vault-wrapped-key-v1";
const FILE_DOMAIN: &[u8] = b"mine-vault-private-file-v1";
const DATABASE_DOMAIN: &[u8] = b"mine-vault-sqlcipher-v1";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "app.delysis.mine.vault";
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "installation-key-v1";

type Result<T> = std::result::Result<T, VaultError>;
type Secret = Arc<Zeroizing<[u8; 32]>>;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Private storage is unavailable on this platform")]
    Unsupported,
    #[error(
        "The system keychain could not unlock Mine. Retry unlock explicitly; your data has not been replaced"
    )]
    Locked,
    #[error("The stored Mine key has an invalid length; it has not been replaced")]
    InvalidKey,
    #[error("Secure random key generation failed")]
    Random,
    #[error("The private payload could not be authenticated")]
    Authentication,
    #[error("The private payload exceeds its read limit")]
    Limit,
    #[error("The private storage identity changed or is invalid")]
    Identity,
    #[error("This project already has a vault")]
    AlreadyExists,
    #[error("Private storage state is unavailable")]
    State,
    #[error("Private storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u32,
    id: String,
    wrapped_key: Vec<u8>,
}

/// A clone shares zeroizing key ownership; Debug never reveals key material.
#[derive(Clone)]
pub struct ProjectVault {
    id: String,
    header_digest: [u8; 32],
    key: Secret,
}

impl fmt::Debug for ProjectVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectVault")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

static PROJECTS: OnceLock<Mutex<HashMap<PathBuf, ProjectVault>>> = OnceLock::new();
#[cfg(target_os = "macos")]
static INSTALLATION_KEY: Mutex<Option<Result<Secret>>> = Mutex::new(None);

pub const fn encrypted_max_len(plaintext_limit: u64) -> u64 {
    plaintext_limit.saturating_add(ENVELOPE_OVERHEAD)
}

impl ProjectVault {
    /// Bind an injected cipher to the exact public vault header at this root.
    pub fn validate_root(&self, root: &Path) -> Result<()> {
        let bytes = project_header(root)?.ok_or(VaultError::Identity)?;
        if self.header_digest != <[u8; 32]>::from(Sha256::digest(&bytes)) {
            return Err(VaultError::Identity);
        }
        Ok(())
    }
    /// Create a vault only as part of explicit project initialization.
    pub fn initialize(root: &Path) -> Result<Self> {
        let key = installation_key()?;
        Self::initialize_with_key(root, **key)
    }

    /// Explicit key injection for caller-owned credentials and isolated tests.
    /// No environment variable or deterministic development key is consulted.
    pub fn initialize_with_key(root: &Path, wrapping_key: [u8; 32]) -> Result<Self> {
        let wrapping_key = Zeroizing::new(wrapping_key);
        let directory = private_directory(root)?;
        let canonical = root.canonicalize()?;
        let mut projects = PROJECTS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| VaultError::State)?;
        // Reserve the bounded entry under the same lock as publication. A
        // capacity failure must precede any durable header or wrapped key.
        if projects.contains_key(&canonical) {
            return Err(VaultError::AlreadyExists);
        }
        if projects.len() >= MAX_CACHED_PROJECTS {
            return Err(VaultError::Limit);
        }
        if read_header(&directory)?.is_some() {
            return Err(VaultError::AlreadyExists);
        }
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id).map_err(|_| VaultError::Random)?;
        let id = hex(&id);
        let key = random_key()?;
        let wrapped_key = encrypt(&wrapping_key, WRAP_DOMAIN, &id, &key[..])?;
        let bytes = serde_json::to_vec(&Header {
            version: 1,
            id: id.clone(),
            wrapped_key,
        })
        .map_err(|_| VaultError::Identity)?;
        install_header(&directory, &bytes)?;
        let vault = Self {
            id,
            header_digest: Sha256::digest(&bytes).into(),
            key: Arc::new(key),
        };
        projects.insert(canonical, vault.clone());
        Ok(vault)
    }

    /// Existing ordinary projects have no header. A previously admitted vault
    /// can never downgrade to plaintext if that header is removed or replaced.
    pub fn open(root: &Path) -> Result<Option<Self>> {
        let canonical = root.canonicalize()?;
        let header = project_header(root)?;
        let cached = PROJECTS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| VaultError::State)?
            .get(&canonical)
            .cloned();
        if let Some(cached) = cached {
            let bytes = header.ok_or(VaultError::Identity)?;
            if cached.header_digest != <[u8; 32]>::from(Sha256::digest(&bytes)) {
                return Err(VaultError::Identity);
            }
            return Ok(Some(cached));
        }
        let Some(bytes) = header else {
            return Ok(None);
        };
        let key = installation_key()?;
        let vault = Self::from_header(&bytes, &key)?;
        remember(root, &vault)?;
        Ok(Some(vault))
    }

    /// Always authenticates the supplied key, including when a vault is cached.
    pub fn open_with_key(root: &Path, wrapping_key: [u8; 32]) -> Result<Option<Self>> {
        let key = Zeroizing::new(wrapping_key);
        let Some(bytes) = project_header(root)? else {
            let canonical = root.canonicalize()?;
            if PROJECTS
                .get_or_init(Mutex::default)
                .lock()
                .map_err(|_| VaultError::State)?
                .contains_key(&canonical)
            {
                return Err(VaultError::Identity);
            }
            return Ok(None);
        };
        let vault = Self::from_header(&bytes, &key)?;
        remember(root, &vault)?;
        Ok(Some(vault))
    }

    fn from_header(bytes: &[u8], key: &[u8; 32]) -> Result<Self> {
        let header: Header = serde_json::from_slice(bytes).map_err(|_| VaultError::Identity)?;
        if header.version != 1
            || header.id.len() != 32
            || !header
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(VaultError::Identity);
        }
        let plain = Zeroizing::new(decrypt(
            key,
            WRAP_DOMAIN,
            &header.id,
            &header.wrapped_key,
            32,
        )?);
        let key: [u8; 32] = plain
            .as_slice()
            .try_into()
            .map_err(|_| VaultError::InvalidKey)?;
        Ok(Self {
            id: header.id,
            header_digest: Sha256::digest(bytes).into(),
            key: Arc::new(Zeroizing::new(key)),
        })
    }

    pub fn seal(&self, namespace: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let key = self.derive(FILE_DOMAIN);
        encrypt(&key, self.id.as_bytes(), namespace, bytes)
    }

    pub fn open_bytes(&self, namespace: &str, bytes: &[u8], max: u64) -> Result<Vec<u8>> {
        let key = self.derive(FILE_DOMAIN);
        decrypt(&key, self.id.as_bytes(), namespace, bytes, max)
    }

    /// `SQLCipher` raw-key notation. The temporary string is wiped after use.
    /// Callers must not log it or enable SQL statement tracing on keyed handles.
    pub fn with_database_key<T>(&self, use_key: impl FnOnce(&str) -> T) -> T {
        let key = self.derive(DATABASE_DOMAIN);
        let mut raw = Zeroizing::new(String::with_capacity(67));
        raw.push_str("x'");
        append_hex(&mut raw, &key[..]);
        raw.push('\'');
        use_key(&raw)
    }

    fn derive(&self, domain: &[u8]) -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0_u8; 32]);
        Hkdf::<Sha256>::new(Some(self.id.as_bytes()), &self.key[..])
            .expand(domain, &mut key[..])
            .expect("32 bytes is a valid HKDF-SHA256 output");
        key
    }
}

fn remember(root: &Path, vault: &ProjectVault) -> Result<()> {
    let mut projects = PROJECTS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| VaultError::State)?;
    let root = root.canonicalize()?;
    if let Some(previous) = projects.get(&root) {
        if previous.header_digest != vault.header_digest {
            return Err(VaultError::Identity);
        }
    } else if projects.len() >= MAX_CACHED_PROJECTS {
        return Err(VaultError::Limit);
    }
    projects.insert(root, vault.clone());
    Ok(())
}

fn random_key() -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0_u8; 32]);
    getrandom::fill(&mut key[..]).map_err(|_| VaultError::Random)?;
    Ok(key)
}

fn encrypt(key: &[u8; 32], domain: &[u8], namespace: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let aad = associated_data(domain, namespace)?;
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: bytes,
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Authentication)?;
    let mut result = Vec::with_capacity(MAGIC.len() + NONCE_BYTES + ciphertext.len());
    result.extend_from_slice(MAGIC);
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

fn decrypt(
    key: &[u8; 32],
    domain: &[u8],
    namespace: &str,
    bytes: &[u8],
    max: u64,
) -> Result<Vec<u8>> {
    if bytes.len() as u64 > encrypted_max_len(max) {
        return Err(VaultError::Limit);
    }
    if bytes.len() < 48 || !bytes.starts_with(MAGIC) {
        return Err(VaultError::Authentication);
    }
    let aad = associated_data(domain, namespace)?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(&bytes[8..32]),
            Payload {
                msg: &bytes[32..],
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Authentication)
}

fn associated_data(domain: &[u8], namespace: &str) -> Result<Vec<u8>> {
    if namespace.is_empty() || namespace.len() > 4096 {
        return Err(VaultError::Identity);
    }
    let mut aad = Vec::with_capacity(domain.len() + namespace.len() + 8);
    aad.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    aad.extend_from_slice(domain);
    aad.extend_from_slice(namespace.as_bytes());
    Ok(aad)
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    append_hex(&mut result, bytes);
    result
}

fn append_hex(result: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
}

#[cfg(target_os = "macos")]
fn installation_key() -> Result<Secret> {
    cached_installation_key(&INSTALLATION_KEY, load_installation_key)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn cached_installation_key(
    cache: &Mutex<Option<Result<Secret>>>,
    load: impl FnOnce() -> Result<Secret>,
) -> Result<Secret> {
    let mut cached = cache.lock().map_err(|_| VaultError::State)?;
    if cached.is_none() {
        // Serialize the first request and remember cancellation as well as
        // success. Concurrent startup never produces a stack of OS prompts.
        *cached = Some(load());
    }
    match cached.as_ref().ok_or(VaultError::State)? {
        Ok(key) => Ok(Arc::clone(key)),
        Err(VaultError::InvalidKey) => Err(VaultError::InvalidKey),
        Err(_) => Err(VaultError::Locked),
    }
}

#[cfg(target_os = "macos")]
fn load_installation_key() -> Result<Secret> {
    use security_framework::passwords::get_generic_password;
    match get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            let key = bytes
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::InvalidKey)?;
            Ok(Arc::new(Zeroizing::new(key)))
        }
        Err(error) if error.code() == -25300 => {
            use core_foundation::data::CFData;
            use security_framework::item::{ItemAddOptions, ItemAddValue, ItemClass};
            let key = random_key()?;
            let mut item = ItemAddOptions::new(ItemAddValue::Data {
                class: ItemClass::generic_password(),
                data: CFData::from_buffer(&key[..]),
            });
            item.set_service(KEYCHAIN_SERVICE)
                .set_account_name(KEYCHAIN_ACCOUNT);
            // Add-only: a concurrent first launch may win. Never overwrite its
            // installation key or strand projects already wrapped with it.
            match item.add() {
                Ok(()) => Ok(Arc::new(key)),
                Err(error) if error.code() == -25299 => {
                    let bytes = Zeroizing::new(
                        get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
                            .map_err(|_| VaultError::Locked)?,
                    );
                    Ok(Arc::new(Zeroizing::new(
                        bytes
                            .as_slice()
                            .try_into()
                            .map_err(|_| VaultError::InvalidKey)?,
                    )))
                }
                Err(_) => Err(VaultError::Locked),
            }
        }
        Err(_) => Err(VaultError::Locked),
    }
}

#[cfg(not(target_os = "macos"))]
fn installation_key() -> Result<Secret> {
    Err(VaultError::Unsupported)
}

/// Only an explicit retry clears a cancelled/failed OS lookup. Available keys
/// remain shared by all projects and operations until the process exits.
pub fn retry_unlock() -> Result<()> {
    #[cfg(target_os = "macos")]
    clear_failed_unlock(&INSTALLATION_KEY)?;
    Ok(())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn clear_failed_unlock(cache: &Mutex<Option<Result<Secret>>>) -> Result<()> {
    let mut cached = cache.lock().map_err(|_| VaultError::State)?;
    if cached.as_ref().is_some_and(std::result::Result::is_err) {
        *cached = None;
    }
    Ok(())
}

#[cfg(unix)]
fn project_header(root: &Path) -> Result<Option<Vec<u8>>> {
    read_header(&private_directory(root)?)
}

#[cfg(not(unix))]
fn project_header(root: &Path) -> Result<Option<Vec<u8>>> {
    let directory = root.join(".loom");
    let metadata = std::fs::symlink_metadata(&directory)?;
    if metadata.is_symlink() || !metadata.is_dir() {
        return Err(VaultError::Identity);
    }
    match std::fs::symlink_metadata(directory.join("vault.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        // A marked vault is never reopened as plaintext on an unsupported host.
        Ok(_) => Err(VaultError::Unsupported),
    }
}

#[cfg(unix)]
fn private_directory(root: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = open(root, flags, Mode::empty()).map_err(std::io::Error::from)?;
    Ok(File::from(
        openat(root, ".loom", flags, Mode::empty()).map_err(std::io::Error::from)?,
    ))
}

#[cfg(unix)]
fn read_header(directory: &File) -> Result<Option<Vec<u8>>> {
    use rustix::fs::{Mode, OFlags, openat};
    let file = match openat(
        directory,
        HEADER_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    if !file.metadata()?.is_file() {
        return Err(VaultError::Identity);
    }
    let mut bytes = Vec::new();
    file.take(MAX_HEADER_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_HEADER_BYTES {
        return Err(VaultError::Limit);
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn install_header(directory: &File, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, linkat, openat, unlinkat};
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| VaultError::Random)?;
    let temporary = format!(".vault-{}.tmp", hex(&random));
    let mut file = File::from(
        openat(
            directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?,
    );
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        linkat(
            directory,
            temporary.as_str(),
            directory,
            HEADER_NAME,
            AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
        Ok::<_, std::io::Error>(())
    })();
    let _ = unlinkat(directory, temporary.as_str(), AtFlags::empty());
    result?;
    directory.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn private_directory(_: &Path) -> Result<File> {
    Err(VaultError::Unsupported)
}
#[cfg(not(unix))]
fn read_header(_: &File) -> Result<Option<Vec<u8>>> {
    Err(VaultError::Unsupported)
}
#[cfg(not(unix))]
fn install_header(_: &File, _: &[u8]) -> Result<()> {
    Err(VaultError::Unsupported)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".loom")).unwrap();
        root
    }

    #[test]
    fn authenticated_round_trip_is_random_bound_and_bounded() {
        let root = project();
        let vault = ProjectVault::initialize_with_key(root.path(), [31; 32]).unwrap();
        let secret = "Private prose\r\n尾  ".as_bytes();
        let bytes = vault.seal("blobs/value", secret).unwrap();
        assert_ne!(bytes, vault.seal("blobs/value", secret).unwrap());
        assert!(!bytes.windows(secret.len()).any(|part| part == secret));
        assert_eq!(
            vault
                .open_bytes("blobs/value", &bytes, secret.len() as u64)
                .unwrap(),
            secret
        );
        assert!(vault.open_bytes("blobs/other", &bytes, 100).is_err());
        assert!(vault.open_bytes("blobs/value", &bytes, 1).is_err());
        assert!(vault.open_bytes("blobs/value", secret, 100).is_err());
        let mut changed = bytes;
        *changed.last_mut().unwrap() ^= 1;
        assert!(vault.open_bytes("blobs/value", &changed, 100).is_err());
    }

    #[test]
    fn wrong_key_or_changed_header_never_replaces_data_or_downgrades() {
        let root = project();
        let vault = ProjectVault::initialize_with_key(root.path(), [17; 32]).unwrap();
        let path = root.path().join(".loom/vault.json");
        let original = fs::read(&path).unwrap();
        let sealed = vault.seal("receipt", b"value").unwrap();
        assert!(ProjectVault::open_with_key(root.path(), [18; 32]).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let reopened = ProjectVault::open_with_key(root.path(), [17; 32])
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.open_bytes("receipt", &sealed, 5).unwrap(),
            b"value"
        );
        fs::remove_file(&path).unwrap();
        assert!(ProjectVault::open(root.path()).is_err());
        assert!(ProjectVault::open_with_key(root.path(), [17; 32]).is_err());
    }

    #[test]
    fn projects_have_independent_keys_and_database_domain() {
        let a = project();
        let b = project();
        let a = ProjectVault::initialize_with_key(a.path(), [8; 32]).unwrap();
        let b = ProjectVault::initialize_with_key(b.path(), [8; 32]).unwrap();
        assert!(
            b.open_bytes("value", &a.seal("value", b"secret").unwrap(), 6)
                .is_err()
        );
        a.with_database_key(|key| {
            assert_eq!(key.len(), 67);
            assert!(key.starts_with("x'"));
            assert_ne!(key, format!("x'{}'", hex(&a.derive(FILE_DOMAIN)[..])));
        });
    }

    #[test]
    fn concurrent_unlocks_coalesce_and_denial_requires_explicit_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);
        let cache = Mutex::new(None);
        std::thread::scope(|threads| {
            for _ in 0..16 {
                threads.spawn(|| {
                    assert!(
                        cached_installation_key(&cache, || {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            Err(VaultError::Locked)
                        })
                        .is_err()
                    );
                });
            }
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        clear_failed_unlock(&cache).unwrap();
        cached_installation_key(&cache, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Zeroizing::new([45; 32])))
        })
        .unwrap();
        clear_failed_unlock(&cache).unwrap();
        cached_installation_key(&cache, || panic!("available keys must remain unlocked")).unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
