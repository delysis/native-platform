//! Bridge explicitly authored attachment references and cabal-owned blobs.
//! Private acquisition paths and processing receipts never cross the network.
use super::{Cabal, IpcFailure, ProjectStore, failure, shareable};
use crate::context_attachments::shared;

pub(super) fn publish_added(
    store: &ProjectStore,
    cabal: &mut Cabal,
    before: &str,
    after: &str,
) -> Result<(), IpcFailure> {
    if after.len() > loom_cabal::MAX_DOCUMENT_BYTES {
        return Err(failure("Shared document exceeds one MiB"));
    }
    let sources = shared::locally_added(store.root(), before, after)
        .map_err(|error| IpcFailure::context_attachment(&error))?;
    for (name, bytes) in sources {
        cabal.publish_asset(&name, &bytes).map_err(failure)?;
    }
    Ok(())
}

pub(super) fn prepare_project(store: &ProjectStore, cabal: &Cabal) -> Result<(), IpcFailure> {
    let recovery_paths = shared::recovery_paths(store.root())
        .map_err(|error| IpcFailure::context_attachment(&error))?;
    let (private, public): (Vec<_>, Vec<_>) = store
        .list_documents()
        .map_err(IpcFailure::store)?
        .into_iter()
        .partition(|document| {
            !shareable(&document.relative_path) && !recovery_paths.contains(&document.relative_path)
        });
    // Write the scope before projecting any peer-authored text. Unknown/new
    // documents default to this public cache, including after a crash before
    // their ordinary-file registration has been acknowledged.
    let root = shared::configure(
        store.root(),
        cabal.id(),
        private
            .into_iter()
            .map(|document| document.document_id.to_string())
            .collect(),
        public
            .into_iter()
            .map(|document| document.document_id.to_string())
            .collect(),
    )
    .map_err(|error| IpcFailure::context_attachment(&error))?;
    let mut imported = 0;
    let mut imported_bytes = 0;
    for asset in cabal.assets().map_err(failure)? {
        if shared::has_public_copy(&root, &asset.sha256)
            .map_err(|error| IpcFailure::context_attachment(&error))?
        {
            continue;
        }
        if imported >= 4 || (imported > 0 && imported_bytes >= 16 * 1024 * 1024) {
            break;
        }
        let bytes = cabal.asset_bytes(&asset.sha256).map_err(failure)?;
        imported_bytes += bytes.len();
        shared::retain_public(&root, cabal.id(), &asset, &bytes)
            .map_err(|error| IpcFailure::context_attachment(&error))?;
        imported += 1;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "cabal_assets_tests.rs"]
mod tests;
