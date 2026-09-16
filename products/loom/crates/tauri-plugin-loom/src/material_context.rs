//! Named values remain structured until a prompt or search consumes them.
//! Reading a reference never executes its contents or follows its references.

use std::collections::{BTreeMap, BTreeSet};

use loom_store::ProjectStore;
use serde::{Deserialize, Serialize};

use crate::IpcFailure;
use crate::document_bindings::{self, ResolvedDocument};
pub(super) use crate::materials::FolderScanBudget;
use crate::materials::{
    self, MaterialEntry, MaterialEvidence, MaterialKind, MaterialRead, MaterialSearch,
};

impl From<materials::MaterialError> for IpcFailure {
    fn from(error: materials::MaterialError) -> Self {
        let code = match &error {
            materials::MaterialError::NotFound(_) => "material_not_found",
            materials::MaterialError::Ambiguous(_) => "material_ambiguous",
            materials::MaterialError::NeedsAuthorization(_) => "material_needs_authorization",
            _ => "material_failed",
        };
        IpcFailure::new(code, error.to_string(), false)
    }
}

const MAX_BYTES: usize = 65_536;
const EVIDENCE_HEADER: &str =
    "Selected source passages; these are not a claim of complete corpus coverage.\n";
const EMPTY_EVIDENCE: &str = "No matching source passages were found.\n";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(super) enum Value {
    Text(String),
    Documents {
        documents: Vec<ResolvedDocument>,
    },
    Folder {
        folder: document_bindings::FolderSnapshot,
    },
    Material {
        material: MaterialValue,
    },
    Evidence {
        evidence: Vec<MaterialEvidence>,
        #[serde(default)]
        retrieval: Option<Box<MaterialSearch>>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct MaterialValue {
    pub material: MaterialEntry,
    pub source_revision: String,
    pub text: Option<String>,
    pub complete: bool,
}

impl From<materials::MaterialAdmission> for MaterialValue {
    fn from(value: materials::MaterialAdmission) -> Self {
        Self {
            material: value.material,
            source_revision: value.source_revision,
            text: value.text,
            complete: value.complete,
        }
    }
}

impl From<MaterialRead> for MaterialValue {
    fn from(read: MaterialRead) -> Self {
        Self {
            material: read.material,
            source_revision: read.source_revision,
            text: (read.text.len() <= MAX_BYTES).then_some(read.text),
            complete: read.complete,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ContextPlan {
    pub text: String,
    pub bindings: BTreeMap<String, Value>,
    pub evidence: Vec<MaterialEvidence>,
    pub byte_budget: usize,
    pub omitted_evidence: BTreeMap<String, Vec<String>>,
}

/// Shared operation bound: overlapping folders cannot multiply admission work.
#[derive(Default)]
pub(super) struct FolderAdmissionBudget {
    documents: BTreeSet<loom_types::DocumentId>,
    bytes: usize,
}

impl FolderAdmissionBudget {
    pub(super) fn admit(&mut self, value: &Value) -> Result<(), IpcFailure> {
        let Value::Folder { folder } = value else {
            return Ok(());
        };
        self.documents
            .extend(folder.members.iter().map(|member| member.document_id));
        self.bytes = self.bytes.saturating_add(
            serde_json::to_vec(folder)
                .map_err(|error| failure(error.to_string()))?
                .len(),
        );
        if self.documents.len() > document_bindings::MAX_FOLDER_DOCUMENTS
            || self.bytes > 1024 * 1024
        {
            return Err(failure(
                "The referenced folders exceed this operation's 1,024-document or 1 MiB membership limit.",
            ));
        }
        Ok(())
    }
}

pub(super) fn evidence_artifact_ids(
    evidence: &[MaterialEvidence],
) -> Result<Vec<loom_types::ArtifactId>, IpcFailure> {
    let mut ids = BTreeSet::new();
    for hit in evidence {
        if hit.locator.get("kind").and_then(serde_json::Value::as_str) == Some("document_revision")
        {
            let value = hit
                .locator
                .get("artifact_id")
                .ok_or_else(|| failure("Document evidence is missing its source artifact."))?;
            ids.insert(
                serde_json::from_value(value.clone())
                    .map_err(|error| failure(error.to_string()))?,
            );
        }
    }
    Ok(ids.into_iter().collect())
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("material_context_invalid", message, false)
}

pub(super) fn bounded(text: String) -> Result<String, IpcFailure> {
    if text.len() > MAX_BYTES {
        return Err(failure(
            "The exact material exceeds 64 KiB. Select a passage or search it with find().",
        ));
    }
    Ok(text)
}

pub(super) fn resolve(store: &ProjectStore, name: &str) -> Result<Value, IpcFailure> {
    if name.ends_with('/') {
        return Ok(Value::Folder {
            folder: document_bindings::snapshot_folder(store, name)?,
        });
    }
    if let Some(evidence) = materials::resolve_evidence_reference(store, name)? {
        return Ok(Value::Evidence {
            evidence: vec![evidence],
            retrieval: None,
        });
    }
    if name.starts_with("materials/") || materials::is_material_id(name) {
        let material = materials::resolve_optional(store, name)?;
        let entry = material.ok_or_else(|| failure(format!("Material @{name} is unavailable.")))?;
        return Ok(Value::Material {
            material: materials::admit(store, &entry.id, MAX_BYTES)?.into(),
        });
    }
    let documents = document_bindings::resolve_references(store, &[name.to_owned()]);
    if let Ok(documents) = &documents
        && documents.iter().any(|document| document.path == name)
    {
        return Ok(Value::Documents {
            documents: documents.clone(),
        });
    }
    let material = materials::resolve_optional(store, name)?;
    match (documents, material) {
        (Ok(documents), Some(_)) if !documents.iter().any(|doc| doc.path == name) => {
            Err(failure(format!(
                "Reference @{name} names both a document and material. Use its full reference."
            )))
        }
        (Ok(documents), _) => Ok(Value::Documents { documents }),
        (Err(error), Some(entry)) if error.code == "document_reference_missing" => {
            Ok(Value::Material {
                material: materials::admit(store, &entry.id, MAX_BYTES)?.into(),
            })
        }
        (Err(error), _) => Err(error),
    }
}

pub(super) fn evidence_text(evidence: &[MaterialEvidence]) -> Result<String, IpcFailure> {
    if evidence.is_empty() {
        return Ok(EMPTY_EVIDENCE.into());
    }
    let mut text = String::from(EVIDENCE_HEADER);
    for hit in evidence {
        text.push_str(&evidence_passage(hit));
    }
    bounded(text)
}

fn evidence_passage(hit: &MaterialEvidence) -> String {
    format!(
        "\n--- Source {:?}; reference {} ---\n{}\n--- End source ---\n",
        hit.title, hit.reference, hit.text
    )
}

pub(super) fn exact(value: &Value) -> Result<String, IpcFailure> {
    match value {
        Value::Folder { .. } => Err(failure(
            "A folder is a collection, not an exact text argument. Use find(@Folder/, \"query\") to select evidence.",
        )),
        Value::Text(text) => bounded(text.clone()),
        Value::Documents { documents } => bounded(
            documents
                .iter()
                .map(|doc| doc.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        Value::Evidence { evidence, .. } => evidence_text(evidence),
        Value::Material { material } => {
            if material.material.kind == MaterialKind::Library {
                return Err(failure(
                    "A library is a collection, not a text value. Use find(@Library, \"query\") to choose evidence.",
                ));
            }
            if !material.complete {
                return Err(failure(
                    "This source was only partially prepared. Select a retained passage instead of passing it as complete text.",
                ));
            }
            material.text.clone().ok_or_else(|| failure("This source is too large for an exact argument. Select a passage or use find()."))
        }
    }
}

pub(super) fn search(
    store: &ProjectStore,
    source: &Value,
    query: &str,
) -> Result<Value, IpcFailure> {
    search_with_cancel(store, source, query, &FolderScanBudget::default(), &|| {
        false
    })
}

pub(super) fn search_with_cancel(
    store: &ProjectStore,
    source: &Value,
    query: &str,
    scan_budget: &FolderScanBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value, IpcFailure> {
    if let Value::Folder { folder } = source {
        let result = materials::search_folder(store, folder, query, scan_budget, cancelled)?;
        return Ok(Value::Evidence {
            evidence: result.hits.clone(),
            retrieval: Some(Box::new(result)),
        });
    }
    let Value::Material { material } = source else {
        return Err(failure(
            "find() requires a folder, imported source, or library reference as its first argument.",
        ));
    };
    if query.trim().is_empty() || query.len() > 4096 {
        return Err(failure("A search needs nonempty text within 4 KiB."));
    }
    let result = materials::search(store, &material.material.id, query)?;
    if result.source_revision != material.source_revision {
        return Err(failure(
            "The material changed after this run began. Run again to use the new source version.",
        ));
    }
    Ok(Value::Evidence {
        evidence: result.hits.clone(),
        retrieval: Some(Box::new(result)),
    })
}

fn query_window(query: &str) -> &str {
    let mut start = query.len().saturating_sub(4096);
    while !query.is_char_boundary(start) {
        start += 1;
    }
    &query[start..]
}

/// Context consultation may retrieve a collection; exact function arguments may not.
pub(super) fn consult(
    store: &ProjectStore,
    value: &Value,
    query: &str,
) -> Result<Value, IpcFailure> {
    match value {
        Value::Folder { .. } => search(store, value, query_window(query)),
        Value::Material { material }
            if material.material.kind == MaterialKind::Library
                || material.text.is_none()
                || !material.complete =>
        {
            search(store, value, query_window(query))
        }
        _ => Ok(value.clone()),
    }
}

#[cfg(all(test, unix))]
pub(super) fn markdown_plan(
    store: &ProjectStore,
    markdown: &str,
    query: &str,
) -> Result<ContextPlan, IpcFailure> {
    markdown_plan_with_budget(store, markdown, query, MAX_BYTES)
}

#[cfg(all(test, unix))]
pub(super) fn consult_with_budget(
    store: &ProjectStore,
    value: &Value,
    query: &str,
    budget: usize,
) -> Result<(Value, Vec<String>), IpcFailure> {
    consult_with_budget_and_cancel(
        store,
        value,
        query,
        budget,
        &FolderScanBudget::default(),
        &|| false,
    )
}

pub(super) fn consult_with_budget_and_cancel(
    store: &ProjectStore,
    value: &Value,
    query: &str,
    budget: usize,
    scan_budget: &FolderScanBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Value, Vec<String>), IpcFailure> {
    let consulted = match value {
        Value::Folder { .. } => {
            search_with_cancel(store, value, query_window(query), scan_budget, cancelled)?
        }
        Value::Material { material }
            if material
                .text
                .as_ref()
                .is_some_and(|text| text.len() > budget) =>
        {
            search(store, value, query_window(query))?
        }
        _ => consult(store, value, query)?,
    };
    let Value::Evidence {
        evidence,
        mut retrieval,
    } = consulted
    else {
        if exact(&consulted)?.len() > budget {
            return Err(failure(
                "The referenced document does not fit the remaining context. Select a smaller passage.",
            ));
        }
        return Ok((consulted, Vec::new()));
    };
    let mut used = if evidence.is_empty() {
        EMPTY_EVIDENCE.len()
    } else {
        EVIDENCE_HEADER.len()
    };
    let mut selected = Vec::new();
    let mut omitted = Vec::new();
    for hit in evidence {
        let size = evidence_passage(&hit).len();
        if used.saturating_add(size) <= budget {
            used += size;
            selected.push(hit);
        } else {
            omitted.push(hit.id);
        }
    }
    if used > budget || (selected.is_empty() && !omitted.is_empty()) {
        return Err(failure(
            "No whole source passage fits the remaining context. Select a smaller passage or shorten the writing prefix.",
        ));
    }
    if !omitted.is_empty()
        && let Some(search) = &mut retrieval
    {
        search.hits.clone_from(&selected);
        search.complete = false;
        search.warnings.push(format!(
            "{} retrieved passages omitted to fit the context budget.",
            omitted.len()
        ));
    }
    Ok((
        Value::Evidence {
            evidence: selected,
            retrieval,
        },
        omitted,
    ))
}

/// Pack ordinary context using whole retained passages. Exact function values
/// keep their independent semantics and are never shortened by this planner.
pub(super) fn markdown_plan_with_budget(
    store: &ProjectStore,
    markdown: &str,
    query: &str,
    byte_budget: usize,
) -> Result<ContextPlan, IpcFailure> {
    let references =
        loom_document::document_references(markdown).map_err(|error| failure(error.to_string()))?;
    let mut plan = ContextPlan {
        byte_budget: byte_budget.min(MAX_BYTES),
        ..ContextPlan::default()
    };
    let mut seen = BTreeSet::new();
    let mut folder_budget = FolderAdmissionBudget::default();
    let scan_budget = FolderScanBudget::default();
    for reference in references {
        if !seen.insert(reference.name.clone()) {
            continue;
        }
        let value = resolve(store, &reference.name)?;
        folder_budget.admit(&value)?;
        let header = format!("\n--- Referenced material {:?} ---\n", reference.name);
        let footer = "\n--- End material ---\n";
        let remaining = plan
            .byte_budget
            .saturating_sub(plan.text.len() + header.len() + footer.len());
        let (consulted, omitted) =
            consult_with_budget_and_cancel(store, &value, query, remaining, &scan_budget, &|| {
                false
            })?;
        if !omitted.is_empty() {
            plan.omitted_evidence
                .insert(reference.name.clone(), omitted);
        }
        if let Value::Evidence { evidence, .. } = &consulted {
            plan.evidence.extend(evidence.iter().cloned());
        }
        plan.text.push_str(&header);
        plan.text.push_str(&exact(&consulted)?);
        plan.text.push_str(footer);
        plan.bindings.insert(reference.name, consulted);
    }
    plan.text = bounded(plan.text)?;
    if plan.text.len() > plan.byte_budget {
        return Err(failure(
            "The source labels exceed the remaining context budget.",
        ));
    }
    Ok(plan)
}

/// Only explicitly named values contribute native media. Retrieved text never
/// imports all media from its collection or follows links in source passages.
pub(super) fn native_media<'a>(
    store: &ProjectStore,
    values: impl IntoIterator<Item = &'a Value>,
) -> Result<Vec<llama_native_types::MediaInput>, IpcFailure> {
    let mut media = Vec::new();
    for value in values {
        match value {
            Value::Material { material } => {
                media.extend(materials::native_media(store, &material.material.id)?);
            }
            Value::Documents { documents } => {
                // A document reference admits its visible media, without
                // that document's private scratch attachments.
                crate::terminal_media::append_references(store, documents, &mut media)?;
            }
            Value::Text(_) | Value::Evidence { .. } | Value::Folder { .. } => {}
        }
    }
    crate::terminal_media::merge(Vec::new(), media)
}

// These integration fixtures require the supported private project store.
#[cfg(all(test, unix))]
#[path = "material_context_tests.rs"]
mod tests;
