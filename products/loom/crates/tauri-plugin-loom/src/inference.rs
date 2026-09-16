//! Explicit, global power-user configuration. No discovery, listeners or GUI.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fte_providers::{
    HostedAuth, HostedEndpoints, HostedProtocol, HostedProviderBackend, HostedProviderConfig,
};
use fte_router::{Gateway, GatewayDefaults};
use fte_store::SecretResolver;
use fte_types::{
    BackendLocation, Modality, ModelCapabilities, ModelDescriptor, PromptForm, QuotaLimits,
    RouteTarget,
};
use loom_types::{BlobId, ModelEnvironment, ModelEnvironmentId};
use serde::{Deserialize, Serialize};

use super::IpcFailure;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const KEYCHAIN_SERVICE: &str = "org.loom.inference";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dotfile {
    version: u32,
    #[serde(default)]
    inference: InferenceConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct InferenceConfig {
    suggestions: Vec<String>,
    weave: Vec<String>,
    servers: BTreeMap<String, Server>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Server {
    /// Exact OpenAI-compatible raw /completions URL; never inferred from a host.
    endpoint: String,
    model: String,
    context_tokens: u32,
    auth: Auth,
    credential: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_seconds: u32,
    #[serde(default)]
    quota: QuotaLimits,
}

const fn default_timeout() -> u32 {
    60
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Auth {
    None,
    Bearer,
}

#[derive(Debug, Default)]
struct Keychain;

impl SecretResolver for Keychain {
    fn resolve(&self, account: &str) -> Result<Option<String>, fte_types::GatewayError> {
        let failure = || {
            fte_types::GatewayError::unavailable(
                &fte_types::RequestId("credential".into()),
                "inference_credential_unavailable",
                "the configured inference credential could not be read from the OS keychain",
            )
        };
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account).map_err(|_| failure())?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(failure()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Writer {
    pub model_id: String,
    pub completion: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Status {
    pub suggestions: Option<Writer>,
    pub weave: Option<Writer>,
}

/// Immutable per-launch authority. Scope lists and provider state stay together;
/// an operation cannot observe a mixture of configurations.
pub(super) struct Service {
    pub gateway: Arc<Gateway>,
    identity: BlobId,
    suggestions: Vec<RouteTarget>,
    weave: Vec<RouteTarget>,
    context_tokens: BTreeMap<String, u32>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceService")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(super) struct Scope {
    pub service: Arc<Service>,
    pub routes: Vec<RouteTarget>,
    pub context_tokens: u32,
    pub model_id: String,
    pub automatic: bool,
}

fn invalid(message: &str) -> IpcFailure {
    IpcFailure::new("inference_config_invalid", message, false)
}

impl InferenceConfig {
    fn validate(&self) -> Result<(), IpcFailure> {
        let config = self;
        if config.servers.len() > 16 {
            return Err(invalid("configure at most sixteen inference servers"));
        }
        for list in [&config.suggestions, &config.weave] {
            if list.len() > 16
                || list.iter().collect::<BTreeSet<_>>().len() != list.len()
                || list.iter().any(|name| !config.servers.contains_key(name))
            {
                return Err(invalid(
                    "scope routes must be distinct configured server names, at most sixteen",
                ));
            }
        }
        // Validate the entire file before constructing any provider. Merely
        // reading configuration performs no network request or credential read.
        for (name, server) in &config.servers {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || server.model.trim().is_empty()
                || server.model.len() > 256
                || !(256..=1_048_576).contains(&server.context_tokens)
                || !(1..=600).contains(&server.timeout_seconds)
            {
                return Err(invalid(
                    "invalid inference server name, model, context or timeout bound",
                ));
            }
            let url = url::Url::parse(&server.endpoint)
                .map_err(|_| invalid("invalid inference endpoint URL"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || server.endpoint.len() > 4096
            {
                return Err(invalid(
                    "inference endpoints require HTTP(S), without URL credentials, queries or fragments",
                ));
            }
            match (server.auth, server.credential.as_deref()) {
                (Auth::None, None) => {}
                (Auth::Bearer, Some(account))
                    if !account.trim().is_empty() && account.len() <= 256 => {}
                _ => {
                    return Err(invalid(
                        "auth=none forbids a credential; auth=bearer requires a keychain account",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Service {
    pub fn read(path: &Path) -> Result<Option<Arc<Self>>, IpcFailure> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        // A mistaken FIFO dotfile must not block application startup. The
        // opened handle is checked for regular-file identity immediately below.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(invalid("could not open ~/.loom.toml")),
        };
        if !file
            .metadata()
            .map_err(|_| invalid("could not inspect ~/.loom.toml"))?
            .is_file()
        {
            return Err(invalid("~/.loom.toml must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("could not read ~/.loom.toml"))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(invalid("~/.loom.toml exceeds 64 KiB"));
        }
        let secrets: Arc<dyn SecretResolver> = Arc::new(Keychain);
        Self::parse(&bytes, &secrets).map(Some)
    }

    fn parse(bytes: &[u8], secrets: &Arc<dyn SecretResolver>) -> Result<Arc<Self>, IpcFailure> {
        let source =
            std::str::from_utf8(bytes).map_err(|_| invalid("~/.loom.toml must be UTF-8"))?;
        // Parser errors may contain source lines (including accidental secrets).
        let file: Dotfile = toml::from_str(source).map_err(|error: toml::de::Error| {
            if let Some(span) = error.span()
                && let Some(prefix) = source.get(..span.start)
            {
                let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
                let column = prefix
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .count()
                    + 1;
                return invalid(&format!(
                    "invalid ~/.loom.toml schema or syntax at line {line}, column {column}"
                ));
            }
            invalid("invalid ~/.loom.toml schema or syntax")
        })?;
        if file.version != 1 {
            return Err(invalid("unsupported ~/.loom.toml version"));
        }
        let config = file.inference;
        config.validate()?;
        let identity = BlobId::digest(bytes);
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: identity.to_string(),
        }));
        for (name, server) in &config.servers {
            let descriptor = ModelDescriptor {
                id: server.model.clone(),
                aliases: vec![],
                display_name: server.model.clone(),
                backend_id: name.clone(),
                location: BackendLocation::Hosted,
                capabilities: ModelCapabilities {
                    prompt_forms: vec![PromptForm::Completion],
                    modalities: vec![Modality::Text],
                    ..Default::default()
                },
                context_tokens: Some(server.context_tokens),
                max_output_tokens: None,
                quota: server.quota,
                observed: fte_types::RouteObservations::default(),
            };
            let backend = HostedProviderBackend::new(
                HostedProviderConfig {
                    id: name.clone(),
                    display_name: name.clone(),
                    protocol: HostedProtocol::OpenAiCompatible,
                    secret_id: server.credential.clone().unwrap_or_default(),
                    auth: match server.auth {
                        Auth::None => HostedAuth::None,
                        Auth::Bearer => HostedAuth::Bearer,
                    },
                    endpoints: HostedEndpoints {
                        completions: Some(server.endpoint.clone()),
                        ..Default::default()
                    },
                    static_headers: BTreeMap::new(),
                    chat_compatibility: BTreeMap::new(),
                    models: vec![descriptor],
                    catalog_version: identity.to_string(),
                    connect_timeout: Duration::from_secs(5),
                    request_timeout: Duration::from_secs(u64::from(server.timeout_seconds)),
                },
                Arc::clone(secrets),
            )
            .map_err(|_| invalid("could not construct inference server transport"))?;
            gateway
                .register_backend(Arc::new(backend))
                .map_err(|_| invalid("could not register inference server"))?;
        }
        let routes = |names: &[String]| {
            names
                .iter()
                .map(|name| RouteTarget {
                    backend_id: name.clone(),
                    model_id: config.servers[name].model.clone(),
                })
                .collect()
        };
        Ok(Arc::new(Self {
            gateway,
            identity,
            suggestions: routes(&config.suggestions),
            weave: routes(&config.weave),
            context_tokens: config
                .servers
                .iter()
                .map(|(name, server)| (name.clone(), server.context_tokens))
                .collect(),
        }))
    }

    pub fn scope(self: &Arc<Self>, automatic: bool) -> Option<Scope> {
        let routes = if automatic {
            &self.suggestions
        } else {
            &self.weave
        };
        let context_tokens = routes
            .iter()
            .filter_map(|route| self.context_tokens.get(&route.backend_id))
            .min()
            .copied()?;
        Some(Scope {
            service: Arc::clone(self),
            routes: routes.clone(),
            context_tokens,
            automatic,
            model_id: format!(
                "loom-inference:{}:{}",
                self.identity,
                if automatic { "suggestions" } else { "weave" }
            ),
        })
    }

    pub fn status(self: &Arc<Self>) -> Status {
        let writer = |automatic| {
            self.scope(automatic).map(|scope| Writer {
                model_id: scope.model_id,
                completion: true,
            })
        };
        Status {
            suggestions: writer(true),
            weave: writer(false),
        }
    }
}

impl Scope {
    pub fn environment(&self) -> ModelEnvironment {
        ModelEnvironment {
            environment_id: ModelEnvironmentId::digest(self.model_id.as_bytes()),
            model_identifier: self.model_id.clone(),
            model_fingerprint: None,
            tokenizer_fingerprint: None,
            backend_identifier: "fte-configured-completion".into(),
            capabilities: serde_json::json!({ "configuration_sha256": self.service.identity, "ordered_routes": self.routes, "text_completion": true, "model_identity": "server_owned", "token_ids": "unavailable" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoSecrets;
    impl SecretResolver for NoSecrets {
        fn resolve(&self, _: &str) -> Result<Option<String>, fte_types::GatewayError> {
            panic!("configuration must not read credentials")
        }
    }
    fn parse(source: &str) -> Result<Arc<Service>, IpcFailure> {
        let secrets: Arc<dyn SecretResolver> = Arc::new(NoSecrets);
        Service::parse(source.as_bytes(), &secrets)
    }
    fn config(extra: &str) -> String {
        format!(
            "version = 1\n[inference]\nsuggestions = ['desk']\n[inference.servers.desk]\nendpoint = 'http://127.0.0.1:8080/v1/completions'\nmodel = 'writer'\ncontext_tokens = 4096\nauth = 'none'\n{extra}"
        )
    }
    #[test]
    fn explicit_scope_has_unknown_server_fingerprints_and_no_credential_probe() {
        let service = parse(&config("")).unwrap();
        assert!(service.scope(false).is_none());
        let scope = service.scope(true).unwrap();
        assert!(scope.environment().model_fingerprint.is_none());
        assert!(scope.environment().tokenizer_fingerprint.is_none());
        assert_eq!(scope.routes[0].backend_id, "desk");
    }
    #[test]
    fn invalid_configuration_never_broadens_authority_or_echoes_source() {
        for source in [
            config("api_key = 'private-secret'"),
            config("").replace("['desk']", "['absent']"),
            config("").replace("http://127.0.0.1", "http://user:private-secret@localhost"),
            config("").replace("auth = 'none'", "auth = 'bearer'"),
        ] {
            let error = parse(&source).unwrap_err();
            assert!(!error.message.contains("private-secret"));
        }
    }
    #[test]
    fn absent_dotfile_has_no_server_authority() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Service::read(&dir.path().join(".loom.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn omitted_or_empty_scopes_keep_native_inference() {
        for source in [
            "version = 1".to_owned(),
            "version = 1\n[inference]".to_owned(),
            "version = 1\n[inference]\nsuggestions = []\nweave = []".to_owned(),
            config("").replace("suggestions = ['desk']", "# suggestions = ['desk']"),
            config("").replace("['desk']", "[]"),
        ] {
            let service = parse(&source).unwrap();
            assert!(service.scope(true).is_none());
            assert!(service.scope(false).is_none());
            assert!(service.status().suggestions.is_none());
            assert!(service.status().weave.is_none());
        }
        assert!(parse("version = 1\n[inference]\nsuggestions = ['absent']").is_err());
    }

    #[test]
    fn manual_scope_does_not_authorize_suggestions() {
        let service = parse(&config("").replace("suggestions =", "weave =")).unwrap();
        assert!(service.scope(true).is_none());
        assert_eq!(service.scope(false).unwrap().routes[0].backend_id, "desk");
    }

    #[test]
    fn documented_toml_examples_are_valid_without_secret_reads() {
        let documentation = include_str!("../../../docs/inference-dotfile.md");
        let mut examples = 0;
        for block in documentation.split("```toml\n").skip(1) {
            let source = block.split_once("```").unwrap().0;
            let service = parse(source).unwrap();
            assert!(service.scope(true).is_some());
            examples += 1;
        }
        assert_eq!(examples, 2);
    }

    #[test]
    fn syntax_diagnostics_locate_errors_without_exposing_source() {
        let error =
            parse("version = 1\n[inference]\nprivate_secret = 'never-log-this'").unwrap_err();
        assert_eq!(
            error.message,
            "invalid ~/.loom.toml schema or syntax at line 3, column 1"
        );
    }

    #[test]
    fn typos_and_out_of_bounds_settings_fail_closed() {
        for source in [
            config("").replace("version = 1", "version = 2"),
            config("").replace("suggestions =", "suggestion ="),
            config("").replace("['desk']", "['desk', 'desk']"),
            config("timeout_seconds = 0"),
            config("timeout_seconds = 601"),
            config("").replace("context_tokens = 4096", "context_tokens = 0"),
            config("[inference.servers.desk.quota]\nrequest_per_minute = 10"),
            config("[inference.servers.desk.quota]\nrequests_per_minute = -1"),
        ] {
            assert!(parse(&source).is_err(), "invalid configuration accepted");
        }
        assert!(
            parse(&config(
                "[inference.servers.desk.quota]\nrequests_per_minute = 0"
            ))
            .is_ok()
        );
    }

    #[test]
    fn dotfile_read_enforces_size_and_regular_file_bounds() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Service::read(dir.path()).is_err());
        let path = dir.path().join(".loom.toml");
        let mut source = "version = 1\n#".to_owned();
        source.push_str(&"x".repeat(MAX_CONFIG_BYTES as usize - source.len()));
        std::fs::write(&path, &source).unwrap();
        assert!(Service::read(&path).unwrap().unwrap().scope(true).is_none());
        source.push('x');
        std::fs::write(&path, &source).unwrap();
        assert!(Service::read(&path).is_err());
    }
}
