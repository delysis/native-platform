use std::fmt;
use std::path::{Component, Path};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

pub(crate) const DOCUMENT_FILESYSTEM_HINT_EVENT: &str = "loom://document-filesystem-hint";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DocumentFilesystemHint {
    project_id: String,
    session_id: String,
}

impl DocumentFilesystemHint {
    fn new(project_id: String, session_id: String) -> Self {
        Self {
            project_id,
            session_id,
        }
    }
}

/// Keeps the operating-system watcher alive for one open project session.
///
/// Dropping this value stops the watch. The callback emits only a scoped hint;
/// the renderer must obtain the authoritative document snapshot separately.
#[must_use = "dropping the filesystem watcher stops filesystem hints"]
pub(crate) struct DocumentFilesystemWatcher {
    _watcher: RecommendedWatcher,
}

impl DocumentFilesystemWatcher {
    pub(crate) fn start<R: Runtime>(
        app: &AppHandle<R>,
        project_root: &Path,
        project_id: String,
        session_id: String,
    ) -> notify::Result<Self> {
        let watched_root = project_root.to_path_buf();
        let hint = DocumentFilesystemHint::new(project_id, session_id);
        let event_app = AppHandle::clone(app);
        let event_root = watched_root.clone();

        let mut watcher = notify::recommended_watcher(move |event| {
            if event_requires_hint(&event_root, &event) {
                let _ = event_app.emit(DOCUMENT_FILESYSTEM_HINT_EVENT, hint.clone());
            }
        })?;

        // Watch the stable project root, not the manuscript directory itself.
        // Editors and file managers may atomically replace that whole directory.
        watcher.watch(&watched_root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

impl fmt::Debug for DocumentFilesystemWatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentFilesystemWatcher")
            .field("watcher", &"active")
            .finish()
    }
}

fn event_requires_hint(project_root: &Path, event: &notify::Result<Event>) -> bool {
    let Ok(event) = event else {
        // Backend errors mean the incremental event stream is no longer proof
        // of current state. Ask the renderer to fetch an authoritative snapshot.
        return true;
    };

    if event.need_rescan() {
        return true;
    }
    if event.kind.is_access() {
        return false;
    }
    if event.paths.is_empty() {
        return true;
    }

    event
        .paths
        .iter()
        .any(|path| is_writing_path(project_root, path))
}

fn is_writing_path(project_root: &Path, event_path: &Path) -> bool {
    let relative = relative_event_path(project_root, event_path);
    let Some(relative) = relative else {
        return false;
    };
    relative.components().all(|component| match component {
        Component::Normal(name) => name.to_str().is_some_and(|name| {
            !name.starts_with('.') && !matches!(name, "node_modules" | "target")
        }),
        _ => false,
    })
}

fn relative_event_path<'a>(project_root: &Path, event_path: &'a Path) -> Option<&'a Path> {
    if event_path.is_absolute() {
        event_path.strip_prefix(project_root).ok()
    } else {
        Some(event_path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use notify::event::{AccessKind, CreateKind, Flag, ModifyKind, RemoveKind};
    use notify::{Error, Event, EventKind};
    use serde_json::json;

    use super::{DOCUMENT_FILESYSTEM_HINT_EVENT, DocumentFilesystemHint, event_requires_hint};

    fn assert_send<T: Send>() {}

    fn project_root() -> PathBuf {
        std::env::temp_dir().join("loom-document-watcher-test")
    }

    fn project_path(relative: &str) -> PathBuf {
        project_root().join(relative)
    }

    fn event(kind: EventKind, paths: impl IntoIterator<Item = PathBuf>) -> Event {
        let mut event = Event::new(kind);
        event.paths = paths.into_iter().collect();
        event
    }

    #[test]
    fn event_name_and_payload_are_path_free() {
        assert_eq!(
            DOCUMENT_FILESYSTEM_HINT_EVENT,
            "loom://document-filesystem-hint"
        );
        assert_eq!(
            serde_json::to_value(DocumentFilesystemHint::new(
                "project-1".to_owned(),
                "session-2".to_owned(),
            ))
            .expect("hint serializes"),
            json!({
                "project_id": "project-1",
                "session_id": "session-2",
            })
        );
    }

    #[test]
    fn watcher_can_be_owned_by_synchronized_plugin_state() {
        assert_send::<super::DocumentFilesystemWatcher>();
    }

    #[test]
    fn manuscript_create_modify_and_remove_require_a_hint() {
        let root = project_root();
        for kind in [
            EventKind::Create(CreateKind::Any),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Remove(RemoveKind::Any),
        ] {
            assert!(event_requires_hint(
                &root,
                &Ok(event(kind, [project_path("manuscript/chapter.md")])),
            ));
        }
    }

    #[test]
    fn manuscript_directory_deletion_requires_a_hint() {
        assert!(event_requires_hint(
            &project_root(),
            &Ok(event(
                EventKind::Remove(RemoveKind::Any),
                [project_path("manuscript")],
            )),
        ));
    }

    #[test]
    fn relative_manuscript_paths_require_a_hint() {
        assert!(event_requires_hint(
            &project_root(),
            &Ok(event(
                EventKind::Modify(ModifyKind::Any),
                [PathBuf::from("manuscript/chapter.md")],
            )),
        ));
    }

    #[test]
    fn root_writing_and_folder_moves_require_a_hint() {
        // A removed or renamed directory may itself have an extension.
        for path in ["Notes.md", "Notes.TXT", "chapters", "drafts.v2"] {
            assert!(event_requires_hint(
                &project_root(),
                &Ok(event(
                    EventKind::Modify(ModifyKind::Any),
                    [project_path(path)],
                ))
            ));
        }
    }

    #[test]
    fn metadata_and_unrelated_paths_are_ignored() {
        let root = project_root();
        for path in [
            project_path(".loom/project.sqlite3"),
            project_path("target/generated.md"),
            std::env::temp_dir().join("other-project/manuscript/chapter.md"),
        ] {
            assert!(!event_requires_hint(
                &root,
                &Ok(event(EventKind::Modify(ModifyKind::Any), [path])),
            ));
        }
    }

    #[test]
    fn parent_directory_escape_is_not_a_manuscript_event() {
        assert!(!event_requires_hint(
            &project_root(),
            &Ok(event(
                EventKind::Modify(ModifyKind::Any),
                [PathBuf::from("manuscript/../.loom/project.sqlite3")],
            )),
        ));
    }

    #[test]
    fn access_only_events_are_ignored_even_without_paths() {
        assert!(!event_requires_hint(
            &project_root(),
            &Ok(event(EventKind::Access(AccessKind::Any), [])),
        ));
    }

    #[test]
    fn empty_non_access_events_and_backend_errors_require_a_rescan() {
        assert!(event_requires_hint(
            &project_root(),
            &Ok(event(EventKind::Any, [])),
        ));
        assert!(event_requires_hint(
            &project_root(),
            &Err(Error::generic("watch stream lost")),
        ));
    }

    #[test]
    fn explicit_rescan_flag_overrides_path_filtering() {
        let mut rescan = Event::new(EventKind::Other);
        rescan.paths.push(project_path(".loom/project.sqlite3"));
        let rescan = rescan.set_flag(Flag::Rescan);
        assert!(event_requires_hint(&project_root(), &Ok(rescan),));
    }
}
