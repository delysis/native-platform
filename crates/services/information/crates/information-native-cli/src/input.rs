use crate::{CliResult, fail};
use information_native_types::{RightsStatement, UsePolicy};
use serde::de::DeserializeOwned;
use std::fs::File;
use std::io::{Read, Take};
use std::path::Path;

pub(crate) const MAX_METADATA_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> CliResult<Vec<u8>> {
    if max_bytes == 0 {
        return fail("file byte limit must be greater than zero");
    }
    let file = File::open(path).map_err(|error| {
        crate::message_error(format!("cannot open {}: {error}", path.display()))
    })?;
    let metadata = file.metadata().map_err(|error| {
        crate::message_error(format!("cannot inspect {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return fail(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > max_bytes {
        return fail(format!(
            "{} is {} bytes, exceeding the {max_bytes}-byte limit",
            path.display(),
            metadata.len()
        ));
    }

    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| crate::message_error("file byte limit overflow"))?;
    let mut reader: Take<File> = file.take(read_limit);
    let capacity = usize::try_from(metadata.len().min(max_bytes))
        .map_err(|_| crate::message_error("file length does not fit this platform"))?;
    let mut bytes = Vec::with_capacity(capacity);
    reader.read_to_end(&mut bytes).map_err(|error| {
        crate::message_error(format!("cannot read {}: {error}", path.display()))
    })?;
    let observed = u64::try_from(bytes.len())
        .map_err(|_| crate::message_error("read length does not fit a 64-bit counter"))?;
    if observed > max_bytes {
        return fail(format!(
            "{} changed while being read and exceeded the {max_bytes}-byte limit",
            path.display()
        ));
    }
    Ok(bytes)
}

pub(crate) fn read_json_bounded<T: DeserializeOwned>(
    path: &Path,
    max_bytes: u64,
    label: &'static str,
) -> CliResult<T> {
    let bytes = read_bounded(path, max_bytes)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        crate::message_error(format!(
            "{label} JSON in {} is invalid: {error}",
            path.display()
        ))
    })
}

pub(crate) fn load_rights(path: Option<&Path>) -> CliResult<Vec<RightsStatement>> {
    match path {
        Some(path) => read_json_bounded(path, MAX_METADATA_FILE_BYTES, "rights statement array"),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn load_use_policy(path: Option<&Path>) -> CliResult<UsePolicy> {
    match path {
        Some(path) => read_json_bounded(path, MAX_METADATA_FILE_BYTES, "use policy"),
        None => Ok(UsePolicy::default()),
    }
}

pub(crate) fn validate_values(
    label: &str,
    values: &[String],
    max_values: usize,
    max_value_bytes: usize,
) -> CliResult<()> {
    if values.len() > max_values {
        return fail(format!(
            "{label} contains {} values, exceeding the {max_values}-value limit",
            values.len()
        ));
    }
    for value in values {
        if value.trim().is_empty() || value.len() > max_value_bytes {
            return fail(format!(
                "{label} contains an empty value or one longer than {max_value_bytes} bytes"
            ));
        }
    }
    Ok(())
}
