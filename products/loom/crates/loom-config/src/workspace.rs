//! Layout is data. Describing a pane never resolves a document, starts a model,
//! spawns a process, or grants access to a referenced resource.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAX_PANES: usize = 8;

/// A name selects existing native authority; it cannot grant authority to a
/// path, URL, arbitrary model digest, or download operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceModel {
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
pub enum WorkspaceThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceTheme {
    pub mode: WorkspaceThemeMode,
    pub canvas: Option<String>,
    pub text: Option<String>,
    pub accent: Option<String>,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Editor,
    Chat,
    Terminal,
    Browser,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PanePosition {
    Main,
    Right,
    Bottom,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceOverrides {
    pub model: Option<WorkspaceModel>,
    pub theme: WorkspaceTheme,
    pub panes: BTreeMap<String, PaneOverrides>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaneOverrides {
    pub kind: Option<PaneKind>,
    pub position: Option<PanePosition>,
    pub visible: Option<bool>,
    pub title: Option<String>,
    pub document: Option<String>,
    pub context: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PaneConfig {
    pub kind: PaneKind,
    pub position: PanePosition,
    pub visible: bool,
    pub title: Option<String>,
    pub document: Option<String>,
    pub context: Vec<String>,
}

impl PaneConfig {
    fn for_kind(kind: PaneKind) -> Self {
        Self {
            kind,
            position: match kind {
                PaneKind::Editor => PanePosition::Main,
                PaneKind::Chat => PanePosition::Right,
                PaneKind::Terminal | PaneKind::Browser => PanePosition::Bottom,
            },
            visible: kind == PaneKind::Editor,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceConfig {
    pub model: Option<WorkspaceModel>,
    pub theme: WorkspaceTheme,
    pub panes: BTreeMap<String, PaneConfig>,
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
            .map(|(name, kind)| (name.into(), PaneConfig::for_kind(kind)))
            .collect(),
        }
    }
}

impl WorkspaceOverrides {
    pub fn resolve(&self) -> Result<WorkspaceConfig, String> {
        self.resolve_with_defaults(false)
    }

    fn resolve_with_defaults(&self, all_panes_visible: bool) -> Result<WorkspaceConfig, String> {
        if self.panes.len() > MAX_PANES {
            return Err("A workspace supports at most eight panes.".into());
        }
        let mut config = WorkspaceConfig::default();
        if let Some(model) = &self.model {
            model.validate()?;
        }
        self.theme.validate()?;
        config.model.clone_from(&self.model);
        config.theme.clone_from(&self.theme);
        if all_panes_visible {
            for pane in config.panes.values_mut() {
                pane.visible = true;
            }
        }
        for (name, overrides) in &self.panes {
            if !valid_pane_id(name) {
                return Err(
                    "Pane names use up to 64 letters, digits, hyphens, or underscores.".into(),
                );
            }
            let base = match config.panes.remove(name) {
                Some(base) => base,
                None => PaneConfig::for_kind(
                    overrides
                        .kind
                        .ok_or_else(|| format!("Pane {name} needs a kind."))?,
                ),
            };
            let mut resolved = resolve_pane(name, base, overrides)?;
            if all_panes_visible && overrides.visible.is_none() {
                resolved.visible = true;
            }
            config.panes.insert(name.clone(), resolved);
            if config.panes.len() > MAX_PANES {
                return Err("A workspace supports at most eight panes.".into());
            }
        }
        let mut main = config
            .panes
            .values()
            .filter(|pane| pane.visible && pane.position == PanePosition::Main);
        // The current shell's main-document lifecycle still belongs to its
        // editor. Do not advertise a chat-first lifecycle by relaxing a parser.
        if !matches!(main.next(), Some(pane) if pane.kind == PaneKind::Editor)
            || main.next().is_some()
        {
            return Err("Keep exactly one visible editor in the main position.".into());
        }
        Ok(config)
    }
}

fn resolve_pane(
    name: &str,
    mut resolved: PaneConfig,
    overrides: &PaneOverrides,
) -> Result<PaneConfig, String> {
    if let Some(kind) = overrides.kind
        && kind != resolved.kind
    {
        resolved = PaneConfig::for_kind(kind);
    }
    if let Some(position) = overrides.position {
        resolved.position = position;
    }
    if let Some(visible) = overrides.visible {
        resolved.visible = visible;
    }
    if let Some(title) = &overrides.title {
        if title.len() > 128 || title.chars().any(char::is_control) {
            return Err(format!(
                "Pane {name} needs a title of at most 128 bytes without control characters."
            ));
        }
        resolved.title = Some(title.clone());
    }
    if let Some(document) = &overrides.document {
        validate_reference(document)?;
        resolved.document = Some(document.clone());
    }
    if let Some(context) = &overrides.context {
        if context.len() > 16 {
            return Err(format!("Pane {name} has more than 16 context references."));
        }
        for reference in context {
            validate_reference(reference)?;
        }
        resolved.context.clone_from(context);
    }
    Ok(resolved)
}

/// A durable presentation identity, never a filesystem path or command.
pub fn valid_pane_id(id: &str) -> bool {
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

/// Read the current-main Markdown workspace format without modifying its source.
/// Mine TOML is preferred when present; callers choose exactly one source.
pub fn parse_loom_workspace(markdown: &str) -> Result<WorkspaceConfig, String> {
    if markdown.len() > crate::MAX_CONFIG_BYTES {
        return Err("The workspace template exceeds 64 KiB.".into());
    }
    let overrides: WorkspaceOverrides = toml::from_str(config_fence(markdown)?)
        .map_err(|error| format!("Workspace settings: {error}"))?;
    overrides.resolve_with_defaults(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_theme_defaults_and_strict_colors() {
        assert_eq!(
            parse_loom_workspace("").unwrap().theme,
            WorkspaceTheme::default()
        );
        let config = parse_loom_workspace("```loom-workspace\n[theme]\nmode='dark'\ncanvas='#123abc'\ntext='#ABCDEF'\naccent='#000000'\n```").unwrap();
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
                parse_loom_workspace(&format!(
                    "```loom-workspace\n[theme]\ncanvas='{invalid}'\n```"
                ))
                .is_err()
            );
        }
        assert!(parse_loom_workspace("```loom-workspace\n[theme]\nmode='auto'\n```").is_err());
        assert!(
            parse_loom_workspace("```loom-workspace\n[theme]\nstylesheet='https://x'\n```")
                .is_err()
        );
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
            let config = parse_loom_workspace(&format!(
                "```loom-workspace\n[model]\n{field}='google.gemma-4-12b-it-qat-q4_0'\n```"
            ))
            .expect("bounded native identity");
            assert_eq!(config.model, Some(expected));
            assert_eq!(config.panes, parse_loom_workspace("").unwrap().panes);
        }
        for model in [
            "catalog='one'\nprofile='two'".to_owned(),
            "path='/tmp/model.gguf'".to_owned(),
            "catalog='../model'".to_owned(),
            "catalog=''".to_owned(),
            "catalog='https://example.invalid/model'".to_owned(),
            format!("catalog='{}'", "x".repeat(129)),
        ] {
            assert!(
                parse_loom_workspace(&format!("```loom-workspace\n[model]\n{model}\n```")).is_err()
            );
        }
    }

    #[test]
    fn current_markdown_workspace_retains_default_visibility_and_exact_fences() {
        let text = "# Workspace\r\n~~~loom-workspace\r\n[theme]\r\nmode='dark'\r\n[panes.chat]\r\nvisible=false\r\n~~~\r\n";
        let config = parse_loom_workspace(text).unwrap();
        assert!(config.panes["terminal"].visible);
        assert!(!config.panes["chat"].visible);
        assert_eq!(config.theme.mode, WorkspaceThemeMode::Dark);
        let example = "````markdown\n```loom-workspace\ninvalid\n```\n````\n";
        assert!(parse_loom_workspace(example).is_ok());
        assert!(parse_loom_workspace("```loom-workspace\n").is_err());
        assert!(parse_loom_workspace("```loom-workspace\n```\n```loom-workspace\n```\n").is_err());
    }

    fn parse(source: &str) -> Result<WorkspaceConfig, String> {
        toml::from_str::<WorkspaceOverrides>(source)
            .map_err(|error| error.to_string())?
            .resolve()
    }

    #[test]
    fn the_default_surface_is_just_writing_and_power_panes_are_opt_in() {
        let config = parse("").expect("defaults");
        assert_eq!(config, WorkspaceConfig::default());
        assert_eq!(config.panes.values().filter(|pane| pane.visible).count(), 1);
        let configured = parse("[panes.chat]\nvisible=true\n[panes.reader]\nkind='chat'\nvisible=true\ntitle='読み手'\ncontext=['@\"My draft\"','@notes/']").expect("explicit panes");
        assert!(configured.panes["chat"].visible);
        assert_eq!(configured.panes["reader"].position, PanePosition::Right);
        assert!(!configured.panes["terminal"].visible);
    }

    #[test]
    fn invalid_layouts_and_embedded_commands_are_rejected() {
        for source in [
            "unknown=true",
            "[panes.chat]\nvisible='yes'",
            "[panes.chat]\nposition='floating'",
            "[panes.writing]\nvisible=false",
            "[panes.other]\nvisible=true",
            "[panes.chat]\ncontext=['not a reference']",
            "[panes.chat]\ndocument='@One @Two'",
            "[panes.chat]\ncommand='sh'",
            "[panes.'../escape']\nkind='chat'",
            "[panes.chat]\ntitle='a\tb'",
            "[panes.chat]\nposition='main'\nvisible=true",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn pane_and_reference_limits_include_inherited_defaults() {
        let mut overrides = WorkspaceOverrides::default();
        for index in 0..5 {
            overrides.panes.insert(
                format!("extra{index}"),
                PaneOverrides {
                    kind: Some(PaneKind::Chat),
                    ..Default::default()
                },
            );
        }
        assert!(overrides.resolve().is_err());
        overrides.panes.clear();
        overrides.panes.insert(
            "chat".into(),
            PaneOverrides {
                context: Some(vec!["@document".into(); 17]),
                ..Default::default()
            },
        );
        assert!(overrides.resolve().is_err());
    }
}
