//! Explicit mounted-document admission. Names select private grants, never paths.
use std::collections::BTreeMap;

use crate::{
    IpcFailure, PluginState, Session, document_bindings,
    material_context::{ReadContext, Value},
    workspace_owner, workspace_roots,
};
use loom_store::ProjectStore;

#[derive(serde::Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "IPC names distinguish the three identity domains"
)]
pub(crate) struct Destination {
    root_id: String,
    document_id: String,
    workspace_session_id: String,
}

#[tauri::command]
pub(crate) async fn workspace_reference_resolve(
    project_id: String,
    session_id: String,
    reference: String,
    state: tauri::State<'_, PluginState>,
) -> Result<Destination, IpcFailure> {
    let _admission = crate::lock_application_admission(&state, "opening a mounted reference")?;
    let mut session = crate::lock_session(&state)?;
    crate::require_bound_store(&mut session, &project_id, &session_id)?;
    let token = session
        .workspace
        .as_ref()
        .ok_or_else(|| failure("Open a workspace first."))?
        .session_id
        .to_string();
    with_document(&state, &session, &reference, |store, name, root_id| {
        Ok(Destination {
            root_id: root_id.into(),
            document_id: document_bindings::resolve_document_id(store, name)?.to_string(),
            workspace_session_id: token,
        })
    })
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub value: Value,
    pub media: Vec<llama_native_types::MediaInput>,
}

#[derive(Debug, Default)]
pub(crate) struct Snapshots {
    pub entries: BTreeMap<String, Result<Snapshot, IpcFailure>>,
    bytes: usize,
    media_bytes: usize,
    media_count: usize,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("document_reference_invalid", message, false)
}

pub(crate) fn split(name: &str) -> Result<Option<(&str, &str)>, IpcFailure> {
    let Some((root, path)) = name.split_once("::") else {
        return Ok(None);
    };
    if path.ends_with('/') {
        return Err(failure(
            "Mounted folder references are not supported yet. Name an individual document.",
        ));
    }
    if root.trim().is_empty()
        || root.trim() != root
        || root.contains(['/', '\\'])
        || path.is_empty()
        || path.contains(['\\', ':'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || name.chars().any(char::is_control)
        || name.len() > 4096
    {
        return Err(failure(
            "Use Root::path for a document in a mounted folder.",
        ));
    }
    Ok(Some((root, path)))
}

/// A called mounted function's unqualified direct references belong to its root.
/// Reserved @document still means the explicitly captured active manuscript.
pub(crate) fn relative(function: &str, reference: &str) -> Result<String, IpcFailure> {
    if reference == "document" || split(reference)?.is_some() {
        return Ok(reference.into());
    }
    Ok(match split(function)? {
        Some((root, _)) => format!("{root}::{reference}"),
        None => reference.into(),
    })
}

pub(crate) fn with_document<T>(
    state: &PluginState,
    session: &Session,
    name: &str,
    read: impl FnOnce(&ProjectStore, &str, &str) -> Result<T, IpcFailure>,
) -> Result<T, IpcFailure> {
    let (root_name, path) = split(name)?.ok_or_else(|| failure("Expected Root::path."))?;
    let owner = workspace_owner::store(session)?;
    let private = workspace_owner::private_root(state)?;
    let descriptors = workspace_roots::list(owner, &private)?;
    let mut matches = descriptors.iter().filter(|root| root.name == root_name);
    let root = matches.next().ok_or_else(|| {
        IpcFailure::new(
            "document_reference_missing",
            "The named workspace folder is not mounted.",
            false,
        )
    })?;
    if matches.next().is_some() {
        return Err(IpcFailure::new(
            "document_reference_ambiguous",
            "Several workspace folders have this name. Rename one before using its reference.",
            false,
        ));
    }
    let target = workspace_roots::resolve(owner, &root.id, &private)?;
    let existing = session
        .store
        .as_ref()
        .filter(|store| store.root() == target)
        .or_else(|| (owner.root() == target).then_some(owner));
    let opened;
    let store = if let Some(store) = existing {
        store
    } else {
        opened = ProjectStore::open(&target).map_err(IpcFailure::store)?;
        &opened
    };
    workspace_roots::validate_opened(owner, &root.id, store, &private)?;
    let result = read(store, path, &root.id)?;
    workspace_roots::validate_opened(owner, &root.id, store, &private)?;
    Ok(result)
}

impl Snapshots {
    pub fn admit<'a>(
        &mut self,
        state: &PluginState,
        session: &Session,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), IpcFailure> {
        for name in names {
            if !name.contains("::") || self.entries.contains_key(name) {
                continue;
            }
            if self.entries.len() >= 32 {
                return Err(failure(
                    "At most 32 mounted document references can be admitted.",
                ));
            }
            let value = with_document(state, session, name, |store, path, _| {
                let documents = document_bindings::resolve_references(store, &[path.into()])?;
                let bytes = documents
                    .iter()
                    .map(|document| document.text.len())
                    .sum::<usize>();
                let value = crate::material_context::SourceOrigin::of(store)
                    .wrap(Value::Documents { documents });
                let media = ReadContext::from(store).native_media([&value])?;
                Ok((Snapshot { value, media }, bytes))
            })
            .and_then(|(snapshot, bytes)| {
                if self.bytes.saturating_add(bytes) > 65_536 {
                    return Err(IpcFailure::new(
                        "document_reference_budget_exceeded",
                        "Mounted document inputs exceed 64 KiB.",
                        false,
                    ));
                }
                let media_bytes = snapshot
                    .media
                    .iter()
                    .map(|media| media.bytes.len())
                    .sum::<usize>();
                if self.media_bytes.saturating_add(media_bytes)
                    > crate::terminal_media::MAX_MEDIA_BYTES
                    || self.media_count.saturating_add(snapshot.media.len())
                        > crate::terminal_media::MAX_MEDIA
                {
                    return Err(failure(
                        "Mounted document media exceed the bounded input budget.",
                    ));
                }
                self.bytes += bytes;
                self.media_bytes += media_bytes;
                self.media_count += snapshot.media.len();
                Ok(snapshot)
            });
            self.entries.insert(name.into(), value);
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{BuildModelPolicy, material_context, workspace_roots};
    use loom_document::DocumentContent;

    fn document(store: &mut ProjectStore, path: &str, text: &str) {
        store
            .create_document_if_absent(path, DocumentContent::Prose(text.into()), "fixture")
            .unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn mounted_snapshots_preserve_origin_and_bytes_without_retaining_a_second_lease() {
        let directory = tempfile::tempdir().unwrap();
        let state = PluginState::with_app_local_data_root(
            Some(directory.path().join("Private")),
            true,
            BuildModelPolicy::default(),
        );
        let (mut owner, _) =
            ProjectStore::initialize(directory.path().join("Owner"), "Owner").unwrap();
        let (mut child, _) =
            ProjectStore::initialize(directory.path().join("Research"), "Research").unwrap();
        document(&mut owner, "Notes.md", "Wrong owner notes.");
        document(&mut child, "Notes.md", "Exact child notes.\r\n");
        document(&mut child, "Explain.md", "Use @Notes.");
        let mut wav = std::io::Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(
            &mut wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(42_i16).unwrap();
        writer.finalize().unwrap();
        let wav = wav.into_inner();
        let audio =
            crate::context_attachments::import_recorded_wav(child.root(), "voice.wav".into(), &wav)
                .unwrap();
        document(&mut child, "Voice.md", &audio.inline_markdown);
        let private = workspace_owner::private_root(&state).unwrap();
        let root = workspace_roots::mount(&mut owner, &child, &private, None).unwrap();
        let child_path = child.root().to_owned();
        drop(child);
        let mut session = Session::default();
        workspace_owner::establish(&mut session, &owner);
        session.store = Some(owner);
        let mut snapshots = Snapshots::default();
        snapshots
            .admit(
                &state,
                &session,
                ["Research::Explain", "Research::Notes", "Research::Voice"],
            )
            .unwrap();
        let context = ReadContext::from(session.store.as_ref().unwrap()).with_mounted(&snapshots);
        let function = context.resolve("Research::Explain").unwrap();
        let direct =
            loom_document::document_references(&material_context::exact(&function).unwrap())
                .unwrap();
        let key = relative("Research::Explain", &direct[0].name).unwrap();
        let notes = context.resolve(&key).unwrap();
        let voice = context.resolve("Research::Voice").unwrap();
        let media = context.native_media([&voice]).unwrap();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].bytes, wav);
        assert_eq!(
            material_context::exact(&notes).unwrap(),
            "Exact child notes.\r\n"
        );
        assert!(
            material_context::local_artifact_ids(session.store.as_ref().unwrap(), [&notes])
                .unwrap()
                .is_empty()
        );
        // The only child lease was the bounded explicit read; another client may now open it.
        let held = ProjectStore::open(&child_path).unwrap();
        let mut blocked = Snapshots::default();
        blocked
            .admit(&state, &session, ["Research::Notes"])
            .unwrap();
        assert!(
            ReadContext::from(session.store.as_ref().unwrap())
                .with_mounted(&blocked)
                .resolve("Research::Notes")
                .is_err()
        );
        drop(held);
        workspace_roots::remove(session.store.as_mut().unwrap(), &root.id, &private).unwrap();
        let mut revoked = Snapshots::default();
        revoked
            .admit(&state, &session, ["Research::Notes"])
            .unwrap();
        assert!(
            ReadContext::from(session.store.as_ref().unwrap())
                .with_mounted(&revoked)
                .resolve("Research::Notes")
                .is_err()
        );
        // Admitted text remains exact data after removal; serialized provenance grants no fresh read.
        assert_eq!(
            material_context::exact(&notes).unwrap(),
            "Exact child notes.\r\n"
        );
        assert!(
            ReadContext::from(session.store.as_ref().unwrap())
                .consult_with_budget_and_cancel(
                    &notes,
                    "",
                    1024,
                    &material_context::FolderScanBudget::default(),
                    &|| false
                )
                .is_ok()
        );
        std::fs::remove_dir_all(&child_path).unwrap();
        let retained =
            crate::terminal_media::retain(session.store.as_mut().unwrap(), &media).unwrap();
        assert_eq!(
            session
                .store
                .as_ref()
                .unwrap()
                .read_blob(retained[0].bytes_blob_id)
                .unwrap(),
            wav
        );
    }

    #[test]
    fn qualification_rejects_folders_traversal_and_duplicate_owner_names() {
        assert_eq!(split("notes/Plan.md").unwrap(), None);
        for name in [
            "Research::../Notes",
            "Research::/Notes",
            "Research::Notes/",
            "Research::a//b",
            "Research::",
            "::Notes",
        ] {
            assert!(split(name).is_err(), "{name}");
        }
        assert_eq!(
            relative("Research::Explain", "Other::Notes").unwrap(),
            "Other::Notes"
        );
        assert_eq!(
            relative("Research::Explain", "document").unwrap(),
            "document"
        );
        let directory = tempfile::tempdir().unwrap();
        let state = PluginState::with_app_local_data_root(
            Some(directory.path().join("Private")),
            true,
            BuildModelPolicy::default(),
        );
        let (mut owner, _) =
            ProjectStore::initialize(directory.path().join("Owner"), "Same").unwrap();
        let (child, _) = ProjectStore::initialize(directory.path().join("Child"), "Same").unwrap();
        let private = workspace_owner::private_root(&state).unwrap();
        workspace_roots::mount(&mut owner, &child, &private, None).unwrap();
        let config = owner.read_document(".loom.md").unwrap();
        owner
            .save_document(
                ".loom.md",
                DocumentContent::Prose(config.text.replace("Same (2)", "Same")),
                "ambiguous names fixture",
            )
            .unwrap();
        let mut session = Session::default();
        workspace_owner::establish(&mut session, &owner);
        session.store = Some(owner);
        assert_eq!(
            with_document(&state, &session, "Same::Notes", |_, _, _| Ok(()))
                .unwrap_err()
                .code,
            "document_reference_ambiguous"
        );
    }
}
