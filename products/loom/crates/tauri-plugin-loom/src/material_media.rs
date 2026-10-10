//! Session- and binding-scoped previews of named retained media.
use super::*;

#[path = "material_pdf.rs"]
pub(super) mod pdf;

const VERSION: &str = "m1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Request {
    project_id: ProjectId,
    session_id: CommandId,
    material_id: String,
    media_sha256: String,
    member: Option<Member>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Member {
    snapshot_id: String,
    occurrence_id: String,
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn token(request: &Request) -> Option<String> {
    if !materials::is_material_id(&request.material_id) || !valid_hash(&request.media_sha256) {
        return None;
    }
    let mut encoded = format!(
        "{VERSION}-{}-{}-{}-{}",
        request.project_id,
        request.session_id,
        request.material_id.strip_prefix("material-")?,
        request.media_sha256
    );
    if let Some(member) = &request.member {
        let occurrence = member.occurrence_id.strip_prefix("occurrence-")?;
        if !valid_hash(&member.snapshot_id) || !valid_hash(occurrence) {
            return None;
        }
        encoded.push('-');
        encoded.push_str(&member.snapshot_id);
        encoded.push('-');
        encoded.push_str(occurrence);
    }
    Some(encoded)
}
pub(super) fn parse(uri: &http::Uri) -> Option<Request> {
    if !matches!(
        (
            uri.scheme_str(),
            uri.authority().map(http::uri::Authority::as_str)
        ),
        (Some(LOOM_ASSET_SCHEME), Some("localhost")) | (Some("http"), Some("loom-asset.localhost"))
    ) || uri.query().is_some()
    {
        return None;
    }
    let encoded = uri.path().strip_prefix('/')?;
    if !encoded.is_ascii() || encoded.contains(['/', '%']) {
        return None;
    }
    let mut fields = encoded.split('-');
    if fields.next()? != VERSION {
        return None;
    }
    let mut request = Request {
        project_id: fields.next()?.parse().ok()?,
        session_id: fields.next()?.parse().ok()?,
        material_id: format!("material-{}", fields.next()?),
        media_sha256: fields.next()?.into(),
        member: None,
    };
    if let Some(snapshot_id) = fields.next() {
        request.member = Some(Member {
            snapshot_id: snapshot_id.into(),
            occurrence_id: format!("occurrence-{}", fields.next()?),
        });
    }
    if fields.next().is_some() || token(&request).as_deref() != Some(encoded) {
        return None;
    }
    Some(request)
}
pub(super) fn bind_tokens(
    mut source: materials::MaterialRead,
    project: &str,
    session: &str,
) -> Result<materials::MaterialRead, IpcFailure> {
    let project_id = project.parse().map_err(|_| {
        IpcFailure::new(
            "invalid_project_id",
            "The workspace identity is invalid.",
            false,
        )
    })?;
    let session_id = session.parse().map_err(|_| {
        IpcFailure::new(
            "invalid_session_id",
            "The workspace session is invalid.",
            false,
        )
    })?;
    let member = source.evidence.first().and_then(|evidence| {
        Some(Member {
            snapshot_id: evidence
                .locator
                .get("collection_snapshot")?
                .as_str()?
                .into(),
            occurrence_id: evidence.locator.get("occurrence_id")?.as_str()?.into(),
        })
    });
    if let Some(presentation) = &mut source.presentation {
        if presentation.detected_format == "pdf" {
            presentation.pdf_preview_token = token(&Request {
                project_id,
                session_id,
                material_id: source.material.id.clone(),
                media_sha256: presentation.id.clone(),
                member: member.clone(),
            });
            if presentation.pdf_preview_token.is_none() {
                return Err(IpcFailure::new(
                    "material_media_invalid",
                    "The PDF identity is invalid.",
                    false,
                ));
            }
        }
        for media in &mut presentation.media {
            media.preview_token = token(&Request {
                project_id,
                session_id,
                material_id: source.material.id.clone(),
                media_sha256: media.sha256.clone(),
                member: member.clone(),
            });
            if media.preview_token.is_none() {
                return Err(IpcFailure::new(
                    "material_media_invalid",
                    "The source media identity is invalid.",
                    false,
                ));
            }
        }
    }
    Ok(source)
}
fn selected_attachment(
    state: &PluginState,
    authority: &LoomAssetAuthority,
    request: &Request,
) -> Result<String, LoomAssetReadFailure> {
    let session = state
        .session
        .lock()
        .map_err(|_| LoomAssetReadFailure::Unavailable)?;
    let store = source_store(&session, request)?;
    if store.root() != authority.project_root {
        return Err(LoomAssetReadFailure::NotFound);
    }
    attachment_in_store(store, request)
}

fn source_store<'a>(
    session: &'a Session,
    request: &Request,
) -> Result<&'a ProjectStore, LoomAssetReadFailure> {
    if workspace_owner::is_bound(session, request.project_id, request.session_id) {
        return workspace_owner::store(session).map_err(|_| LoomAssetReadFailure::NotFound);
    }
    if session.phase != SessionPhase::Open || session.active_session_id != Some(request.session_id)
    {
        return Err(LoomAssetReadFailure::NotFound);
    }
    session
        .store
        .as_ref()
        .filter(|store| store.manifest().project_id == request.project_id)
        .ok_or(LoomAssetReadFailure::NotFound)
}

fn capture_source_authority(
    state: &PluginState,
    request: &Request,
) -> Result<(LoomAssetAuthority, bool), LoomAssetReadFailure> {
    let session = state
        .session
        .lock()
        .map_err(|_| LoomAssetReadFailure::Unavailable)?;
    let store = source_store(&session, request)?;
    Ok((
        LoomAssetAuthority {
            project_id: request.project_id,
            session_id: request.session_id,
            project_root: store.root().to_owned(),
        },
        workspace_owner::is_bound(&session, request.project_id, request.session_id),
    ))
}
fn attachment_in_store(
    store: &ProjectStore,
    request: &Request,
) -> Result<String, LoomAssetReadFailure> {
    let material = materials::resolve(store, &request.material_id)
        .map_err(|_| LoomAssetReadFailure::NotFound)?;
    if let Some(member) = &request.member {
        if material.kind != materials::MaterialKind::Collection {
            return Err(LoomAssetReadFailure::NotFound);
        }
        let snapshot =
            connected_collections::read_snapshot(store, &material.id, &member.snapshot_id)
                .map_err(|_| LoomAssetReadFailure::NotFound)?;
        return snapshot
            .members
            .into_iter()
            .find(|source| source.occurrence_id == member.occurrence_id)
            .map(|source| source.attachment_id)
            .ok_or(LoomAssetReadFailure::NotFound);
    }
    material.attachment_id.ok_or(LoomAssetReadFailure::NotFound)
}
pub(super) fn read(
    state: &PluginState,
    request: &Request,
) -> Result<LoadedProtocolAsset, LoomAssetReadFailure> {
    let (authority, _) = capture_source_authority(state, request)?;
    let attachment = selected_attachment(state, &authority, request)?;
    let media = context_attachments::read_context_media(
        &authority.project_root,
        &attachment,
        &request.media_sha256,
    )
    .map_err(|_| LoomAssetReadFailure::NotFound)?;
    // Recheck both session and binding after bounded filesystem work.
    if selected_attachment(state, &authority, request)? != attachment {
        return Err(LoomAssetReadFailure::NotFound);
    }
    Ok(LoadedProtocolAsset {
        bytes: media.bytes,
        media_type: media.mime_type,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::connected_collections as collections;
    use crate::workspace_template::{CollectionDefinition, CollectionScope};

    #[test]
    #[allow(clippy::too_many_lines)] // One owner lifetime: active, parked, then revoked.
    fn owner_media_survives_document_switch_but_not_binding_removal() {
        let temp = tempfile::tempdir().unwrap();
        let (mut owner, _) = ProjectStore::initialize(temp.path().join("Owner"), "Owner").unwrap();
        let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
        let attachment = context_attachments::import_path(owner.root(), &png).unwrap();
        let material =
            materials::bind_attachment(&mut owner, &attachment.id, Some("Picture")).unwrap();
        let media_sha256 = materials::read(&owner, &material.id)
            .unwrap()
            .presentation
            .unwrap()
            .media[0]
            .sha256
            .clone();
        let owner_id = owner.manifest().project_id;
        let document_session = CommandId::new();
        let state = PluginState::default();
        let owner_session = {
            let mut session = state.session.lock().unwrap();
            workspace_owner::establish(&mut session, &owner);
            let id = session.workspace.as_ref().unwrap().session_id;
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(document_session);
            session.store = Some(owner);
            id
        };
        let request = Request {
            project_id: owner_id,
            session_id: owner_session,
            material_id: material.id.clone(),
            media_sha256,
            member: None,
        };
        let bytes = std::fs::read(&png).unwrap();
        assert_eq!(read(&state, &request).unwrap().bytes, bytes);
        let document_request = Request {
            session_id: document_session,
            ..request.clone()
        };
        assert_eq!(read(&state, &document_request).unwrap().bytes, bytes);
        {
            let mut session = state.session.lock().unwrap();
            workspace_owner::park_active(&mut session);
            let (child, _) = ProjectStore::initialize(temp.path().join("Child"), "Child").unwrap();
            session.store = Some(child);
            session.active_session_id = Some(CommandId::new());
        }
        assert_eq!(read(&state, &request).unwrap().bytes, bytes);
        assert_eq!(
            read(&state, &document_request),
            Err(LoomAssetReadFailure::NotFound)
        );
        {
            let session = state.session.lock().unwrap();
            let child_id = session
                .store
                .as_ref()
                .unwrap()
                .manifest()
                .project_id
                .to_string();
            let child_session = session.active_session_id.unwrap().to_string();
            let context = workspace_owner::read_context(
                &session,
                &child_id,
                &child_session,
                &owner_id.to_string(),
                &owner_session.to_string(),
            )
            .unwrap();
            assert_eq!(context.materials.manifest().project_id, owner_id);
            assert_ne!(context.documents.root(), context.materials.root());
            assert!(
                workspace_owner::read_context(
                    &session,
                    &child_id,
                    &document_session.to_string(),
                    &owner_id.to_string(),
                    &owner_session.to_string(),
                )
                .is_err()
            );
            assert!(
                workspace_owner::read_context(
                    &session,
                    &child_id,
                    &child_session,
                    &owner_id.to_string(),
                    &CommandId::new().to_string(),
                )
                .is_err()
            );
        }
        {
            let mut session = state.session.lock().unwrap();
            materials::remove(
                workspace_owner::store_mut(&mut session).unwrap(),
                &material.id,
            )
            .unwrap();
        }
        assert_eq!(read(&state, &request), Err(LoomAssetReadFailure::NotFound));
        assert_eq!(std::fs::read(&png).unwrap(), bytes);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn collection_preview_stays_on_its_version_and_removal_revokes_access() {
        let temp = tempfile::tempdir().unwrap();
        let private = std::fs::canonicalize(temp.path()).unwrap().join("private");
        let (mut store, _) =
            ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
        let def = CollectionDefinition {
            id: format!("material-{}", "a".repeat(64)),
            name: "Research".into(),
            pinned: false,
            workspace_path: None,
            scope: CollectionScope::DriveFolder {
                id: "chosen-folder".into(),
            },
        };
        workspace_template::upsert_collection(&mut store, None, &def).unwrap();
        let principal = "writer@example.test";
        collections::save_grant(&store, &private, &def, principal).unwrap();
        let identity = collections::identity(&def, principal).unwrap();
        let remote = collections::RemoteMember {
            remote_id: "drawing".into(),
            name: "Drawing.png".into(),
            source_uri: "https://drive.google.com/file/d/drawing".into(),
            mime_type: "image/png".into(),
            listed_modified_time: None,
        };
        let publish = |store: &ProjectStore, path: &Path| {
            let attachment = context_attachments::import_path(store.root(), path).unwrap();
            let origin = context_attachments::record_import_origin(
                store.root(),
                &serde_json::json!({
                    "service":"drive", "account_email":principal, "source_uri":remote.source_uri,
                    "remote_id":remote.remote_id, "source_sha256":attachment.id,
                    "source_bytes":attachment.byte_count,
                }),
            )
            .unwrap();
            let version =
                collections::OccurrenceVersion::new(&identity, &remote, attachment.id, origin)
                    .unwrap();
            let occurrence = version.occurrence_id.clone();
            let head =
                collections::begin_refresh(store, &identity, &CommandId::new().to_string(), false)
                    .unwrap();
            let head =
                collections::set_page(store, &private, &head, vec![remote.clone()], None).unwrap();
            let head =
                collections::publish_member(store, &head, version, attachment.byte_count).unwrap();
            let head = collections::finish_page(store, &head).unwrap();
            let head =
                collections::finish_refresh(store, &head, collections::RefreshPhase::Complete)
                    .unwrap();
            (head.snapshot_id, occurrence)
        };
        let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
        let (snapshot, occurrence) = publish(&store, &png);
        let project = store.manifest().project_id;
        let session_id = CommandId::new();
        let source = bind_tokens(
            materials::collections::read_member(&store, &def.id, &snapshot, &occurrence).unwrap(),
            &project.to_string(),
            &session_id.to_string(),
        )
        .unwrap();
        let encoded = source.presentation.unwrap().media[0]
            .preview_token
            .clone()
            .unwrap();
        let request = parse(&format!("loom-asset://localhost/{encoded}").parse().unwrap()).unwrap();
        assert_eq!(request.member.as_ref().unwrap().snapshot_id, snapshot);
        // A refresh replaces the current occurrence, not the retained preview.
        let replacement = temp.path().join("replacement.txt");
        std::fs::write(&replacement, "New source content").unwrap();
        let (new_snapshot, _) = publish(&store, &replacement);
        assert_ne!(new_snapshot, snapshot);
        let state = PluginState::default();
        {
            let mut session = state.session.lock().unwrap();
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(session_id);
            session.store = Some(store);
        }
        assert_eq!(
            read(&state, &request).unwrap().bytes,
            std::fs::read(&png).unwrap()
        );
        let mut retargeted = request.clone();
        retargeted.member.as_mut().unwrap().snapshot_id = new_snapshot;
        assert_eq!(
            read(&state, &retargeted),
            Err(LoomAssetReadFailure::NotFound)
        );
        assert_eq!(
            read(
                &state,
                &Request {
                    session_id: CommandId::new(),
                    ..request.clone()
                }
            ),
            Err(LoomAssetReadFailure::NotFound)
        );
        {
            let mut session = state.session.lock().unwrap();
            let store = session.store.as_mut().unwrap();
            let revision = workspace_template::collection_definitions(store)
                .unwrap()
                .revision_id;
            workspace_template::remove_collection(store, revision, &def.id).unwrap();
        }
        assert_eq!(read(&state, &request), Err(LoomAssetReadFailure::NotFound));
    }

    #[test]
    fn preview_tokens_preserve_identity_and_reject_altered_urls() {
        let request = Request {
            project_id: ProjectId::new(),
            session_id: CommandId::new(),
            material_id: format!("material-{}", "a".repeat(64)),
            media_sha256: "b".repeat(64),
            member: None,
        };
        let encoded = token(&request).unwrap();
        for origin in ["loom-asset://localhost", "http://loom-asset.localhost"] {
            let uri: http::Uri = format!("{origin}/{encoded}").parse().unwrap();
            assert_eq!(parse(&uri), Some(request.clone()));
            for suffix in ["?bypass=1", "/extra", "-extra", "%2f"] {
                assert!(parse(&format!("{origin}/{encoded}{suffix}").parse().unwrap()).is_none());
            }
        }
        for origin in [
            "https://loom-asset.localhost",
            "loom-asset://other",
            "http://localhost",
        ] {
            assert!(parse(&format!("{origin}/{encoded}").parse().unwrap()).is_none());
        }
    }

    #[test]
    #[cfg(unix)]
    fn preview_is_revoked_by_removal_and_cannot_cross_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
        let attachment = context_attachments::import_path(store.root(), &path).unwrap();
        let entry = materials::bind_attachment(&mut store, &attachment.id, None).unwrap();
        let project = store.manifest().project_id;
        let session_id = CommandId::new();
        let source = bind_tokens(
            materials::read(&store, &entry.id).unwrap(),
            &project.to_string(),
            &session_id.to_string(),
        )
        .unwrap();
        let encoded = source.presentation.unwrap().media[0]
            .preview_token
            .clone()
            .unwrap();
        let uri: http::Uri = format!("loom-asset://localhost/{encoded}").parse().unwrap();
        let request = parse(&uri).unwrap();
        let state = PluginState::default();
        {
            let mut session = state.session.lock().unwrap();
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(session_id);
            session.store = Some(store);
        }
        assert_eq!(
            read(&state, &request).unwrap().bytes,
            std::fs::read(path).unwrap()
        );
        assert_eq!(
            read(
                &state,
                &Request {
                    session_id: CommandId::new(),
                    ..request.clone()
                }
            ),
            Err(LoomAssetReadFailure::NotFound)
        );
        assert!(
            parse(
                &format!("loom-asset://localhost/{encoded}?bypass=1")
                    .parse()
                    .unwrap()
            )
            .is_none()
        );
        {
            let mut session = state.session.lock().unwrap();
            materials::remove(session.store.as_mut().unwrap(), &entry.id).unwrap();
        }
        assert_eq!(read(&state, &request), Err(LoomAssetReadFailure::NotFound));
    }
}
