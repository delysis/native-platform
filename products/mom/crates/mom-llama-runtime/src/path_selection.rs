use crate::receipts::{Blocker, CommandResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathSelectionKind {
    Model,
    MultimodalProjector,
    Conversation,
    Attachment,
    McpExecutable,
}

impl PathSelectionKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "mmproj" => Some(Self::MultimodalProjector),
            "conversation" => Some(Self::Conversation),
            "attachment" => Some(Self::Attachment),
            "mcp" => Some(Self::McpExecutable),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::MultimodalProjector => "mmproj",
            Self::Conversation => "conversation",
            Self::Attachment => "attachment",
            Self::McpExecutable => "mcp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathSelection {
    pub kind: PathSelectionKind,
    pub path: Option<PathBuf>,
}

pub fn path_select(
    kind: PathSelectionKind,
    path: Option<PathBuf>,
) -> Result<CommandResult<PathSelection>> {
    let Some(path) = path else {
        return Ok(CommandResult::passed(
            "mom_llama.path_select",
            "contracted",
            PathSelection { kind, path: None },
            Vec::new(),
            Vec::new(),
            false,
            false,
        ));
    };
    if !path.exists() {
        return Ok(blocked(
            kind,
            "selected_path_missing",
            format!("The selected path does not exist: {}", path.display()),
        ));
    }
    if !path.is_file() {
        return Ok(blocked(
            kind,
            "selected_path_not_file",
            format!("The selected path is not a file: {}", path.display()),
        ));
    }
    if !extension_allowed(kind, &path) {
        return Ok(blocked(
            kind,
            "selected_path_type_invalid",
            format!(
                "{} is not a supported {} file.",
                path.display(),
                kind.as_str()
            ),
        ));
    }
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            return Ok(blocked(
                kind,
                "selected_path_unreadable",
                format!("The selected path could not be resolved: {error}"),
            ));
        }
    };
    Ok(CommandResult::passed(
        "mom_llama.path_select",
        "contracted",
        PathSelection {
            kind,
            path: Some(path.clone()),
        },
        Vec::new(),
        vec![path.display().to_string()],
        false,
        false,
    ))
}

fn extension_allowed(kind: PathSelectionKind, path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind {
        PathSelectionKind::Model | PathSelectionKind::MultimodalProjector => extension == "gguf",
        PathSelectionKind::Conversation => extension == "json",
        PathSelectionKind::Attachment | PathSelectionKind::McpExecutable => true,
    }
}

fn blocked(kind: PathSelectionKind, code: &str, message: String) -> CommandResult<PathSelection> {
    CommandResult::blocked(
        "mom_llama.path_select",
        "stub_blocked",
        Blocker::new(
            code,
            message,
            vec![format!("Choose another {} path.", kind.as_str())],
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_selection_is_typed_canonical_and_extension_bounded() {
        let root = std::env::temp_dir().join(format!(
            "mom-llama-path-select-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create fixture directory");
        let model = root.join("model.gguf");
        let text = root.join("notes.txt");
        std::fs::write(&model, b"GGUF").expect("write model fixture");
        std::fs::write(&text, b"notes").expect("write text fixture");

        let selected = path_select(PathSelectionKind::Model, Some(model.clone()))
            .expect("select model")
            .result
            .expect("selected model result");
        assert_eq!(selected.kind, PathSelectionKind::Model);
        assert_eq!(
            selected.path,
            Some(model.canonicalize().expect("canonical model"))
        );

        let blocked =
            path_select(PathSelectionKind::Model, Some(text.clone())).expect("reject text model");
        assert_eq!(blocked.status, "blocked");
        assert_eq!(
            blocked
                .blocker
                .as_ref()
                .map(|blocker| blocker.code.as_str()),
            Some("selected_path_type_invalid")
        );
        assert!(
            path_select(PathSelectionKind::Attachment, Some(text))
                .expect("select attachment")
                .result
                .is_some()
        );
        std::fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn cancelling_a_native_picker_is_a_typed_noop() {
        let result = path_select(PathSelectionKind::Conversation, None)
            .expect("cancel path selection")
            .result
            .expect("cancelled selection result");
        assert_eq!(result.path, None);
    }
}
