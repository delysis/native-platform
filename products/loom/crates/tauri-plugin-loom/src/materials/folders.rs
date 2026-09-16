//! Bounded retrieval over a frozen set of registered writing revisions.
//! No directory walking, new index, recursive references, or implicit media.
use super::{
    MAX_CONTEXT_CHARS, MAX_HITS, MAX_QUERY_BYTES, MaterialEntry, MaterialEvidence, MaterialKind,
    MaterialSearch, Result, digest, invalid, reference, retain_evidence, select_passages,
};
use crate::document_bindings::FolderSnapshot;
use loom_store::{ProjectStore, StoreError};
use loom_types::DocumentId;
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_MEMBER_BYTES: u64 = 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 8 * 1024 * 1024;

/// One allowance per operation, including repeated reads for different queries.
#[derive(Debug)]
pub(crate) struct FolderScanBudget {
    remaining: std::cell::Cell<u64>,
}

impl Default for FolderScanBudget {
    fn default() -> Self {
        Self {
            remaining: std::cell::Cell::new(MAX_SCAN_BYTES),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FolderRetrieval {
    pub(crate) snapshot: FolderSnapshot,
    pub(crate) scanned_document_ids: Vec<DocumentId>,
    pub(crate) omitted: Vec<FolderOmission>,
    pub(crate) result_limit_reached: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FolderOmission {
    document_id: DocumentId,
    reason: String,
}

impl FolderRetrieval {
    fn omit(&mut self, document_id: DocumentId, reason: &str) {
        self.omitted.push(FolderOmission {
            document_id,
            reason: reason.into(),
        });
    }
}

fn owner(folder: &FolderSnapshot) -> Result<MaterialEntry> {
    let bytes = serde_json::to_vec(&("loom.folder.owner.v1", folder.project_id, &folder.prefix))?;
    Ok(MaterialEntry {
        id: format!("folder-{}", digest(&bytes)),
        name: folder.prefix.trim_end_matches('/').into(),
        reference: reference(&folder.prefix)?,
        kind: MaterialKind::Folder,
        pinned: false,
        available: true,
        source_path: Some(folder.prefix.clone()),
        attachment_id: None,
        workspace_path: None,
    })
}

// Keep coverage accounting and per-member admission in the same bounded loop.
#[allow(clippy::too_many_lines)]
pub(crate) fn search_folder(
    store: &ProjectStore,
    folder: &FolderSnapshot,
    query: &str,
    scan_budget: &FolderScanBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<MaterialSearch> {
    folder
        .validate(store)
        .map_err(|error| invalid(error.message))?;
    if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(invalid("A search needs nonempty text within 4 KiB."));
    }
    let material = owner(folder)?;
    let mut coverage = FolderRetrieval {
        snapshot: folder.clone(),
        scanned_document_ids: Vec::new(),
        omitted: Vec::new(),
        result_limit_reached: false,
    };
    let mut text_remaining = MAX_CONTEXT_CHARS as usize;
    let mut hits = Vec::new();
    let mut complete = true;
    for member in &folder.members {
        if cancelled() {
            return Err(invalid("Folder search cancelled."));
        }
        if hits.len() >= MAX_HITS as usize || text_remaining == 0 {
            coverage.result_limit_reached = true;
            coverage.omit(member.document_id, "result_limit");
            continue;
        }
        let scan_remaining = scan_budget.remaining.get();
        if scan_remaining == 0 {
            coverage.omit(member.document_id, "operation_scan_byte_limit");
            continue;
        }
        let loaded =
            match store.read_document_bounded(&member.path, MAX_MEMBER_BYTES.min(scan_remaining)) {
                Ok(loaded) => loaded,
                Err(StoreError::DocumentTooLarge { .. }) => {
                    coverage.omit(
                        member.document_id,
                        if scan_remaining < MAX_MEMBER_BYTES {
                            "operation_scan_byte_limit"
                        } else {
                            "member_byte_limit"
                        },
                    );
                    continue;
                }
                Err(error) => {
                    return Err(invalid(format!(
                        "Folder member {:?} is unavailable or changed: {error}",
                        member.path
                    )));
                }
            };
        if loaded.document_id != member.document_id || loaded.revision_id != member.revision_id {
            return Err(invalid(
                "The folder changed after this run began. Run again to use its current writing.",
            ));
        }
        scan_budget
            .remaining
            .set(scan_remaining - loaded.text.len() as u64);
        coverage.scanned_document_ids.push(member.document_id);
        let selection = select_passages(
            &loaded.text,
            query,
            MAX_HITS as usize - hits.len(),
            text_remaining,
        );
        complete &= selection.complete;
        coverage.result_limit_reached |= selection.result_limit_reached;
        for passage in selection.passages {
            let text = &loaded.text[passage.range.clone()];
            text_remaining -= text.len();
            hits.push(retain_evidence(store, MaterialEvidence {
                complete: passage.complete,
                warnings: vec!["A selected passage from registered writing, not the complete folder.".into()],
                id: String::new(), reference: String::new(), material_id: material.id.clone(),
                title: member.title.clone(), text: text.into(), source_revision: folder.source_revision.clone(), text_sha256: digest(text.as_bytes()),
                locator: json!({"kind":"document_revision", "project_id":folder.project_id, "document_id":member.document_id,
                    "revision_id":member.revision_id,"blob_id":loaded.blob_id,"artifact_id":loaded.artifact_id,"path":member.path,
                    "start_byte":passage.range.start,"end_byte":passage.range.end}), source_evidence: None,
            })?);
        }
    }
    if cancelled() {
        return Err(invalid("Folder search cancelled."));
    }
    complete &= coverage.omitted.is_empty() && !coverage.result_limit_reached;
    let mut warnings = vec!["Search covers the admitted registered writing only, not arbitrary files or folder history.".into()];
    if folder.excluded > 0 {
        warnings.push(format!(
            "{} configuration or retained-output documents excluded from writing scope.",
            folder.excluded
        ));
    }
    if !complete {
        warnings.push(format!("Bounded retrieval: {} admitted documents were not scanned; selected passages may omit other matches.", coverage.omitted.len()));
    }
    Ok(MaterialSearch {
        source_revision: folder.source_revision.clone(),
        material,
        query: query.into(),
        hits,
        complete,
        warnings,
        folder: Some(coverage),
    })
}

#[cfg(all(test, unix))]
#[path = "folder_tests.rs"]
mod tests;
