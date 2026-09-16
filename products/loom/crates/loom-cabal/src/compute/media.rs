//! Bounded, immutable model input. Labels, paths and document authority stay
//! on the requesting device; the host receives only format and exact bytes.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{Error, Result};

pub const MAX_COMPUTE_MEDIA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_COMPUTE_MEDIA_OBJECTS: usize = 8;
pub const MAX_COMPUTE_FRAME_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeModality {
    Image,
    Audio,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeMediaFormat {
    Png,
    Jpeg,
    Gif,
    Wav,
}

impl ComputeMediaFormat {
    pub fn modality(self) -> ComputeModality {
        match self {
            Self::Png | Self::Jpeg | Self::Gif => ComputeModality::Image,
            Self::Wav => ComputeModality::Audio,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Wav => "audio/wav",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeMedia {
    pub format: ComputeMediaFormat,
    pub sha256: String,
    /// Canonical standard base64. Bounded before decoding or persistence.
    pub bytes: String,
}

impl ComputeMedia {
    pub fn new(format: ComputeMediaFormat, bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_COMPUTE_MEDIA_BYTES {
            return Err(limit());
        }
        Ok(Self {
            format,
            sha256: hex::encode(Sha256::digest(bytes)),
            bytes: STANDARD.encode(bytes),
        })
    }

    pub fn decode(&self) -> Result<Vec<u8>> {
        if self.bytes.len() > MAX_COMPUTE_MEDIA_BYTES.div_ceil(3) * 4 {
            return Err(limit());
        }
        let bytes = STANDARD
            .decode(&self.bytes)
            .map_err(|_| Error::Invalid("Invalid compute media encoding"))?;
        if bytes.is_empty() || bytes.len() > MAX_COMPUTE_MEDIA_BYTES {
            return Err(limit());
        }
        if hex::encode(Sha256::digest(&bytes)) != self.sha256 {
            return Err(Error::Invalid(
                "Compute media digest does not match its bytes",
            ));
        }
        Ok(bytes)
    }
}

pub(super) fn validate(media: &[ComputeMedia]) -> Result<()> {
    if media.len() > MAX_COMPUTE_MEDIA_OBJECTS {
        return Err(limit());
    }
    let encoded = media.iter().try_fold(0_usize, |total, item| {
        total.checked_add(item.bytes.len()).ok_or_else(limit)
    })?;
    // Account for per-object padding, then enforce the exact decoded bound.
    if encoded > MAX_COMPUTE_MEDIA_BYTES.div_ceil(3) * 4 + media.len() * 4 {
        return Err(limit());
    }
    let mut total = 0_usize;
    for item in media {
        total += item.decode()?.len();
        if total > MAX_COMPUTE_MEDIA_BYTES {
            return Err(limit());
        }
    }
    Ok(())
}

fn limit() -> Error {
    Error::Invalid("Peer jobs accept at most eight image/audio inputs totaling eight MiB")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::ComputeInput;

    #[test]
    fn immutable_media_binds_bytes_format_order_and_declared_modality() -> Result<()> {
        let image = ComputeMedia::new(ComputeMediaFormat::Png, b"wire fixture")?;
        let audio = ComputeMedia::new(ComputeMediaFormat::Wav, b"audio fixture")?;
        let input = ComputeInput {
            prompt: "Describe".into(),
            max_output_tokens: 16,
            seed: 7,
            media: vec![image.clone(), audio],
        };
        let grant = uuid::Uuid::new_v4();
        let fingerprint = input.fingerprint(grant)?;
        let mut changed = input.clone();
        changed.media.reverse();
        assert_ne!(changed.fingerprint(grant)?, fingerprint);
        changed = input.clone();
        changed.media[0].format = ComputeMediaFormat::Jpeg;
        assert_ne!(changed.fingerprint(grant)?, fingerprint);
        changed = input;
        changed.media[0].bytes = STANDARD.encode(b"substituted");
        assert!(changed.fingerprint(grant).is_err());
        let mut corrupt = image;
        corrupt.bytes = "d2lyZSBmaXh0dXJl=".into();
        assert!(corrupt.decode().is_err(), "noncanonical padding");
        assert!(
            serde_json::from_str::<ComputeMedia>(
                r#"{"format":"png","sha256":"a","bytes":"YQ==","path":"private"}"#
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn media_limits_apply_before_decode_and_across_objects() -> Result<()> {
        let item = ComputeMedia::new(ComputeMediaFormat::Wav, b"bounded")?;
        assert!(validate(&vec![item.clone(); MAX_COMPUTE_MEDIA_OBJECTS + 1]).is_err());
        let mut oversized = item;
        oversized.bytes = "!".repeat(MAX_COMPUTE_MEDIA_BYTES.div_ceil(3) * 4 + 1);
        assert!(oversized.decode().is_err());
        let half = ComputeMedia::new(
            ComputeMediaFormat::Wav,
            &vec![7; MAX_COMPUTE_MEDIA_BYTES / 2 + 1],
        )?;
        assert!(validate(&[half.clone(), half]).is_err());
        Ok(())
    }
}
