//! Translate exact media at the network boundary, retaining native image/audio
//! semantics. Remote labels and filesystem paths are never model authority.
use std::io::Cursor;

use llama_native_types::{MediaInput, MediaKind};
use loom_cabal::compute::{
    ComputeFailure, ComputeInput, ComputeMedia, ComputeMediaFormat, ComputeModality, ComputeModel,
    MAX_COMPUTE_MEDIA_BYTES, MAX_COMPUTE_MEDIA_OBJECTS,
};

use super::IpcFailure;

pub(super) fn encode(
    media: &[MediaInput],
    model: &ComputeModel,
) -> Result<Vec<ComputeMedia>, IpcFailure> {
    let failure = || {
        IpcFailure::new(
            "peer_media_invalid",
            "Use up to eight retained PNG, JPEG, GIF or WAV inputs totaling eight MiB, supported by the friend's model.",
            false,
        )
    };
    if media.len() > MAX_COMPUTE_MEDIA_OBJECTS {
        return Err(failure());
    }
    let mut total = 0_usize;
    let mut result = Vec::with_capacity(media.len());
    for item in media {
        total = total.checked_add(item.bytes.len()).ok_or_else(failure)?;
        if total > MAX_COMPUTE_MEDIA_BYTES {
            return Err(failure());
        }
        let format = match (item.kind, item.mime.as_str()) {
            (MediaKind::Image, "image/png") => ComputeMediaFormat::Png,
            (MediaKind::Image, "image/jpeg") => ComputeMediaFormat::Jpeg,
            (MediaKind::Image, "image/gif") => ComputeMediaFormat::Gif,
            (MediaKind::Audio, "audio/wav") => ComputeMediaFormat::Wav,
            _ => return Err(failure()),
        };
        if !model.media.contains(&format.modality()) {
            return Err(failure());
        }
        let encoded = ComputeMedia::new(format, &item.bytes).map_err(|_| failure())?;
        if encoded.sha256 != item.sha256 {
            return Err(failure());
        }
        result.push(encoded);
    }
    Ok(result)
}

pub(super) fn decode(
    input: &ComputeInput,
    model: &ComputeModel,
) -> Result<Vec<MediaInput>, ComputeFailure> {
    input
        .validate_for_model(model)
        .map_err(|_| ComputeFailure::InputUnsupported)?;
    input
        .media
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let bytes = item
                .decode()
                .map_err(|_| ComputeFailure::InputUnsupported)?;
            let kind = match item.format.modality() {
                ComputeModality::Image => {
                    crate::attachments::validate_image_payload(&bytes, item.format.mime())
                        .map_err(|_| ComputeFailure::InputUnsupported)?;
                    MediaKind::Image
                }
                ComputeModality::Audio => {
                    validate_wav(&bytes)?;
                    MediaKind::Audio
                }
            };
            Ok(MediaInput {
                id: format!("peer-{index}-{}", item.sha256),
                kind,
                mime: item.format.mime().into(),
                sha256: item.sha256.clone(),
                bytes,
            })
        })
        .collect()
}

fn validate_wav(bytes: &[u8]) -> Result<(), ComputeFailure> {
    let invalid = |_| ComputeFailure::InputUnsupported;
    let mut reader = hound::WavReader::new(Cursor::new(bytes)).map_err(invalid)?;
    let spec = reader.spec();
    let samples = reader.len();
    if !(1..=2).contains(&spec.channels)
        || !(8_000..=192_000).contains(&spec.sample_rate)
        || samples == 0
        || u64::from(samples) > u64::from(spec.channels) * u64::from(spec.sample_rate) * 120
    {
        return Err(ComputeFailure::InputUnsupported);
    }
    let mut count = 0_u32;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                if !sample.map_err(invalid)?.is_finite() {
                    return Err(ComputeFailure::InputUnsupported);
                }
                count += 1;
            }
        }
        hound::SampleFormat::Int => {
            for sample in reader.samples::<i32>() {
                sample.map_err(invalid)?;
                count += 1;
            }
        }
    }
    if count != samples {
        return Err(ComputeFailure::InputUnsupported);
    }
    Ok(())
}
