//! Explicit connection setup. Settings reads never open client files or accounts.

use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{ConfigError, read_project_file};

const MAX_CLIENT_FILE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImportsConfig {
    pub google: GoogleImportConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoogleImportConfig {
    /// Public desktop clients without a secret can name their client ID directly.
    pub client_id: Option<String>,
    /// The downloaded Google Desktop client JSON, relative to the project root.
    /// Its credentials are read only by the explicit Connect command.
    pub client_file: Option<String>,
}

/// Intentionally not serializable: client secrets never enter settings snapshots.
pub struct GoogleDesktopClient {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize)]
struct ClientFile {
    installed: InstalledClient,
}

#[derive(Deserialize)]
struct InstalledClient {
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

impl fmt::Debug for GoogleDesktopClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GoogleDesktopClient { credentials: [redacted] }")
    }
}

impl GoogleImportConfig {
    pub fn is_configured(&self) -> bool {
        self.client_id.is_some() || self.client_file.is_some()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.client_id.is_some() && self.client_file.is_some() {
            return Err(invalid("choose client_id or client_file, not both"));
        }
        if let Some(id) = &self.client_id {
            validate_client_id(id)?;
        }
        if let Some(path) = &self.client_file {
            if path.is_empty()
                || path.len() > 1024
                || path.contains('\\')
                || path.contains(':')
                || path.chars().any(char::is_control)
                || !path.ends_with(".json")
                || Path::new(path)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(invalid(
                    "client_file must name a project-relative JSON file without parent traversal",
                ));
            }
        }
        Ok(())
    }

    /// Call only after an explicit connection request. No tokens or credentials
    /// are written here; the existing OS credential store owns the authorization.
    pub fn read_client(&self, root: &Path) -> Result<GoogleDesktopClient, ConfigError> {
        self.validate()?;
        if let Some(client_id) = &self.client_id {
            return Ok(GoogleDesktopClient {
                client_id: client_id.clone(),
                client_secret: String::new(),
            });
        }
        let path = self.client_file.as_deref().ok_or_else(|| {
            invalid("set imports.google.client_file or client_id in .mine.toml before connecting")
        })?;
        let source = read_project_file(root, Path::new(path), MAX_CLIENT_FILE_BYTES)?;
        // Google includes metadata beyond these fields. Endpoint URLs are never
        // consumed from this file; the connector owns its fixed Google endpoints.
        let file: ClientFile = serde_json::from_str(&source)
            .map_err(|_| invalid("client_file must contain a Google Desktop app JSON object with installed.client_id"))?;
        validate_client_id(&file.installed.client_id)?;
        if file.installed.client_secret.len() > 1024
            || file.installed.client_secret.chars().any(char::is_control)
        {
            return Err(invalid(
                "the desktop client secret is invalid or exceeds 1024 bytes",
            ));
        }
        Ok(GoogleDesktopClient {
            client_id: file.installed.client_id,
            client_secret: file.installed.client_secret,
        })
    }
}

fn validate_client_id(id: &str) -> Result<(), ConfigError> {
    if id.len() > 512
        || !id.ends_with(".apps.googleusercontent.com")
        || id
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(invalid(
            "client_id must be a Google Desktop app ID of at most 512 bytes without whitespace or control characters",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> ConfigError {
    ConfigError::Import(message.into())
}

#[cfg(test)]
mod tests {
    use crate::MineConfig;

    #[test]
    fn settings_reads_are_inert_and_explicit_reads_accept_only_desktop_clients() {
        let root = tempfile::tempdir().unwrap();
        let source = "[imports.google]\nclient_file='client.json'\n";
        let settings = MineConfig::parse(source).unwrap();
        assert!(settings.imports.google.is_configured());
        assert!(settings.imports.google.read_client(root.path()).is_err());
        let json = r#"{"installed":{"client_id":"desktop.apps.googleusercontent.com","client_secret":"synthetic-secret","auth_uri":"https://ignored.invalid"}}"#;
        std::fs::write(root.path().join("client.json"), json).unwrap();
        let client = settings.imports.google.read_client(root.path()).unwrap();
        assert_eq!(client.client_id, "desktop.apps.googleusercontent.com");
        assert_eq!(client.client_secret, "synthetic-secret");
        assert!(!format!("{client:?}").contains("synthetic-secret"));
        assert!(
            !serde_json::to_string(&settings)
                .unwrap()
                .contains("synthetic-secret")
        );
        std::fs::write(
            root.path().join("client.json"),
            json.replace("installed", "web"),
        )
        .unwrap();
        assert!(settings.imports.google.read_client(root.path()).is_err());
    }

    #[test]
    fn rejects_unknown_inline_secrets_ambiguous_sources_and_escaping_paths() {
        for extra in [
            "client_secret='secret'",
            "client_id='id'\nclient_file='client.json'",
            "client_file='../client.json'",
            "client_file='/tmp/client.json'",
            "client_file='client.txt'",
            "client_id=''",
            "client_file='C:\\client.json'",
        ] {
            assert!(
                MineConfig::parse(&format!("[imports.google]\n{extra}")).is_err(),
                "{extra}"
            );
        }
        let config =
            MineConfig::parse("[imports.google]\nclient_id='public.apps.googleusercontent.com'")
                .unwrap();
        assert!(
            config
                .imports
                .google
                .read_client(Path::new("/unused"))
                .unwrap()
                .client_secret
                .is_empty()
        );
    }

    #[test]
    fn bounds_client_files_and_never_echoes_malformed_secret_values() {
        let root = tempfile::tempdir().unwrap();
        let config = MineConfig::parse("[imports.google]\nclient_file='client.json'").unwrap();
        std::fs::write(
            root.path().join("client.json"),
            "x".repeat(super::MAX_CLIENT_FILE_BYTES + 1),
        )
        .unwrap();
        assert!(config.imports.google.read_client(root.path()).is_err());
        std::fs::write(
            root.path().join("client.json"),
            r#"{"installed":"synthetic-secret"}"#,
        )
        .unwrap();
        let error = config.imports.google.read_client(root.path()).unwrap_err();
        assert!(!error.to_string().contains("synthetic-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_client_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let config = MineConfig::parse("[imports.google]\nclient_file='client.json'").unwrap();
        std::fs::write(root.path().join("source.json"), "{}").unwrap();
        std::os::unix::fs::symlink("source.json", root.path().join("client.json")).unwrap();
        assert!(config.imports.google.read_client(root.path()).is_err());
    }

    use std::path::Path;
}
