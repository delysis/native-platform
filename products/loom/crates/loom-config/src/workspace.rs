//! Layout is data. Describing a pane never resolves a document, starts a model,
//! spawns a process, or grants access to a referenced resource.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAX_PANES: usize = 8;

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
    pub panes: BTreeMap<String, PaneConfig>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
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
        if self.panes.len() > MAX_PANES {
            return Err("A workspace supports at most eight panes.".into());
        }
        let mut config = WorkspaceConfig::default();
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
            let resolved = resolve_pane(name, base, overrides)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
