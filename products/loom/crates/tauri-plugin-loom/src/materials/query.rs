//! Structured queries retain the whole-scope result and its source snapshot.
use information_native_backend_sqlite::{DocumentCount, DocumentCountRequest};

use super::{
    Deserialize, MaterialEntry, MaterialError, MaterialKind, ProjectStore, Result,
    RetrievalPurpose, Serialize, file_version, grant_key, grants, invalid, resolve,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterialCount {
    pub(crate) material: MaterialEntry,
    pub(crate) source_revision: String,
    pub(crate) result: DocumentCount,
}

impl MaterialCount {
    pub(crate) fn text(&self) -> String {
        format!(
            "{} document {} in the entire library {:?}.\n",
            self.result.value,
            if self.result.value == 1 {
                "record"
            } else {
                "records"
            },
            self.material.name
        )
    }
}

pub(crate) fn count_documents(
    store: &ProjectStore,
    id: &str,
    expected_source_revision: &str,
) -> Result<MaterialCount> {
    let material = resolve(store, id)?;
    if material.kind != MaterialKind::Library {
        return Err(invalid(
            "count supports only an entire supported research library, not selected passages, folders, attachments, or connected collections",
        ));
    }
    let grant = grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .get(&grant_key(store, &material.id))
        .cloned()
        .ok_or_else(|| MaterialError::NeedsAuthorization(material.name.clone()))?;
    if grant.version != expected_source_revision || file_version(&grant.path)? != grant.version {
        return Err(invalid(
            "library changed after run admission; select its current version and start another run",
        ));
    }
    let result = grant
        .backend
        .count_documents(DocumentCountRequest {
            purpose: RetrievalPurpose::ModelContext,
            timeout_ms: 1500,
        })
        .map_err(|error| invalid(format!("{}: {}", error.code, error.safe_message)))?;
    if file_version(&grant.path)? != grant.version {
        return Err(invalid("library changed during count; result discarded"));
    }
    Ok(MaterialCount {
        material,
        source_revision: grant.version.clone(),
        result,
    })
}
