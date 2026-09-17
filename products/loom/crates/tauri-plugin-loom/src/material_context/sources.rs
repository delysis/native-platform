//! Source identity travels with values; live sessions grant access to stores.
//! Serialized paths describe provenance and never open or authorize a store.
use std::path::PathBuf;

use super::{
    BTreeSet, Deserialize, FolderScanBudget, IpcFailure, MaterialEntry, MaterialEvidence,
    ProjectStore, Serialize, Value, budget_failure, evidence_artifact_ids, exact, failure,
    materials, resolve_value,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SourceOrigin {
    project_id: loom_types::ProjectId,
    root: PathBuf,
}

impl SourceOrigin {
    fn of(store: &ProjectStore) -> Self {
        Self {
            project_id: store.manifest().project_id,
            root: store.root().to_owned(),
        }
    }

    pub(super) fn matches(&self, store: &ProjectStore) -> bool {
        self.project_id == store.manifest().project_id && self.root == store.root()
    }

    fn wrap(self, value: Value) -> Value {
        Value::Scoped {
            origin: self,
            value: Box::new(value),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ReadContext<'a> {
    pub documents: &'a ProjectStore,
    pub materials: &'a ProjectStore,
}

pub(crate) struct SelectedSource<'a> {
    pub store: &'a ProjectStore,
    pub material: Option<MaterialEntry>,
    pub evidence: Option<MaterialEvidence>,
}

impl<'a> From<&'a ProjectStore> for ReadContext<'a> {
    fn from(store: &'a ProjectStore) -> Self {
        Self {
            documents: store,
            materials: store,
        }
    }
}

impl<'a> ReadContext<'a> {
    pub(crate) fn reference(self, name: &str) -> Result<SelectedSource<'a>, IpcFailure> {
        if let Some((evidence, store)) = self.retained_evidence(name)? {
            return Ok(SelectedSource {
                store,
                material: materials::resolve_optional(store, &evidence.material_id)?,
                evidence: Some(evidence),
            });
        }
        if !materials::is_material_id(name) && !name.starts_with("materials/") {
            return Err(failure(
                "Expected a retained material or evidence reference.",
            ));
        }
        let (material, store) = self.explicit_material(name)?;
        Ok(SelectedSource {
            store,
            material: Some(material),
            evidence: None,
        })
    }

    fn stores(self) -> impl Iterator<Item = &'a ProjectStore> {
        let same_store = SourceOrigin::of(self.documents).matches(self.materials);
        [
            Some(self.documents),
            (!same_store).then_some(self.materials),
        ]
        .into_iter()
        .flatten()
    }

    pub(super) fn explicit_material(
        self,
        name: &str,
    ) -> Result<(MaterialEntry, &'a ProjectStore), IpcFailure> {
        let mut found = None;
        for store in self.stores() {
            if let Some(entry) = materials::resolve_optional(store, name)? {
                if found.is_some() {
                    return Err(failure(
                        "This material reference exists in two source workspaces.",
                    ));
                }
                found = Some((entry, store));
            }
        }
        found.ok_or_else(|| failure(format!("Material @{name} is unavailable.")))
    }

    pub(crate) fn resolve(self, name: &str) -> Result<Value, IpcFailure> {
        let (value, store) = resolve_value(self, name)?;
        Ok(SourceOrigin::of(store).wrap(value))
    }

    fn source(
        self,
        value: &Value,
    ) -> Result<(&'a ProjectStore, &Value, &SourceOrigin), IpcFailure> {
        let Value::Scoped { origin, value } = value else {
            return Err(failure(
                "The referenced value has no admitted source identity.",
            ));
        };
        let store = [self.documents, self.materials]
            .into_iter()
            .find(|store| origin.matches(store))
            .ok_or_else(|| {
                failure("The source belongs to a workspace that is no longer admitted.")
            })?;
        if matches!(value.as_ref(), Value::Scoped { .. }) {
            return Err(failure(
                "A source value cannot contain another source envelope.",
            ));
        }
        Ok((store, value, origin))
    }

    pub(super) fn retained_evidence(
        self,
        name: &str,
    ) -> Result<Option<(MaterialEvidence, &'a ProjectStore)>, IpcFailure> {
        if !name.starts_with("evidence/") {
            return Ok(None);
        }
        let mut found = None;
        for store in self.stores() {
            match materials::resolve_evidence_reference(store, name) {
                Ok(Some(evidence)) if found.is_none() => found = Some((evidence, store)),
                Ok(Some(_)) => {
                    return Err(failure(
                        "This evidence reference exists in two source workspaces.",
                    ));
                }
                Ok(None) | Err(materials::MaterialError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        found
            .map(Some)
            .ok_or_else(|| materials::MaterialError::NotFound(name.into()).into())
    }

    pub(crate) fn search_with_cancel(
        self,
        source: &Value,
        query: &str,
        scan_budget: &FolderScanBudget,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, IpcFailure> {
        let (store, value, origin) = self.source(source)?;
        Ok(origin.clone().wrap(super::search_with_cancel(
            store,
            value,
            query,
            scan_budget,
            cancelled,
        )?))
    }

    pub(crate) fn consult_with_budget_and_cancel(
        self,
        source: &Value,
        query: &str,
        budget: usize,
        scan_budget: &FolderScanBudget,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Value, Vec<String>), IpcFailure> {
        if let Value::Text(_) = source {
            let text = exact(source)?;
            if text.len() > budget {
                return Err(budget_failure(
                    "The exact text does not fit the remaining context.",
                ));
            }
            return Ok((source.clone(), Vec::new()));
        }
        let (store, value, origin) = self.source(source)?;
        let (result, omitted) = super::consult_with_budget_and_cancel(
            store,
            value,
            query,
            budget,
            scan_budget,
            cancelled,
        )?;
        Ok((origin.clone().wrap(result), omitted))
    }

    pub(crate) fn native_media<'v>(
        self,
        values: impl IntoIterator<Item = &'v Value>,
    ) -> Result<Vec<llama_native_types::MediaInput>, IpcFailure> {
        let mut media = Vec::new();
        for value in values {
            if matches!(value, Value::Text(_)) {
                continue;
            }
            let (store, value, _) = self.source(value)?;
            media.extend(super::native_media(store, [value])?);
        }
        crate::terminal_media::merge(Vec::new(), media)
    }
}

/// Only artifacts genuinely owned by the destination may enter its SQL FK list.
/// Foreign identities and exact bytes remain in the scoped values in the receipt.
pub(crate) fn local_artifact_ids<'a>(
    destination: &ProjectStore,
    values: impl IntoIterator<Item = &'a Value>,
) -> Result<Vec<loom_types::ArtifactId>, IpcFailure> {
    let mut ids = BTreeSet::new();
    for value in values {
        if matches!(value, Value::Text(_)) {
            continue;
        }
        let Value::Scoped { origin, value } = value else {
            return Err(failure("A run input has no source identity."));
        };
        if !origin.matches(destination) {
            continue;
        }
        match value.as_ref() {
            Value::Documents { documents } => {
                ids.extend(documents.iter().map(|document| document.artifact_id));
            }
            Value::Evidence { evidence, .. } => {
                // A retained passage may itself describe a foreign document.
                let local = evidence
                    .iter()
                    .filter(|hit| {
                        hit.locator
                            .get("project_id")
                            .and_then(serde_json::Value::as_str)
                            == Some(origin.project_id.to_string().as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                ids.extend(evidence_artifact_ids(&local)?);
            }
            Value::Scoped { .. } => return Err(failure("Nested source identity is invalid.")),
            _ => {}
        }
    }
    Ok(ids.into_iter().collect())
}
