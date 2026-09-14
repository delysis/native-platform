//! Optional workspace layout, authored as an ordinary Markdown document.
//! Parsing describes panes and model identity; it never resolves files or invokes a model.

use super::*;

const TEMPLATE_PATH: &str = ".loom.md";
const MAX_TEMPLATE_BYTES: usize = 65_536;
const MAX_PANES: usize = 8;

const DEFAULT_TEMPLATE: &str = r##"# Workspace

Uncomment a setting to change it. Other settings keep their defaults.
`@document` means the document currently being edited. Name another document
with `@Name` or `@"A name with spaces"`; a trailing `/` names a directory.
Pane documents are ordinary Markdown, too. Opening a pane never runs a prompt.
Model selection names an installed catalog model or build-policy profile;
choose only one. Leaving it commented keeps the usual local default.

```loom-workspace
# [model]
# catalog = "google.gemma-4-12b-it-qat-q4_0"
# Or replace catalog with a build-policy profile:
# profile = "gemma_4_e2b_base_q8_loom_v1"

# [theme]
# mode = "system"
# canvas = "#faf9f6"
# text = "#242424"
# accent = "#625bd6"

# [panes.writing]
# kind = "editor"
# position = "main"
# visible = true

# [panes.chat]
# kind = "chat"
# position = "right"
# visible = true
# context = ["@document"]
# document = "@.chat"

# [panes.terminal]
# kind = "terminal"
# position = "bottom"
# visible = true

# [panes.browser]
# kind = "browser"
# position = "bottom"
# visible = true
# document = "@.browser"
```
"##;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PaneKind {
    Editor,
    Chat,
    Terminal,
    Browser,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PanePosition {
    Main,
    Right,
    Bottom,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct PaneConfig {
    kind: PaneKind,
    position: PanePosition,
    visible: bool,
    title: Option<String>,
    document: Option<String>,
    context: Vec<String>,
}

impl PaneConfig {
    fn new(kind: PaneKind) -> Self {
        Self {
            kind,
            position: match kind {
                PaneKind::Editor => PanePosition::Main,
                PaneKind::Chat => PanePosition::Right,
                PaneKind::Terminal | PaneKind::Browser => PanePosition::Bottom,
            },
            visible: true,
            title: None,
            document: None,
            context: if kind == PaneKind::Chat {
                vec!["@document".into()]
            } else {
                Vec::new()
            },
        }
    }
}

/// A name selects existing native authority; it cannot grant authority to a
/// path, URL, arbitrary model digest, or download operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkspaceModel {
    Catalog(String),
    Profile(String),
}

impl WorkspaceModel {
    fn validate(&self) -> Result<(), String> {
        let (Self::Catalog(id) | Self::Profile(id)) = self;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(
                "Model names use up to 128 letters, digits, dots, hyphens, or underscores.".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkspaceThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WorkspaceTheme {
    mode: WorkspaceThemeMode,
    canvas: Option<String>,
    text: Option<String>,
    accent: Option<String>,
}

impl WorkspaceTheme {
    fn validate(&self) -> Result<(), String> {
        for color in [&self.canvas, &self.text, &self.accent]
            .into_iter()
            .flatten()
        {
            if color.len() != 7
                || !color.starts_with('#')
                || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            {
                return Err("Theme colors use exactly #RRGGBB hexadecimal values.".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct WorkspaceConfig {
    model: Option<WorkspaceModel>,
    theme: WorkspaceTheme,
    panes: BTreeMap<String, PaneConfig>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            model: None,
            theme: WorkspaceTheme::default(),
            panes: [
                ("writing", PaneKind::Editor),
                ("chat", PaneKind::Chat),
                ("terminal", PaneKind::Terminal),
                ("browser", PaneKind::Browser),
            ]
            .into_iter()
            .map(|(name, kind)| (name.into(), PaneConfig::new(kind)))
            .collect(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WorkspaceOverrides {
    model: Option<WorkspaceModel>,
    theme: WorkspaceTheme,
    panes: BTreeMap<String, PaneOverrides>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PaneOverrides {
    kind: Option<PaneKind>,
    position: Option<PanePosition>,
    visible: Option<bool>,
    title: Option<String>,
    document: Option<String>,
    context: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceTemplateSnapshot {
    enabled: bool,
    document_id: Option<String>,
    revision_id: Option<String>,
    config: WorkspaceConfig,
    error: Option<String>,
}

/// Pane IDs are durable presentation identities, not filesystem paths.
pub(super) fn valid_pane_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_reference(reference: &str) -> Result<(), String> {
    if reference.len() > 1024 {
        return Err("A pane document reference is too long.".into());
    }
    let parsed =
        loom_document::document_references(reference).map_err(|error| error.to_string())?;
    if parsed.len() != 1 || parsed[0].range != (0..reference.len()) {
        return Err("Pane documents and context use one @document reference per entry.".into());
    }
    Ok(())
}

fn parse_config(markdown: &str) -> Result<WorkspaceConfig, String> {
    if markdown.len() > MAX_TEMPLATE_BYTES {
        return Err("The workspace template exceeds 64 KiB.".into());
    }
    let overrides: WorkspaceOverrides = toml::from_str(config_fence(markdown)?)
        .map_err(|error| format!("Workspace settings: {error}"))?;
    let mut config = WorkspaceConfig::default();
    if let Some(model) = overrides.model {
        model.validate()?;
        config.model = Some(model);
    }
    overrides.theme.validate()?;
    config.theme = overrides.theme;
    for (name, pane) in overrides.panes {
        if !valid_pane_id(&name) {
            return Err("Pane names use up to 64 letters, digits, hyphens, or underscores.".into());
        }
        let mut resolved = config
            .panes
            .remove(&name)
            .unwrap_or_else(|| PaneConfig::new(pane.kind.unwrap_or(PaneKind::Editor)));
        if pane.kind.is_none()
            && !matches!(name.as_str(), "writing" | "chat" | "terminal" | "browser")
        {
            return Err(format!("Pane {name} needs a kind."));
        }
        if let Some(kind) = pane.kind
            && kind != resolved.kind
        {
            resolved = PaneConfig::new(kind);
        }
        if let Some(position) = pane.position {
            resolved.position = position;
        }
        if let Some(visible) = pane.visible {
            resolved.visible = visible;
        }
        if let Some(title) = pane.title {
            if title.len() > 128 || title.contains(['\r', '\n']) {
                return Err(format!(
                    "Pane {name} needs a single-line title of at most 128 bytes."
                ));
            }
            resolved.title = Some(title);
        }
        if let Some(document) = pane.document {
            validate_reference(&document)?;
            resolved.document = Some(document);
        }
        if let Some(context) = pane.context {
            if context.len() > 16 {
                return Err(format!("Pane {name} has more than 16 context references."));
            }
            for reference in &context {
                validate_reference(reference)?;
            }
            resolved.context = context;
        }
        config.panes.insert(name, resolved);
        if config.panes.len() > MAX_PANES {
            return Err("A workspace supports at most eight panes.".into());
        }
    }
    let mut main = config
        .panes
        .values()
        .filter(|pane| pane.visible && pane.position == PanePosition::Main);
    if !matches!(main.next(), Some(pane) if pane.kind == PaneKind::Editor) || main.next().is_some()
    {
        return Err("Keep exactly one visible editor in the main position.".into());
    }
    Ok(config)
}

/// Ignore configuration-looking text inside other Markdown fences. Only one
/// complete, top-level `loom-workspace` fence is authoritative.
fn config_fence(markdown: &str) -> Result<&str, String> {
    let mut open: Option<(u8, usize, bool, usize)> = None;
    let mut found = None;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if indent <= 3 {
            let marker = trimmed.as_bytes().first().copied().unwrap_or_default();
            let count = trimmed.bytes().take_while(|byte| *byte == marker).count();
            if matches!(marker, b'`' | b'~') && count >= 3 {
                let suffix = trimmed[count..].trim();
                if let Some((opening_marker, opening_count, selected, start)) = open {
                    if marker == opening_marker && count >= opening_count && suffix.is_empty() {
                        if selected {
                            found = Some(&markdown[start..offset]);
                        }
                        open = None;
                    }
                } else {
                    let selected = suffix == "loom-workspace";
                    if selected && found.is_some() {
                        return Err("Use one loom-workspace settings fence.".into());
                    }
                    open = Some((marker, count, selected, offset + line.len()));
                }
            }
        }
        offset += line.len();
    }
    if matches!(open, Some((_, _, true, _))) {
        return Err("Close the loom-workspace settings fence.".into());
    }
    Ok(found.unwrap_or(""))
}

fn snapshot(store: &mut ProjectStore) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let document = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .find(|document| document.relative_path == TEMPLATE_PATH);
    let Some(document) = document else {
        return Ok(WorkspaceTemplateSnapshot {
            enabled: false,
            document_id: None,
            revision_id: None,
            config: WorkspaceConfig::default(),
            error: None,
        });
    };
    store
        .import_external_changes_if_uncontested(TEMPLATE_PATH, "Read external workspace settings")
        .map_err(IpcFailure::store)?;
    let loaded = store
        .read_document(TEMPLATE_PATH)
        .map_err(IpcFailure::store)?;
    let (config, error) = match parse_config(&loaded.text) {
        Ok(config) => (config, None),
        Err(error) => (WorkspaceConfig::default(), Some(error)),
    };
    Ok(WorkspaceTemplateSnapshot {
        enabled: true,
        document_id: Some(document.document_id.to_string()),
        revision_id: Some(loaded.revision_id.to_string()),
        config,
        error,
    })
}

fn enable(store: &mut ProjectStore) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let current = snapshot(store)?;
    if current.enabled {
        return Ok(current);
    }
    match std::fs::symlink_metadata(store.root().join(TEMPLATE_PATH)) {
        Ok(_) => {
            store
                .adopt_visible_document_if_absent(
                    TEMPLATE_PATH,
                    DocumentKind::Prose,
                    "Enable workspace template",
                )
                .map_err(IpcFailure::store)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store
                .create_document_if_absent(
                    TEMPLATE_PATH,
                    DocumentContent::Prose(DEFAULT_TEMPLATE.into()),
                    "Enable workspace template",
                )
                .map_err(IpcFailure::store)?;
        }
        Err(error) => {
            return Err(IpcFailure::new(
                "workspace_template_failed",
                error.to_string(),
                false,
            ));
        }
    }
    snapshot(store)
}

#[tauri::command]
pub(super) async fn workspace_template_get(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let _admission = lock_application_admission(&state, "workspace configuration")?;
    let mut session = lock_session(&state)?;
    snapshot(require_bound_store(&mut session, &project_id, &session_id)?)
}

#[tauri::command]
pub(super) async fn workspace_template_enable(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<WorkspaceTemplateSnapshot, IpcFailure> {
    let _admission = lock_application_admission(&state, "workspace configuration")?;
    let mut session = lock_session(&state)?;
    enable(require_bound_store(&mut session, &project_id, &session_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn workspace_theme_defaults_and_strict_colors() {
        assert_eq!(
            parse_config(DEFAULT_TEMPLATE).unwrap().theme,
            WorkspaceTheme::default()
        );
        let config = parse_config("```loom-workspace\n[theme]\nmode='dark'\ncanvas='#123abc'\ntext='#ABCDEF'\naccent='#000000'\n```").unwrap();
        assert_eq!(config.theme.mode, WorkspaceThemeMode::Dark);
        assert_eq!(config.theme.canvas.as_deref(), Some("#123abc"));
        for invalid in [
            "red",
            "#123",
            "#12345678",
            "#gg0000",
            "url(x)",
            "#ffffff;",
            "#éaaaa",
        ] {
            assert!(
                parse_config(&format!(
                    "```loom-workspace\n[theme]\ncanvas='{invalid}'\n```"
                ))
                .is_err()
            );
        }
        assert!(parse_config("```loom-workspace\n[theme]\nmode='auto'\n```").is_err());
        assert!(parse_config("```loom-workspace\n[theme]\nstylesheet='https://x'\n```").is_err());
    }

    #[test]
    fn comments_inherit_defaults_and_named_overrides_are_independent() {
        assert_eq!(
            parse_config(DEFAULT_TEMPLATE).unwrap(),
            WorkspaceConfig::default()
        );
        assert!(
            WorkspaceConfig::default()
                .panes
                .values()
                .all(|pane| pane.visible)
        );
        let config = parse_config("```loom-workspace\n[panes.terminal]\nvisible=true\n[panes.reader]\nkind='chat'\ntitle='読み手'\ncontext=['@\"My draft\"','@notes/']\n```\n").unwrap();
        assert!(config.panes["terminal"].visible);
        assert!(config.panes["chat"].visible);
        assert_eq!(config.panes["reader"].title.as_deref(), Some("読み手"));
        assert_eq!(config.panes["reader"].position, PanePosition::Right);
    }

    #[test]
    fn model_selection_names_exactly_one_bounded_native_authority() {
        for (field, expected) in [
            (
                "catalog",
                WorkspaceModel::Catalog("google.gemma-4-12b-it-qat-q4_0".into()),
            ),
            (
                "profile",
                WorkspaceModel::Profile("google.gemma-4-12b-it-qat-q4_0".into()),
            ),
        ] {
            let config = parse_config(&format!(
                "```loom-workspace\n[model]\n{field}='google.gemma-4-12b-it-qat-q4_0'\n```"
            ))
            .expect("bounded native identity");
            assert_eq!(config.model, Some(expected));
            assert_eq!(config.panes, WorkspaceConfig::default().panes);
        }
        for model in [
            "catalog='one'\nprofile='two'".to_owned(),
            "path='/tmp/model.gguf'".to_owned(),
            "catalog='../model'".to_owned(),
            "catalog=''".to_owned(),
            "catalog='https://example.invalid/model'".to_owned(),
            format!("catalog='{}'", "x".repeat(129)),
        ] {
            assert!(parse_config(&format!("```loom-workspace\n[model]\n{model}\n```")).is_err());
        }
    }

    #[test]
    fn markdown_fences_do_not_accidentally_execute_examples() {
        let text = "````markdown\n```loom-workspace\ninvalid\n```\n````\n";
        assert_eq!(parse_config(text).unwrap(), WorkspaceConfig::default());
        assert!(parse_config("```loom-workspace\n").is_err());
        assert!(parse_config("```loom-workspace\n```\n```loom-workspace\n```\n").is_err());
        assert_eq!(
            parse_config("~~~loom-workspace\r\n# settings\r\n~~~").unwrap(),
            WorkspaceConfig::default()
        );
    }

    #[test]
    fn invalid_or_unbounded_settings_are_rejected() {
        for settings in [
            "unknown=true",
            "[panes.chat]\nvisible='yes'",
            "[panes.chat]\nposition='floating'",
            "[panes.writing]\nvisible=false",
            "[panes.other]\nvisible=true",
            "[panes.chat]\ncontext=['not a reference']",
            "[panes.chat]\ndocument='@One @Two'",
            "[panes.chat]\ncommand='sh'",
            "[panes.'../escape']\nkind='chat'",
        ] {
            assert!(
                parse_config(&format!("```loom-workspace\n{settings}\n```\n")).is_err(),
                "{settings}"
            );
        }
        assert!(parse_config(&"x".repeat(MAX_TEMPLATE_BYTES + 1)).is_err());
        let mut panes = String::new();
        for index in 0..5 {
            writeln!(panes, "[panes.extra{index}]\nkind='chat'").unwrap();
        }
        assert!(parse_config(&format!("```loom-workspace\n{panes}```\n")).is_err());
        let references = vec!["'@document'"; 17].join(",");
        assert!(
            parse_config(&format!(
                "```loom-workspace\n[panes.chat]\ncontext=[{references}]\n```\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn enabling_is_explicit_and_never_overwrites_ordinary_author_content() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        assert!(!snapshot(&mut store).unwrap().enabled);
        assert!(!store.root().join(TEMPLATE_PATH).exists());
        let first = enable(&mut store).unwrap();
        assert!(first.enabled);
        assert_eq!(
            store.read_document(TEMPLATE_PATH).unwrap().text,
            DEFAULT_TEMPLATE
        );
        assert_eq!(enable(&mut store).unwrap().revision_id, first.revision_id);
        let text = "# My workspace\n\n```loom-workspace\n[panes.chat]\nvisible=false\n```\n";
        store
            .save_document(
                TEMPLATE_PATH,
                DocumentContent::Prose(text.into()),
                "author edit",
            )
            .unwrap();
        assert!(!enable(&mut store).unwrap().config.panes["chat"].visible);
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, text);
    }

    #[test]
    fn existing_template_is_adopted_without_rewriting_even_when_invalid() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let text = "# Mine\r\n```loom-workspace\r\nunknown = true\r\n```\r\n";
        std::fs::write(store.root().join(TEMPLATE_PATH), text).unwrap();
        assert!(!snapshot(&mut store).unwrap().enabled);
        let result = enable(&mut store).unwrap();
        assert!(result.enabled && result.error.is_some() && result.document_id.is_some());
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, text);
        assert_eq!(enable(&mut store).unwrap().revision_id, result.revision_id);
    }
    #[test]
    fn external_template_refresh_imports_only_uncontested_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let original = enable(&mut store).unwrap();
        let external =
            "# My settings\r\n```loom-workspace\r\n[panes.chat]\r\nvisible=false\r\n```\r\n";
        std::fs::write(store.root().join(TEMPLATE_PATH), external).unwrap();
        let refreshed = snapshot(&mut store).unwrap();
        assert!(!refreshed.config.panes["chat"].visible);
        assert_ne!(refreshed.revision_id, original.revision_id);
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, external);
        assert_eq!(
            std::fs::read_to_string(store.root().join(TEMPLATE_PATH)).unwrap(),
            external
        );
        let stable = snapshot(&mut store).unwrap();
        assert_eq!(stable.revision_id, refreshed.revision_id);
        let base = store.read_document(TEMPLATE_PATH).unwrap();
        store
            .upsert_transient_draft(
                TEMPLATE_PATH,
                base.revision_id,
                0,
                DocumentContent::Prose("Distinct local writing".into()),
            )
            .unwrap();
        std::fs::write(store.root().join(TEMPLATE_PATH), "A conflicting disk edit").unwrap();
        assert!(snapshot(&mut store).is_err());
        assert_eq!(
            store
                .load_transient_draft(TEMPLATE_PATH)
                .unwrap()
                .unwrap()
                .text,
            "Distinct local writing"
        );
        assert_eq!(
            store
                .reconciliation_snapshot(TEMPLATE_PATH)
                .unwrap()
                .active_revision_id,
            base.revision_id
        );
    }
}
