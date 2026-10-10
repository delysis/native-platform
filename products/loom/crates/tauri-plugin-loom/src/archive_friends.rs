//! Archive I/O runs outside application, model and session admission locks.
//! The resulting immutable evidence cannot grant a workspace or edit authority.
use super::*;
use ::archive_friends::{ContextScope, DOTFILE, PreparedContext};
use std::fmt::Write as _;

pub(super) type Provider = ::archive_friends::NativeContextProvider;

pub(super) enum Admission {
    Replay(WeaveStarted),
    Prepared(Option<Prepared>),
}

pub(super) struct Prepared {
    pub context: PreparedContext,
    authority: Authority,
}

struct Authority {
    project: String,
    session: String,
    owner_project: ProjectId,
    owner_session: CommandId,
    owner_root: PathBuf,
    document: DocumentId,
    revision: RevisionId,
    blob: BlobId,
    cursor: u64,
    epoch: u64,
    model_basis: BlobId,
    draft: Option<loom_store::TransientDraftClaim>,
}

impl Prepared {
    pub(super) fn validate_model(&self, environment: &ModelEnvironment) -> Result<(), IpcFailure> {
        if environment_basis(environment)? != self.authority.model_basis {
            return Err(IpcFailure::new(
                "archive_model_conflict",
                "The writing model changed during archive preparation.",
                true,
            ));
        }
        Ok(())
    }

    pub(super) fn validate(
        &self,
        session: &Session,
        state: &PluginState,
        loaded: &LoadedDocument,
        cursor: u64,
    ) -> Result<(), IpcFailure> {
        self.authority.validate(
            session,
            state.archive_friends.epoch(),
            loaded,
            cursor,
            &self.context.source_basis,
        )
    }
}

impl Authority {
    fn validate(
        &self,
        session: &Session,
        epoch: u64,
        loaded: &LoadedDocument,
        cursor: u64,
        source_basis: &str,
    ) -> Result<(), IpcFailure> {
        let owner = session.workspace.as_ref().ok_or_else(stale)?;
        let authority = self;
        if session.phase != SessionPhase::Open
            || session
                .active_session_id
                .is_none_or(|id| id.to_string() != authority.session)
            || session
                .store
                .as_ref()
                .is_none_or(|store| store.manifest().project_id.to_string() != authority.project)
            || owner.project_id != authority.owner_project
            || owner.session_id != authority.owner_session
            || owner.root != authority.owner_root
            || loaded.document_id != authority.document
            || loaded.revision_id != authority.revision
            || loaded.blob_id != authority.blob
            || cursor != authority.cursor
            || epoch != authority.epoch
            || source_basis != format!("{}:{}", authority.blob, authority.cursor)
            || draft_claim(
                session.store.as_ref().ok_or_else(stale)?,
                &loaded.relative_path,
            )? != authority.draft
        {
            return Err(stale());
        }
        Ok(())
    }
}

fn draft_claim(
    store: &ProjectStore,
    path: &str,
) -> Result<Option<loom_store::TransientDraftClaim>, IpcFailure> {
    store
        .load_transient_draft(path)
        .map(|draft| {
            draft.map(|draft| loom_store::TransientDraftClaim {
                version: draft.version,
                source_revision_id: draft.source_revision_id,
                blob_id: draft.blob_id,
            })
        })
        .map_err(IpcFailure::store)
}

fn document_scope(
    owner: &workspace_owner::Owner,
    project: &str,
    active_session: &str,
    document: &str,
) -> ContextScope {
    ContextScope {
        project: format!("{}:{project}", owner.project_id),
        session: format!("{}:{active_session}", owner.session_id),
        document: document.into(),
    }
}

pub(super) fn cancel_document(
    state: &PluginState,
    session: &Session,
    project: &str,
    active_session: &str,
    document: &str,
) {
    if let Some(owner) = &session.workspace {
        state
            .archive_friends
            .cancel(&document_scope(owner, project, active_session, document));
    }
}

fn environment_basis(environment: &ModelEnvironment) -> Result<BlobId, IpcFailure> {
    serde_json::to_vec(environment)
        .map(|bytes| BlobId::digest(&bytes))
        .map_err(failure)
}

fn stale() -> IpcFailure {
    IpcFailure::new(
        "archive_source_conflict",
        "Archive preparation belongs to an earlier source or workspace session.",
        true,
    )
}
fn failure(error: impl std::fmt::Display) -> IpcFailure {
    IpcFailure::new("archive_context_unavailable", error.to_string(), true)
}

/// Validate the same typed policy, replay and model/document authority as final
/// admission before any archive access. Final admission repeats these checks;
/// no mutex or model lease survives the asynchronous preparation.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn prepare(
    state: &State<'_, PluginState>,
    project_id: &str,
    session_id: &str,
    command_id: &str,
    document_id: &str,
    relative_path: &str,
    revision_id: &str,
    blob_id: &str,
    cursor_byte: u64,
    policy: WeavePolicySnapshot,
) -> Result<Admission, IpcFailure> {
    let command = parse_command_id(command_id)?;
    let document = document_id.parse::<DocumentId>().map_err(|_| {
        IpcFailure::new(
            "invalid_document_id",
            "document ID is not a valid ULID",
            false,
        )
    })?;
    let revision = revision_id.parse::<RevisionId>().map_err(|_| {
        IpcFailure::new(
            "invalid_revision_id",
            "source revision ID is not a valid ULID",
            false,
        )
    })?;
    let blob = blob_id.parse::<BlobId>().map_err(|_| {
        IpcFailure::new(
            "invalid_blob_id",
            "visible blob ID is not a valid SHA-256 digest",
            false,
        )
    })?;
    let validated = validate_weave_policy(policy)?;
    let (authority, dotfile, prefix, scope) = {
        let _application = lock_application_admission(state, "archive context")?;
        if let Some(replay) = replay_weave_if_recorded(
            state,
            project_id,
            session_id,
            command,
            document,
            relative_path,
            revision,
            blob,
            cursor_byte,
            &validated,
        )? {
            return Ok(Admission::Replay(replay));
        }
        if matches!(validated, ValidatedWeavePolicy::LoompadV2 { .. }) {
            ensure_no_active_generations(state, "extending idle choices")?;
        }
        let _model = lock_model_lifecycle(state)?;
        let engine = server_weave::Engine::bind(validated, state)?;
        if engine.branch_count() > engine.max_cases() {
            return Err(IpcFailure::new(
                "model_branch_limit",
                "The active writing engine cannot admit this branch count.",
                false,
            ));
        }
        let mut session = lock_session(state)?;
        engine.admit(&session.agency)?;
        let store = require_bound_store(&mut session, project_id, session_id)?;
        let loaded = store
            .read_document(relative_path)
            .map_err(IpcFailure::store)?;
        ensure_document_id(&loaded, document_id)?;
        engine.bind_document_kind(loaded.kind)?;
        if loaded.revision_id != revision || loaded.blob_id != blob {
            return Err(stale());
        }
        let cursor = usize::try_from(cursor_byte).map_err(|_| stale())?;
        if cursor > loaded.text.len() || !loaded.text.is_char_boundary(cursor) {
            return Err(IpcFailure::new(
                "invalid_cursor_boundary",
                "The generation cursor is not a UTF-8 boundary in the source revision.",
                false,
            ));
        }
        let draft = draft_claim(store, relative_path)?;
        let owner = session.workspace.as_ref().ok_or_else(stale)?;
        let dotfile = workspace_owner::store(&session)?.root().join(DOTFILE);
        let prefix = loaded.text[..cursor].to_owned();
        let scope = document_scope(owner, project_id, session_id, document_id);
        let authority = Authority {
            project: project_id.into(),
            session: session_id.into(),
            owner_project: owner.project_id,
            owner_session: owner.session_id,
            owner_root: owner.root.clone(),
            document,
            revision,
            blob,
            cursor: cursor_byte,
            epoch: state.archive_friends.epoch(),
            model_basis: environment_basis(&engine.environment()?)?,
            draft,
        };
        (authority, dotfile, prefix, scope)
    };
    let provider = Arc::clone(&state.archive_friends);
    let basis = format!("{}:{}", authority.blob, cursor_byte);
    let epoch = authority.epoch;
    let context = tauri::async_runtime::spawn_blocking(move || {
        provider.prepare_at_epoch(scope, &basis, &dotfile, &prefix, epoch)
    })
    .await
    .map_err(failure)?
    .map_err(failure)?;
    if context.is_some()
        && authority.draft.as_ref().is_some_and(|draft| {
            draft.blob_id != authority.blob || draft.source_revision_id != authority.revision
        })
    {
        return Err(stale());
    }
    Ok(Admission::Prepared(
        context.map(|context| Prepared { context, authority }),
    ))
}

pub(super) fn workspace_references(
    markdown: &str,
    prepared: Option<&Prepared>,
) -> Result<Vec<loom_document::DocumentReference>, IpcFailure> {
    partition_references(
        markdown,
        prepared
            .map(|prepared| prepared.context.reserved_handles.as_slice())
            .unwrap_or_default(),
    )
}

fn partition_references(
    markdown: &str,
    handles: &[String],
) -> Result<Vec<loom_document::DocumentReference>, IpcFailure> {
    let mut references = loom_document::document_references(markdown)
        .map_err(|error| IpcFailure::new("document_reference_invalid", error.to_string(), false))?;
    references
        .retain(|reference| !::archive_friends::is_friend_invitation(markdown, reference, handles));
    Ok(references)
}

pub(super) fn drain(state: &PluginState, timeout: Duration) -> Result<(), IpcFailure> {
    state
        .archive_friends
        .cancel_and_drain(timeout)
        .map_err(failure)
}

fn help_workspace(session: &Session) -> Option<(ProjectId, CommandId, PathBuf)> {
    if session.phase != SessionPhase::Open {
        return None;
    }
    let owner = session.workspace.as_ref()?;
    let store = workspace_owner::store(session).ok()?;
    Some((
        owner.project_id,
        owner.session_id,
        store.root().join(DOTFILE),
    ))
}

/// Deliberately opened Help is substantive content, not editor chrome. Only
/// configuration I/O enters a background job; it shares the provider's drain.
pub(super) fn show_help<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<PluginState>() else {
        return;
    };
    if ensure_application_running(&state, "archive Help").is_err() {
        return;
    }
    let selected = {
        let Ok(session) = lock_session(&state) else {
            return;
        };
        help_workspace(&session)
    };
    let provider = Arc::clone(&state.archive_friends);
    let epoch = provider.epoch();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut text = String::from(
            "Write a bare @alias to invite historical archive passages into a writing suggestion. These are source quotations, not a live conversation.\n\nQuoted @“names” and @file.md remain document references. The archive does not edit your manuscript or run model experiments.\n\n",
        );
        if let Some((project, session_id, dotfile)) = &selected {
            let scope = ContextScope {
                project: project.to_string(),
                session: session_id.to_string(),
                document: "archive-help".into(),
            };
            match provider.inspect_config_at_epoch(scope, dotfile, epoch) {
                Ok(Some(config)) => {
                    let names = config.friends.keys().map(|name| format!("@{name}")).collect::<Vec<_>>().join("  ");
                    let _ = write!(text, "Your circle: {names}\n\n");
                }
                Ok(None) => text.push_str("Add a checkpointed archive path and friend aliases in the workspace dotfile to begin.\n\n"),
                Err(error) => { let _ = write!(text, "Configuration unavailable: {error}\n\n"); },
            }
            let _ = write!(
                text,
                "Source selection is configured in {}.",
                dotfile.display()
            );
        } else {
            text.push_str("Open a workspace with a .community-archive.toml file to begin.");
        }
        let Some(state) = app.try_state::<PluginState>() else {
            return;
        };
        if provider.epoch() != epoch || ensure_application_running(&state, "archive Help").is_err()
        {
            return;
        }
        {
            let Ok(session) = lock_session(&state) else {
                return;
            };
            let current = help_workspace(&session);
            if current != selected {
                return;
            }
        }
        if let Some(window) = app.get_webview_window("main") {
            app.dialog()
                .message(text)
                .title("Friends")
                .parent(&window)
                .show(|_| {});
        }
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use loom_document::DocumentContent;

    #[test]
    fn reserved_alias_keeps_explicit_quoted_document_context() {
        let directory = tempfile::tempdir().expect("context fixture");
        let (mut store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing")
            .expect("context fixture");
        store
            .create_document_if_absent(
                "visa.md",
                DocumentContent::Prose("Explicit source remains readable.".into()),
                "context fixture",
            )
            .expect("context fixture");
        let markdown = "☀ @visa and @“visa”";
        let references = partition_references(markdown, &["visa".into()]).expect("shared grammar");
        assert_eq!(references.len(), 1);
        assert_eq!(&markdown[references[0].range.clone()], "@“visa”");
        let plan = material_context::plan_with_references(
            &store,
            references,
            "source",
            4096,
            material_context::ReferenceRequirement::All,
        )
        .expect("explicit source admission");
        assert!(plan.text.contains("Explicit source remains readable."));
        assert_eq!(plan.bindings.len(), 1);
        assert_eq!(markdown, "☀ @visa and @“visa”");
    }

    #[test]
    fn absence_of_opt_in_retains_the_original_reference_plan() {
        let markdown = "@visa @“visa” @visa.md @visa/path";
        assert_eq!(
            workspace_references(markdown, None).expect("shared grammar"),
            loom_document::document_references(markdown).expect("shared grammar")
        );
    }

    #[test]
    fn prepared_authority_rejects_changed_sessions_source_and_epoch() {
        let directory = tempfile::tempdir().expect("authority fixture");
        let (mut store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing")
            .expect("authority fixture");
        store
            .create_document_if_absent(
                "Draft.md",
                DocumentContent::Prose("☀ Draft".into()),
                "authority fixture",
            )
            .expect("authority fixture");
        let loaded = store.read_document("Draft.md").expect("authority fixture");
        let project = store.manifest().project_id;
        let session_id = CommandId::new();
        let mut session = Session {
            phase: SessionPhase::Open,
            active_session_id: Some(session_id),
            ..Session::default()
        };
        workspace_owner::establish(&mut session, &store);
        session.store = Some(store);
        let owner = session.workspace.as_ref().expect("owned workspace");
        let authority = Authority {
            project: project.to_string(),
            session: session_id.to_string(),
            owner_project: owner.project_id,
            owner_session: owner.session_id,
            owner_root: owner.root.clone(),
            document: loaded.document_id,
            revision: loaded.revision_id,
            blob: loaded.blob_id,
            cursor: 3,
            epoch: 7,
            model_basis: BlobId::digest(b"controlled model environment"),
            draft: None,
        };
        assert_eq!(
            help_workspace(&session).expect("owned Help workspace").2,
            directory
                .path()
                .join("Writing")
                .canonicalize()
                .expect("canonical owned fixture")
                .join(DOTFILE)
        );
        session.phase = SessionPhase::Closed;
        assert!(help_workspace(&session).is_none());
        session.phase = SessionPhase::Open;
        let basis = format!("{}:3", loaded.blob_id);
        assert!(authority.validate(&session, 7, &loaded, 3, &basis).is_ok());
        assert!(authority.validate(&session, 8, &loaded, 3, &basis).is_err());
        assert!(authority.validate(&session, 7, &loaded, 0, &basis).is_err());
        assert!(
            authority
                .validate(&session, 7, &loaded, 3, "other source")
                .is_err()
        );
        session
            .store
            .as_mut()
            .expect("owned source store")
            .upsert_transient_draft(
                "Draft.md",
                loaded.revision_id,
                0,
                DocumentContent::Prose("☀ Newly typed draft".into()),
            )
            .expect("unsaved edit fixture");
        assert!(
            authority.validate(&session, 7, &loaded, 3, &basis).is_err(),
            "unsaved typing must invalidate prepared archive context"
        );
        session
            .store
            .as_mut()
            .expect("owned source store")
            .clear_transient_draft("Draft.md", 1)
            .expect("clear edit fixture");
        assert!(authority.validate(&session, 7, &loaded, 3, &basis).is_ok());
        session.active_session_id = Some(CommandId::new());
        assert!(authority.validate(&session, 7, &loaded, 3, &basis).is_err());
        session.active_session_id = Some(session_id);
        session
            .workspace
            .as_mut()
            .expect("owned workspace")
            .session_id = CommandId::new();
        assert!(authority.validate(&session, 7, &loaded, 3, &basis).is_err());
    }
}
