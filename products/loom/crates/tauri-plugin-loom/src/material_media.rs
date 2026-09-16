//! Session- and binding-scoped previews of named retained media.
use super::*;

const VERSION: &str = "m1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Request {
    project_id: ProjectId,
    session_id: CommandId,
    material_id: String,
    media_sha256: String,
}
fn token(request: &Request) -> Option<String> {
    if !materials::is_material_id(&request.material_id)
        || request.media_sha256.len() != 64
        || !request
            .media_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(format!(
        "{VERSION}-{}-{}-{}-{}",
        request.project_id,
        request.session_id,
        request.material_id.strip_prefix("material-")?,
        request.media_sha256
    ))
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
    let request = Request {
        project_id: fields.next()?.parse().ok()?,
        session_id: fields.next()?.parse().ok()?,
        material_id: format!("material-{}", fields.next()?),
        media_sha256: fields.next()?.into(),
    };
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
    if let Some(presentation) = &mut source.presentation {
        for media in &mut presentation.media {
            media.preview_token = token(&Request {
                project_id,
                session_id,
                material_id: source.material.id.clone(),
                media_sha256: media.sha256.clone(),
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
    id: &str,
) -> Result<String, LoomAssetReadFailure> {
    let session = state
        .session
        .lock()
        .map_err(|_| LoomAssetReadFailure::Unavailable)?;
    if session.phase != SessionPhase::Open
        || session.active_session_id != Some(authority.session_id)
    {
        return Err(LoomAssetReadFailure::NotFound);
    }
    let store = session
        .store
        .as_ref()
        .filter(|store| {
            store.manifest().project_id == authority.project_id
                && store.root() == authority.project_root
        })
        .ok_or(LoomAssetReadFailure::NotFound)?;
    materials::resolve(store, id)
        .map_err(|_| LoomAssetReadFailure::NotFound)?
        .attachment_id
        .ok_or(LoomAssetReadFailure::NotFound)
}
pub(super) fn read(
    state: &PluginState,
    request: &Request,
) -> Result<LoadedProtocolAsset, LoomAssetReadFailure> {
    let authority =
        capture_loom_asset_authority_for(state, request.project_id, request.session_id)?;
    let attachment = selected_attachment(state, &authority, &request.material_id)?;
    let media = context_attachments::read_context_media(
        &authority.project_root,
        &attachment,
        &request.media_sha256,
    )
    .map_err(|_| LoomAssetReadFailure::NotFound)?;
    // Recheck both session and binding after bounded filesystem work.
    if selected_attachment(state, &authority, &request.material_id)? != attachment {
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
    #[test]
    fn preview_is_revoked_by_removal_and_cannot_cross_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
        let attachment = context_attachments::import_path(store.root(), &path).unwrap();
        let entry = materials::bind_attachment(&store, &attachment.id, None).unwrap();
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
            let session = state.session.lock().unwrap();
            materials::remove(session.store.as_ref().unwrap(), &entry.id).unwrap();
        }
        assert_eq!(read(&state, &request), Err(LoomAssetReadFailure::NotFound));
    }
}
