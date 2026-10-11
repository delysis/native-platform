//! Reviewed scratch context becomes an ordinary collaborative document.
//! One durable publication intent per shared source survives a lost reply.
use super::*;
use crate::context_attachments::publication::{self, Material};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublishRequest {
    source: DocumentId,
    publication: Uuid,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    request: PublishRequest,
    roster_hash: String,
    path: String,
    material: Material,
}

#[derive(Debug, Serialize)]
pub(crate) struct Review {
    request: PublishRequest,
    path: String,
    material: Material,
    members: Vec<String>,
    started: bool,
    published: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Published {
    document_id: String,
    path: String,
    reference: String,
}

fn key(source: DocumentId) -> String {
    format!("context-share:{source}")
}

fn require_source(
    store: &ProjectStore,
    cabal: &Cabal,
    source: DocumentId,
) -> Result<String, IpcFailure> {
    if !cabal.is_member(cabal.identity().public_key()) {
        return Err(failure("This device is no longer a member of the cabal."));
    }
    for view in cabal.views().map_err(failure)? {
        if !view.deleted
            && cabal
                .local_record::<Projection>(&projection_key(view.id))
                .map_err(failure)?
                .is_some_and(|projection| projection.local_id == Some(source))
        {
            let registered = store
                .registered_document(source)
                .map_err(IpcFailure::store)?
                .ok_or_else(|| failure("The context source is unavailable."))?;
            if store
                .document_is_deleted(source)
                .map_err(IpcFailure::store)?
            {
                return Err(failure("The context source was removed."));
            }
            return Ok(registered.relative_path);
        }
    }
    Err(failure(
        "Share the source document with this cabal before publishing its context.",
    ))
}

fn prepare(store: &ProjectStore, cabal: &Cabal, source: DocumentId) -> Result<Review, IpcFailure> {
    let name = require_source(store, cabal, source)?;
    let saved = cabal
        .local_record::<Intent>(&key(source))
        .map_err(failure)?;
    let started = saved.is_some();
    let mut intent = if let Some(saved) = saved {
        saved
    } else {
        let material = publication::material(store.root(), &source.to_string())
            .map_err(|error| IpcFailure::context_attachment(&error))?;
        if material.markdown.trim().is_empty() && material.files.is_empty() {
            return Err(failure("Add some context before sharing it."));
        }
        intent(
            source,
            &name,
            Uuid::new_v4(),
            cabal.roster().hash().map_err(failure)?,
            material,
        )?
    };
    intent.roster_hash = cabal.roster().hash().map_err(failure)?;
    intent.request.fingerprint = fingerprint(&intent)?;
    let published = cabal
        .views()
        .map_err(failure)?
        .iter()
        .any(|view| view.id == intent.request.publication);
    Ok(Review {
        request: intent.request,
        path: intent.path,
        material: intent.material,
        members: cabal
            .roster()
            .payload
            .members
            .iter()
            .map(|member| member.name.clone())
            .collect(),
        started,
        published,
    })
}

fn intent(
    source: DocumentId,
    name: &str,
    publication: Uuid,
    roster_hash: String,
    material: Material,
) -> Result<Intent, IpcFailure> {
    let file = name.rsplit('/').next().unwrap_or("Context");
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let stem = stem
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == ' ')
        .take(40)
        .collect::<String>();
    let stem = if stem.trim().is_empty() {
        "Context"
    } else {
        stem.trim()
    };
    let mut intent = Intent {
        request: PublishRequest {
            source,
            publication,
            fingerprint: String::new(),
        },
        path: format!("Context/{stem}-{}.md", publication.simple()),
        roster_hash,
        material,
    };
    intent.request.fingerprint = fingerprint(&intent)?;
    Ok(intent)
}

fn fingerprint(intent: &Intent) -> Result<String, IpcFailure> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                "loom.context-publication.v1",
                intent.request.source,
                intent.request.publication,
                &intent.path,
                &intent.roster_hash,
                &intent.material
            ))
            .map_err(failure)?
        )
    ))
}

fn publish(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    request: &PublishRequest,
) -> Result<Published, IpcFailure> {
    let name = require_source(store, cabal, request.source)?;
    let saved = cabal
        .local_record::<Intent>(&key(request.source))
        .map_err(failure)?;
    let views = cabal.views().map_err(failure)?;
    let completed = views.iter().any(|view| view.id == request.publication);
    let mut intent = if let Some(saved) = saved {
        if saved.request.source != request.source
            || saved.request.publication != request.publication
        {
            return Err(failure(
                "This document already has a context publication. Reopen its sharing review.",
            ));
        }
        saved
    } else {
        if completed {
            return Err(failure("This publication identity is already in use."));
        }
        let material = publication::material(store.root(), &request.source.to_string())
            .map_err(|error| IpcFailure::context_attachment(&error))?;
        if material.markdown.trim().is_empty() && material.files.is_empty() {
            return Err(failure("Add some context before sharing it."));
        }
        let intent = intent(
            request.source,
            &name,
            request.publication,
            cabal.roster().hash().map_err(failure)?,
            material,
        )?;
        if views.len() >= loom_cabal::MAX_DOCUMENTS
            || store
                .document_path_is_reserved(&intent.path)
                .map_err(IpcFailure::store)?
        {
            return Err(failure(
                "There is no free document slot or path for this shared context.",
            ));
        }
        intent
    };
    let original = intent.request.fingerprint.clone();
    intent.roster_hash = cabal.roster().hash().map_err(failure)?;
    intent.request.fingerprint = fingerprint(&intent)?;
    if request.fingerprint != intent.request.fingerprint
        && !(completed && request.fingerprint == original)
    {
        return Err(failure(
            "The context or cabal membership changed. Review it again before sharing.",
        ));
    }
    if !completed {
        // Persist the exact approved bytes and file identities before any file
        // becomes available to the cabal. Scratch edits cannot change a retry.
        // A new membership requires another review of these retained bytes.
        cabal
            .set_local_record(&key(request.source), &intent)
            .map_err(failure)?;
        for file in &intent.material.files {
            let bytes = publication::file_bytes(store.root(), file)
                .map_err(|error| IpcFailure::context_attachment(&error))?;
            cabal.publish_asset(&file.name, &bytes).map_err(failure)?;
        }
    }
    // Validate the original creation identity on retries too. This is a no-op
    // for an existing creation and preserves later collaborative edits.
    cabal
        .create_document_idempotent(&Create {
            document: request.publication,
            client: request.publication,
            name: intent.path,
            kind: TextKind::Prose,
            text: publication::document(&intent.material),
        })
        .map_err(failure)?;
    assets::prepare_project(store, cabal)?;
    let projected = project_document(store, cabal, request.publication)?;
    let path = projected.local.summary.relative_path;
    Ok(Published {
        document_id: projected.local.summary.document_id,
        reference: format!("@{}", serde_json::to_string(&path).map_err(failure)?),
        path,
    })
}

#[tauri::command]
pub(crate) async fn cabal_context_review(
    project_id: String,
    session_id: String,
    document_id: DocumentId,
    state: State<'_, PluginState>,
) -> Result<Review, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let (shared, _) = state
        .cabals
        .bound(&directory(&state)?, &root)
        .await?
        .ok_or_else(|| failure("This workspace is not a cabal."))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let cabal = shared.lock().map_err(|_| failure("Cabal owner stopped"))?;
    prepare(store, &cabal, document_id)
}

#[tauri::command]
pub(crate) async fn cabal_context_publish(
    project_id: String,
    session_id: String,
    request: PublishRequest,
    state: State<'_, PluginState>,
) -> Result<Published, IpcFailure> {
    ensure_application_running(&state, "context sharing")?;
    let root = root_for(&state, &project_id, &session_id)?;
    let (shared, _) = state
        .cabals
        .bound(&directory(&state)?, &root)
        .await?
        .ok_or_else(|| failure("This workspace is not a cabal."))?;
    let _admission = lock_application_admission(&state, "context sharing")?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let mut cabal = shared.lock().map_err(|_| failure("Cabal owner stopped"))?;
    publish(store, &mut cabal, &request)
}

#[cfg(all(test, unix))]
#[path = "cabal_context_tests.rs"]
mod tests;
