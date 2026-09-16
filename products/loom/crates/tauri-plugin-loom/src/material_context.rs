//! Named values remain structured until a prompt or search consumes them.
//! Reading a reference never executes its contents or follows its references.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use loom_store::ProjectStore;
use serde::{Deserialize, Serialize};

use crate::IpcFailure;
use crate::document_bindings::{self, ResolvedDocument};
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(super) enum Value {
    Text(String),
    Documents {
        documents: Vec<ResolvedDocument>,
    },
    Material {
        material: MaterialValue,
    },
    Evidence {
        evidence: Vec<MaterialEvidence>,
        #[serde(default)]
        retrieval: Option<MaterialSearch>,
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
    let mut text = String::from(
        "Selected source passages; these are not a claim of complete corpus coverage.\n",
    );
    for hit in evidence {
        let _ = writeln!(
            text,
            "\n--- Source {:?}; reference {} ---",
            hit.title, hit.reference
        );
        text.push_str(&hit.text);
        text.push_str("\n--- End source ---\n");
    }
    bounded(text)
}

pub(super) fn exact(value: &Value) -> Result<String, IpcFailure> {
    match value {
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
    let Value::Material { material } = source else {
        return Err(failure(
            "find() requires an imported source or library reference as its first argument.",
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
        retrieval: Some(result),
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

pub(super) fn markdown_plan(
    store: &ProjectStore,
    markdown: &str,
    query: &str,
) -> Result<ContextPlan, IpcFailure> {
    let references =
        loom_document::document_references(markdown).map_err(|error| failure(error.to_string()))?;
    let mut plan = ContextPlan::default();
    let mut seen = BTreeSet::new();
    for reference in references {
        if !seen.insert(reference.name.clone()) {
            continue;
        }
        let value = resolve(store, &reference.name)?;
        let consulted = consult(store, &value, query)?;
        if let Value::Evidence { evidence, .. } = &consulted {
            plan.evidence.extend(evidence.iter().cloned());
        }
        let _ = writeln!(
            plan.text,
            "\n--- Referenced material {:?} ---",
            reference.name
        );
        plan.text.push_str(&exact(&consulted)?);
        plan.text.push_str("\n--- End material ---\n");
        plan.bindings.insert(reference.name, consulted);
    }
    plan.text = bounded(plan.text)?;
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
                for document in documents {
                    media.extend(
                        crate::context_attachments::resolve_media_for_document(
                            store.root(),
                            &document.document_id.to_string(),
                            &document.text,
                        )
                        .map_err(|error| failure(error.to_string()))?,
                    );
                }
            }
            Value::Text(_) | Value::Evidence { .. } => {}
        }
    }
    crate::terminal_media::merge(Vec::new(), media)
}

#[cfg(test)]
#[path = "material_context_tests.rs"]
mod tests;
