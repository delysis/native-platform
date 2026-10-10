//! Local reads of retained collection versions; no account or network access.
use super::*;
use crate::connected_collections::{self as storage, CollectionSnapshot, OccurrenceVersion};
use crate::workspace_template::{self, CollectionDefinition};

#[allow(clippy::needless_pass_by_value)] // Consumes storage errors at map_err boundaries.
fn error(error: impl ToString) -> MaterialError {
    invalid(error.to_string())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FrozenCollection {
    pub(crate) material: MaterialEntry,
    pub(crate) snapshot_id: Option<String>,
}

pub(super) fn definitions(store: &ProjectStore) -> Result<Vec<CollectionDefinition>> {
    workspace_template::collection_definitions_current(store)
        .map(|value| value.collections)
        .map_err(|error| invalid(error.message))
}
pub(super) fn entry(store: &ProjectStore, def: &CollectionDefinition) -> Result<MaterialEntry> {
    // A definition is locally readable even before it has any retained members.
    // Availability deliberately says nothing about acquisition authorization.
    let _ = storage::read_head(store, &def.id).map_err(error)?;
    Ok(MaterialEntry {
        id: def.id.clone(),
        name: def.name.clone(),
        reference: reference(&def.id)?,
        kind: MaterialKind::Collection,
        retention: super::MaterialRetention::Ordinary,
        metadata_revision: None,
        pinned: def.pinned,
        available: true,
        source_path: None,
        attachment_id: None,
        workspace_path: def.workspace_path.clone(),
    })
}
pub(crate) fn freeze(store: &ProjectStore, material: MaterialEntry) -> Result<FrozenCollection> {
    Ok(FrozenCollection {
        snapshot_id: storage::read_head(store, &material.id)
            .map_err(error)?
            .map(|head| head.snapshot_id),
        material,
    })
}
fn snapshot(store: &ProjectStore, frozen: &FrozenCollection) -> Result<Option<CollectionSnapshot>> {
    frozen
        .snapshot_id
        .as_ref()
        .map(|id| storage::read_snapshot(store, &frozen.material.id, id).map_err(error))
        .transpose()
}
pub(super) fn read(store: &ProjectStore, material: MaterialEntry) -> Result<MaterialRead> {
    let frozen = freeze(store, material)?;
    Ok(MaterialRead {
        material: frozen.material,
        text: String::new(),
        complete: false,
        source_revision: frozen.snapshot_id.unwrap_or_default(),
        evidence: Vec::new(),
        presentation: None,
        warnings: Vec::new(),
    })
}

#[derive(Serialize)]
pub(crate) struct MemberSummary {
    occurrence_id: String,
    name: String,
    attachment_id: String,
    source_uri: String,
    snapshot_id: String,
}
#[derive(Serialize)]
pub(crate) struct MembersPage {
    snapshot_id: String,
    members: Vec<MemberSummary>,
    total: usize,
    next_offset: Option<usize>,
}
pub(crate) fn members(
    store: &ProjectStore,
    id: &str,
    offset: usize,
    snapshot_id: Option<&str>,
) -> Result<MembersPage> {
    let material = super::resolve(store, id)?;
    if material.kind != MaterialKind::Collection {
        return Err(invalid("This source is not a collection."));
    }
    let mut frozen = freeze(store, material)?;
    if let Some(id) = snapshot_id {
        frozen.snapshot_id = Some(id.into());
    }
    let Some(snapshot) = snapshot(store, &frozen)? else {
        return Ok(MembersPage {
            snapshot_id: String::new(),
            members: Vec::new(),
            total: 0,
            next_offset: None,
        });
    };
    let total = snapshot.members.len();
    if offset > total {
        return Err(invalid("This collection page is out of range."));
    }
    let end = total.min(offset.saturating_add(50));
    let members = snapshot.members[offset..end]
        .iter()
        .map(|member| MemberSummary {
            occurrence_id: member.occurrence_id.clone(),
            name: member.remote.name.clone(),
            attachment_id: member.attachment_id.clone(),
            source_uri: member.remote.source_uri.clone(),
            snapshot_id: snapshot.id.clone(),
        })
        .collect();
    Ok(MembersPage {
        snapshot_id: snapshot.id,
        members,
        total,
        next_offset: (end < total).then_some(end),
    })
}

pub(crate) fn read_member(
    store: &ProjectStore,
    id: &str,
    snapshot_id: &str,
    occurrence_id: &str,
) -> Result<MaterialRead> {
    let mut material = super::resolve(store, id)?;
    if material.kind != MaterialKind::Collection {
        return Err(invalid("This source is not a collection."));
    }
    let snapshot = storage::read_snapshot(store, id, snapshot_id).map_err(error)?;
    let member = snapshot
        .members
        .iter()
        .find(|member| member.occurrence_id == occurrence_id)
        .ok_or_else(|| invalid("The selected source is absent from that collection version."))?;
    let (presentation, text) =
        context_attachments::read_source(store.root(), &member.attachment_id)?;
    let evidence = evidence(
        store,
        &material,
        &snapshot.id,
        member,
        &presentation,
        &text,
        0..text.len(),
        presentation.coverage_complete,
    )?;
    material.name.clone_from(&member.remote.name);
    material.kind = MaterialKind::Attachment;
    material.attachment_id = Some(member.attachment_id.clone());
    Ok(MaterialRead {
        material,
        text,
        complete: presentation.coverage_complete,
        warnings: presentation.warnings.clone(),
        source_revision: presentation.source_revision.clone(),
        evidence: vec![evidence],
        presentation: Some(presentation),
    })
}

// Keep source coordinates adjacent to the exact payload selected from them.
#[allow(clippy::too_many_arguments)]
fn evidence(
    store: &ProjectStore,
    material: &MaterialEntry,
    snapshot_id: &str,
    member: &OccurrenceVersion,
    presentation: &ContextAttachmentPresentation,
    source: &str,
    range: std::ops::Range<usize>,
    complete: bool,
) -> Result<MaterialEvidence> {
    let text = &source[range.clone()];
    retain_evidence(
        store,
        MaterialEvidence {
            id: String::new(),
            reference: String::new(),
            material_id: material.id.clone(),
            retention: material.retention,
            title: member.remote.name.clone(),
            complete,
            warnings: presentation.warnings.clone(),
            text: text.into(),
            text_sha256: digest(text.as_bytes()),
            source_revision: presentation.source_revision.clone(),
            locator: json!({"kind":"collection_attachment", "collection_snapshot":snapshot_id,"occurrence_id":member.occurrence_id,
            "attachment_id":member.attachment_id,"origin_receipt_id":member.origin_receipt_id,"source_uri":member.remote.source_uri,
            "start_byte":range.start,"end_byte":range.end,
            "pdf_pages":presentation.pdf_pages.iter().filter(|page| page.start_byte < range.end && page.end_byte > range.start).collect::<Vec<_>>() }),
            source_evidence: None,
        },
    )
}

pub(crate) fn search(
    store: &ProjectStore,
    frozen: &FrozenCollection,
    query: &str,
    budget: &FolderScanBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<MaterialSearch> {
    if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(invalid("Search needs 1–4096 bytes of text."));
    }
    let snapshot = snapshot(store, frozen)?;
    let mut hits = Vec::new();
    let mut remaining = MAX_CONTEXT_CHARS as usize;
    let mut omitted = Vec::new();
    let mut complete = true;
    if let Some(snapshot) = &snapshot {
        for member in &snapshot.members {
            if cancelled() {
                return Err(invalid("Collection search stopped."));
            }
            if hits.len() >= MAX_HITS as usize || remaining == 0 {
                omitted.push(member.occurrence_id.clone());
                continue;
            }
            let description =
                context_attachments::describe_source(store.root(), &member.attachment_id)?;
            if description.text_bytes > budget.remaining() {
                omitted.push(member.occurrence_id.clone());
                continue;
            }
            budget.charge(description.text_bytes)?;
            let (presentation, text) =
                context_attachments::read_source(store.root(), &member.attachment_id)?;
            if text.len() as u64 != description.text_bytes
                || presentation.source_revision != description.source_revision
            {
                return Err(invalid("The retained source changed while being read."));
            }
            let selection =
                select_passages(&text, query, MAX_HITS as usize - hits.len(), remaining);
            complete &= selection.complete && presentation.coverage_complete;
            for passage in selection.passages {
                remaining -= passage.range.len();
                hits.push(evidence(
                    store,
                    &frozen.material,
                    &snapshot.id,
                    member,
                    &presentation,
                    &text,
                    passage.range,
                    passage.complete && presentation.coverage_complete,
                )?);
            }
        }
    }
    if cancelled() {
        return Err(invalid("Collection search stopped."));
    }
    let mut warnings = vec![
        "Search covers retained local sources, not an exhaustive or live account view.".into(),
    ];
    if !omitted.is_empty() {
        warnings.push(format!(
            "{} retained sources were omitted by the retrieval budget.",
            omitted.len()
        ));
    }
    Ok(MaterialSearch {
        material: frozen.material.clone(),
        source_revision: frozen.snapshot_id.clone().unwrap_or_default(),
        query: query.into(),
        hits,
        complete: complete && omitted.is_empty(),
        warnings,
        folder: None,
        collection: Some(CollectionRetrieval {
            snapshot_id: frozen.snapshot_id.clone(),
            omitted_occurrence_ids: omitted,
        }),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CollectionRetrieval {
    snapshot_id: Option<String>,
    omitted_occurrence_ids: Vec<String>,
}
