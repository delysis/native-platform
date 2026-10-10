//! One application settings snapshot for native and webview adapters.
//! Registered external edits retain the store's existing reconciliation rules.
use loom_config::{CONFIG_FILE, MineConfig, WorkspaceConfig};
use loom_store::{LoadedDocument, ProjectStore};
use loom_types::{ArtifactId, BlobId, DocumentId, RevisionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub enabled: bool,
    pub document_id: Option<String>,
    pub revision_id: Option<String>,
    pub source_sha256: Option<String>,
    pub suggestions: Option<bool>,
    pub model_path: Option<String>,
    pub downloads: std::collections::BTreeMap<String, loom_config::ModelDownloadConfig>,
    pub google_client_configured: bool,
    pub config: WorkspaceConfig,
    pub error: Option<String>,
}

/// Exact source captured by the same read that resolves the settings. A plain
/// dotfile has no invented document/revision identity and is never registered by
/// reading it. The generation receipt can retain these bytes independently.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigurationSource {
    pub path: String,
    pub text: String,
    pub blob_id: BlobId,
    pub document: Option<ConfigurationDocument>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigurationDocument {
    pub document_id: DocumentId,
    pub revision_id: RevisionId,
    pub artifact_id: ArtifactId,
}

impl ConfigurationSource {
    fn authored(path: &str, text: String) -> Self {
        Self {
            path: path.into(),
            blob_id: BlobId::digest(text.as_bytes()),
            text,
            document: None,
        }
    }
    fn registered(path: &str, loaded: LoadedDocument) -> Self {
        Self {
            path: path.into(),
            text: loaded.text,
            blob_id: loaded.blob_id,
            document: Some(ConfigurationDocument {
                document_id: loaded.document_id,
                revision_id: loaded.revision_id,
                artifact_id: loaded.artifact_id,
            }),
        }
    }
}

#[derive(Debug)]
pub struct Capture {
    pub snapshot: Snapshot,
    pub source: Option<ConfigurationSource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FunctionRecipe {
    pub format: loom_config::FunctionFormat,
    pub configuration: Option<ConfigurationSource>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error(transparent)]
    Store(#[from] loom_store::StoreError),
    #[error("{0}")]
    Configuration(String),
}

pub fn function_recipe(store: &mut ProjectStore) -> Result<FunctionRecipe, RecipeError> {
    let capture = capture(store)?;
    if let Some(error) = capture.snapshot.error {
        return Err(RecipeError::Configuration(error));
    }
    Ok(FunctionRecipe {
        format: capture.snapshot.config.functions.format,
        configuration: capture.source,
    })
}

fn resolved_snapshot(
    parsed: Result<MineConfig, loom_config::ConfigError>,
    root: &std::path::Path,
    document_id: Option<String>,
    revision_id: Option<String>,
) -> Snapshot {
    match parsed {
        Ok(settings) => Snapshot {
            enabled: settings.source_sha256().is_some(),
            document_id,
            revision_id,
            source_sha256: settings.source_sha256().map(str::to_owned),
            suggestions: settings.assistance.suggestions,
            model_path: settings
                .model
                .model_path(root)
                .map(|path| path.to_string_lossy().into_owned()),
            // Parsing has already validated the same immutable workspace value.
            config: settings.workspace.resolve().expect("validated workspace"),
            google_client_configured: settings.imports.google.is_configured(),
            downloads: settings.downloads,
            error: None,
        },
        Err(error) => Snapshot {
            enabled: true,
            document_id,
            revision_id,
            source_sha256: None,
            suggestions: None,
            model_path: None,
            downloads: std::collections::BTreeMap::new(),
            google_client_configured: false,
            config: WorkspaceConfig::default(),
            error: Some(error.to_string()),
        },
    }
}

pub fn read(store: &mut ProjectStore) -> Result<Snapshot, loom_store::StoreError> {
    capture(store).map(|capture| capture.snapshot)
}

pub fn capture(store: &mut ProjectStore) -> Result<Capture, loom_store::StoreError> {
    let documents = store.list_documents()?;
    let document = documents
        .iter()
        .find(|document| document.relative_path == CONFIG_FILE);
    let Some(document) = document else {
        // One immutable read supplies both parsed settings and frozen evidence.
        let (parsed, source) = match MineConfig::read_source(store.root()) {
            Ok(Some(text)) => (
                MineConfig::parse(&text),
                Some(ConfigurationSource::authored(CONFIG_FILE, text)),
            ),
            Ok(None) => (Ok(MineConfig::default()), None),
            Err(error) => (Err(error), None),
        };
        let settings = resolved_snapshot(parsed, store.root(), None, None);
        if settings.enabled {
            return Ok(Capture {
                snapshot: settings,
                source,
            });
        }
        let Some(document) = documents
            .iter()
            .find(|document| document.relative_path == ".loom.md")
        else {
            return Ok(Capture {
                snapshot: settings,
                source,
            });
        };
        store.import_external_changes_if_uncontested(
            ".loom.md",
            "Read external workspace settings",
        )?;
        let loaded = store.read_document(".loom.md")?;
        let (config, error) = match loom_config::parse_loom_workspace(&loaded.text) {
            Ok(config) => (config, None),
            Err(error) => (WorkspaceConfig::default(), Some(error)),
        };
        let snapshot = Snapshot {
            enabled: true,
            document_id: Some(document.document_id.to_string()),
            revision_id: Some(loaded.revision_id.to_string()),
            source_sha256: Some(BlobId::digest(loaded.text.as_bytes()).to_string()),
            suggestions: None,
            model_path: None,
            downloads: std::collections::BTreeMap::new(),
            google_client_configured: false,
            config,
            error,
        };
        return Ok(Capture {
            snapshot,
            source: Some(ConfigurationSource::registered(".loom.md", loaded)),
        });
    };
    store.import_external_changes_if_uncontested(CONFIG_FILE, "Read external Mine settings")?;
    let loaded = store.read_document(CONFIG_FILE)?;
    let snapshot = resolved_snapshot(
        MineConfig::parse(&loaded.text),
        store.root(),
        Some(document.document_id.to_string()),
        Some(loaded.revision_id.to_string()),
    );
    Ok(Capture {
        snapshot,
        source: Some(ConfigurationSource::registered(CONFIG_FILE, loaded)),
    })
}
