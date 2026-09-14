//! Authored download definitions are inert until an explicit application command.

use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_NAMED_DOWNLOADS: usize = 32;
const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_URL_BYTES: usize = 16 * 1024;

const fn default_max_bytes() -> u64 {
    64 * 1024 * 1024 * 1024
}

/// One exact GGUF or projector download into the existing local model library.
/// This is neither a catalog identity nor permission to load the downloaded file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDownloadConfig {
    pub url: String,
    pub file_name: String,
    pub sha256: String,
    #[serde(default)]
    pub expected_bytes: Option<u64>,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}

impl ModelDownloadConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.url.len() > MAX_URL_BYTES
            || self.url.trim() != self.url
            || self.url.chars().any(char::is_control)
        {
            return Err("url must fit 16 KiB without surrounding whitespace or control characters");
        }
        let url = Url::parse(&self.url).map_err(|_| "url must be a complete HTTPS URL")?;
        if url.scheme() != "https" || url.host_str().is_none() {
            return Err("url must be a complete HTTPS URL");
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("url must not contain credentials");
        }
        if url.as_str().len() > MAX_URL_BYTES {
            return Err("encoded url must fit 16 KiB");
        }
        validate_file_name(&self.file_name)?;
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("sha256 must contain the publisher's exact 64 hexadecimal digits");
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_DOWNLOAD_BYTES {
            return Err("max_bytes must be between 1 byte and 1 TiB");
        }
        if self
            .expected_bytes
            .is_some_and(|bytes| bytes == 0 || bytes > self.max_bytes)
        {
            return Err("expected_bytes must be positive and no larger than max_bytes");
        }
        Ok(())
    }
}

fn validate_file_name(name: &str) -> Result<(), &'static str> {
    let stem = name.get(..name.len().saturating_sub(5)).unwrap_or_default();
    if name.len() > 240
        || stem.is_empty()
        || !name
            .get(name.len().saturating_sub(5)..)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(".gguf"))
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err("file_name must be one portable GGUF file name, at most 240 UTF-8 bytes");
    }
    let device = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device
            .strip_prefix("COM")
            .or_else(|| device.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    {
        return Err("file_name must not use a reserved Windows device name");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::MineConfig;

    fn definition(extra: &str) -> String {
        format!(
            "[downloads.writer]\nurl = 'https://models.example/writer.gguf?download=1'\nfile_name = 'Writer Q8.gguf'\nsha256 = '{}'\n{extra}",
            "AB".repeat(32)
        )
    }

    #[test]
    fn definitions_preserve_authored_identity_and_use_a_bounded_default() {
        let source = definition("expected_bytes = 4_954_576_032");
        let settings = MineConfig::parse(&source).unwrap();
        let download = &settings.downloads["writer"];
        assert_eq!(
            download.url,
            "https://models.example/writer.gguf?download=1"
        );
        assert_eq!(download.file_name, "Writer Q8.gguf");
        assert_eq!(download.sha256, "AB".repeat(32));
        assert_eq!(download.expected_bytes, Some(4_954_576_032));
        assert_eq!(download.max_bytes, 64 * 1024 * 1024 * 1024);
        assert!(MineConfig::parse(&definition("max_bytes = 1")).is_ok());
    }

    #[test]
    fn invalid_definitions_fail_before_any_command_exists() {
        for extra in [
            "max_bytes = 0",
            "max_bytes = 1_099_511_627_777",
            "expected_bytes = 0",
            "max_bytes = 1\nexpected_bytes = 2",
            "auto_start = true",
        ] {
            assert!(MineConfig::parse(&definition(extra)).is_err(), "{extra}");
        }
        for url in [
            "http://models.example/writer.gguf",
            "https://secret@models.example/writer.gguf",
            " https://models.example/writer.gguf",
            "https://",
        ] {
            assert!(
                MineConfig::parse(
                    &definition("").replace("https://models.example/writer.gguf?download=1", url)
                )
                .is_err(),
                "{url}"
            );
        }
        for name in [
            "../writer.gguf",
            "CON.gguf",
            "lpt9.part.gguf",
            ".gguf",
            "bad:writer.gguf",
            "writer.gguf ",
            "writer.bin",
            "a\u{7f}.gguf",
        ] {
            assert!(
                MineConfig::parse(&definition("").replace("Writer Q8.gguf", name)).is_err(),
                "{name}"
            );
        }
        assert!(MineConfig::parse(&definition("").replace(&"AB".repeat(32), "abcd")).is_err());
        assert!(
            MineConfig::parse(
                &definition("").replace("Writer Q8.gguf", &format!("{}.gguf", "é".repeat(119)))
            )
            .is_err()
        );
        assert!(
            MineConfig::parse(
                &definition("").replace("downloads.writer", "downloads.'not a name'")
            )
            .is_err()
        );
        assert!(
            MineConfig::parse(&definition("").replace("file_name = 'Writer Q8.gguf'\n", ""))
                .is_err()
        );
    }

    #[test]
    fn a_bounded_number_of_downloads_is_supported() {
        let mut source = String::new();
        for index in 0..super::MAX_NAMED_DOWNLOADS {
            source.push_str(
                &definition("").replace("downloads.writer", &format!("downloads.writer_{index}")),
            );
        }
        assert!(MineConfig::parse(&source).is_ok());
        source.push_str(&definition(""));
        assert!(MineConfig::parse(&source).is_err());
    }
}
