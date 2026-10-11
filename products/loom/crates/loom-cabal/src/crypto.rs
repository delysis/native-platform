use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

const DOMAIN: &[u8] = b"app.delysis.loom.cabal/v1\0";

#[derive(Clone)]
pub struct Identity(SecretKey);

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("public_key", &self.public_key())
            .finish()
    }
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Invalid("OS randomness unavailable"))?;
        Ok(Self(SecretKey::from_bytes(&bytes)))
    }

    /// One identity per private profile. The caller holds the profile lock.
    pub fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = directory.join("identity.key");
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(Error::Invalid("Cabal identity cannot be a symbolic link"));
        }
        match OpenOptions::new().read(true).open(&path) {
            Ok(mut file) => {
                if file.metadata()?.len() != 32 {
                    return Err(Error::Invalid("Invalid cabal identity; it was preserved"));
                }
                let mut bytes = [0; 32];
                file.read_exact(&mut bytes)?;
                Ok(Self(SecretKey::from_bytes(&bytes)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path)?;
                file.write_all(&identity.0.to_bytes())?;
                file.sync_all()?;
                Ok(identity)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.0.public()
    }
    pub(crate) fn secret_key(&self) -> SecretKey {
        self.0.clone()
    }

    pub fn sign<T: Serialize>(&self, payload: T) -> Result<Signed<T>> {
        let bytes = signing_bytes(&payload)?;
        Ok(Signed {
            payload,
            signer: self.public_key(),
            signature: URL_SAFE_NO_PAD.encode(self.0.sign(&bytes).to_bytes()),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed<T> {
    pub payload: T,
    pub signer: PublicKey,
    pub signature: String,
}

impl<T: Serialize> Signed<T> {
    pub fn verify(&self) -> Result<()> {
        let bytes: [u8; 64] = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .map_err(|_| Error::Invalid("Invalid cabal signature"))?
            .try_into()
            .map_err(|_| Error::Invalid("Invalid cabal signature"))?;
        self.signer
            .verify(
                &signing_bytes(&self.payload)?,
                &Signature::from_bytes(&bytes),
            )
            .map_err(|_| Error::Invalid("Cabal signature did not verify"))
    }

    pub fn hash(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }
}

fn signing_bytes<T: Serialize>(payload: &T) -> Result<Vec<u8>> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(payload)?);
    Ok(bytes)
}
