//! Named library entries are mutable; an applied context owns its immutable
//! source revision and generation profile, independent of the library entry.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atomic_write_file::AtomicWriteFile;
use desktop_generation_policy::GenerationTask;
use loom_config::{FrozenGenerationProfile, MineConfig};
use loom_types::CommandId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use workspace_document::{
    DocumentLineage, DocumentSelection, DocumentSnapshot, PartContent, PartKind, RevisionLineage,
    SnapshotPart, SourceKind, SourceLineage, SourceReference,
};

#[cfg(test)]
use crate::context_attachments::set_document_context_snapshot;
use crate::context_attachments::{
    ContextAttachmentError, ContextTextSourcePresentation, DocumentContextSnapshot,
    applied_co_writer, document_context_snapshot, set_document_context_with_co_writer,
};

const PROFILE_SCHEMA: &str = "loom.co-writer-profiles.v2";
const CONTEXT_PROFILE_SCHEMA: &str = "loom.co-writer-profiles.v1";
const MAX_PROFILES: usize = 64;
const MAX_NAME_BYTES: usize = 96;
const MAX_MARKDOWN_BYTES: usize = 256 * 1024;
const MAX_STORE_BYTES: usize = 40 * 1024 * 1024;
const CONFIGURED_PREFIX: &str = "configured-";
static PROFILE_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextSources {
    pub(crate) attachment_ids: Vec<String>,
    pub(crate) text_sources: Vec<ContextTextSourcePresentation>,
}

/// This whole payload is copied into the target's existing atomic context
/// record. Editing or removing the library entry cannot change that copy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppliedCoWriter {
    pub(crate) context: DocumentSnapshot<ContextSources, ()>,
    pub(crate) generation: FrozenGenerationProfile,
}

impl AppliedCoWriter {
    pub(crate) fn validate(&self) -> Result<(), CoWriterError> {
        self.generation.validate()?;
        if self.context.selection() != &DocumentSelection::Sequence
            || self.context.parts().len() != 1
            || self.context.lineage().source.is_none()
            || self.context.lineage().revision.is_none()
            || self.context.metadata().attachment_ids.len() > 32
            || self
                .context
                .metadata()
                .attachment_ids
                .iter()
                .any(|id| !is_sha256(id))
            || self.context.metadata().text_sources.len() > 256
        {
            return Err(CoWriterError::Invalid);
        }
        let part = &self.context.parts()[0];
        if part.kind != PartKind::Text || part.parent_id.is_some() || part.source.start_byte != 0 {
            return Err(CoWriterError::Invalid);
        }
        if part.source.kind != SourceKind::Artifact
            || self
                .context
                .lineage()
                .revision
                .as_ref()
                .is_none_or(|revision| revision.revision_id != part.source.occurrence_id)
        {
            return Err(CoWriterError::Invalid);
        }
        match &part.content {
            PartContent::Inline(text) if text.len() <= MAX_MARKDOWN_BYTES => Ok(()),
            _ => Err(CoWriterError::Invalid),
        }
    }

    pub(crate) fn markdown(&self) -> Result<&str, CoWriterError> {
        self.validate()?;
        match &self.context.parts()[0].content {
            PartContent::Inline(text) => Ok(text),
            PartContent::Source => Err(CoWriterError::Invalid),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenCoWriterProfile {
    id: String,
    name: String,
    frozen: AppliedCoWriter,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
}

/// Current-main libraries recorded context only. Keep that distinction until
/// an explicit save or apply freezes a generation profile; a read cannot
/// manufacture historical settings or a source revision that never existed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContextCoWriterProfile {
    id: String,
    name: String,
    source_document_id: String,
    markdown: String,
    attachment_ids: Vec<String>,
    #[serde(default)]
    text_sources: Vec<ContextTextSourcePresentation>,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum StoredCoWriterProfile {
    Frozen(Box<FrozenCoWriterProfile>),
    Context(ContextCoWriterProfile),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CoWriterProfiles {
    schema: String,
    profiles: BTreeMap<String, StoredCoWriterProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CoWriterSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) source_document_id: String,
    pub(crate) context_bytes: u64,
    pub(crate) attachment_count: u32,
    pub(crate) created_at_unix_ms: i64,
    pub(crate) updated_at_unix_ms: i64,
    pub(crate) configured: bool,
}

#[derive(Debug, Error)]
pub(crate) enum CoWriterError {
    #[error("a co-writer name must be 1 to 96 bytes and contain no control characters")]
    InvalidName,
    #[error("the selected completion context is empty")]
    Empty,
    #[error("the project exceeds the bounded co-writer library limit")]
    Limit,
    #[error("the selected co-writer does not exist")]
    NotFound,
    #[error(
        "the co-writer store is invalid or uses an incompatible format; its original file has been preserved"
    )]
    Invalid,
    #[error("configured co-writers are edited or removed in .mine.toml")]
    Configured,
    #[error("co-writer storage state is unavailable")]
    State,
    #[error("completion context failed: {0}")]
    Context(#[from] ContextAttachmentError),
    #[error("co-writer storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("co-writer metadata failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("co-writer document snapshot failed: {0}")]
    Snapshot(#[from] workspace_document::SnapshotError),
    #[error("co-writer configuration failed: {0}")]
    Configuration(#[from] loom_config::ConfigError),
}

pub(crate) fn list(project_root: &Path) -> Result<Vec<CoWriterSummary>, CoWriterError> {
    let mut summaries = read_profiles(project_root)?
        .profiles
        .values()
        .map(profile_summary)
        .collect::<Result<Vec<_>, _>>()?;
    // Workspace settings report their own errors. A broken configured persona
    // must not hide or prevent deletion of independent saved library entries.
    let Ok(config) = MineConfig::read(project_root) else {
        return Ok(summaries);
    };
    for (name, definition) in &config.profiles {
        if definition.context_file.is_none() {
            continue;
        }
        let Ok(frozen) =
            config.freeze_named(project_root, GenerationTask::ManualWriting, Some(name))
        else {
            continue;
        };
        summaries.push(CoWriterSummary {
            id: format!("{CONFIGURED_PREFIX}{name}"),
            name: name.clone(),
            source_document_id: format!(".mine.toml#profiles.{name}"),
            context_bytes: frozen.context_text().len() as u64,
            attachment_count: 0,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            configured: true,
        });
    }
    Ok(summaries)
}

pub(crate) fn save_from_document(
    project_root: &Path,
    document_id: &str,
    name: &str,
    now_unix_ms: i64,
) -> Result<CoWriterSummary, CoWriterError> {
    let _guard = PROFILE_WRITE_LOCK
        .lock()
        .map_err(|_| CoWriterError::State)?;
    let name = validate_name(name)?;
    let snapshot = document_context_snapshot(project_root, document_id)?;
    if snapshot.markdown.trim().is_empty() && snapshot.attachments.is_empty() {
        return Err(CoWriterError::Empty);
    }
    let generation = if let Some(applied) = applied_co_writer(project_root, document_id)? {
        applied.generation
    } else {
        MineConfig::read(project_root)?.freeze(project_root, GenerationTask::ManualWriting)?
    };
    let id = profile_id(&name);
    let mut store = read_profiles(project_root)?;
    if !store.profiles.contains_key(&id) && store.profiles.len() >= MAX_PROFILES {
        return Err(CoWriterError::Limit);
    }
    let previous = store.profiles.get(&id);
    let created_at_unix_ms = previous
        .map(profile_summary)
        .transpose()?
        .map_or(now_unix_ms, |profile| profile.created_at_unix_ms);
    let parent_revision = previous
        .and_then(|profile| match profile {
            StoredCoWriterProfile::Frozen(profile) => {
                profile.frozen.context.lineage().revision.as_ref()
            }
            StoredCoWriterProfile::Context(_) => None,
        })
        .map(|revision| revision.revision_id.clone());
    let frozen = freeze_context(
        &id,
        document_id,
        snapshot.markdown,
        snapshot
            .attachments
            .into_iter()
            .map(|attachment| attachment.id)
            .collect(),
        snapshot.text_sources,
        generation,
        parent_revision,
    )?;
    let profile = StoredCoWriterProfile::Frozen(Box::new(FrozenCoWriterProfile {
        id: id.clone(),
        name,
        frozen,
        created_at_unix_ms,
        updated_at_unix_ms: now_unix_ms,
    }));
    let summary = profile_summary(&profile)?;
    store.profiles.insert(id, profile);
    write_profiles(project_root, &store)?;
    Ok(summary)
}

fn freeze_context(
    id: &str,
    source_document_id: &str,
    markdown: String,
    attachment_ids: Vec<String>,
    text_sources: Vec<ContextTextSourcePresentation>,
    generation: FrozenGenerationProfile,
    parent_revision_id: Option<String>,
) -> Result<AppliedCoWriter, CoWriterError> {
    let revision_id = CommandId::new().to_string();
    let part = SnapshotPart {
        id: "context".into(),
        parent_id: None,
        kind: PartKind::Text,
        source: SourceReference {
            kind: SourceKind::Artifact,
            occurrence_id: revision_id.clone(),
            start_byte: 0,
            end_byte: markdown.len() as u64,
        },
        content: PartContent::Inline(markdown),
        metadata: (),
    };
    let context = DocumentSnapshot::new(
        id.into(),
        DocumentSelection::Sequence,
        DocumentLineage {
            revision: Some(RevisionLineage {
                revision_id,
                parent_revision_id,
            }),
            source: Some(SourceLineage {
                document_id: source_document_id.into(),
                part_id: None,
            }),
        },
        ContextSources {
            attachment_ids,
            text_sources,
        },
        vec![part],
    )?;
    let frozen = AppliedCoWriter {
        context,
        generation,
    };
    frozen.validate()?;
    Ok(frozen)
}

pub(crate) fn apply_to_document(
    project_root: &Path,
    document_id: &str,
    profile_id: &str,
) -> Result<DocumentContextSnapshot, CoWriterError> {
    let frozen = if let Some(name) = profile_id.strip_prefix(CONFIGURED_PREFIX) {
        let config = MineConfig::read(project_root)?;
        let mut generation =
            config.freeze_named(project_root, GenerationTask::ManualWriting, Some(name))?;
        if generation.context.is_none() {
            return Err(CoWriterError::Empty);
        }
        let markdown = generation.context_text().to_owned();
        generation.context_in_document = true;
        freeze_context(
            profile_id,
            &format!(".mine.toml#profiles.{name}"),
            markdown,
            vec![],
            vec![],
            generation,
            None,
        )?
    } else {
        let profile = read_profiles(project_root)?
            .profiles
            .remove(profile_id)
            .ok_or(CoWriterError::NotFound)?;
        match profile {
            StoredCoWriterProfile::Frozen(profile) => profile.frozen,
            StoredCoWriterProfile::Context(profile) => {
                // These are the settings at this explicit application, not
                // settings attributed to the original context-only save.
                let generation = MineConfig::read(project_root)?
                    .freeze(project_root, GenerationTask::ManualWriting)?;
                freeze_context(
                    &profile.id,
                    &profile.source_document_id,
                    profile.markdown,
                    profile.attachment_ids,
                    profile.text_sources,
                    generation,
                    None,
                )?
            }
        }
    };
    frozen.validate()?;
    set_document_context_with_co_writer(project_root, document_id, &frozen).map_err(Into::into)
}

pub(crate) fn delete(
    project_root: &Path,
    profile_id: &str,
) -> Result<Vec<CoWriterSummary>, CoWriterError> {
    if profile_id.starts_with(CONFIGURED_PREFIX) {
        return Err(CoWriterError::Configured);
    }
    let _guard = PROFILE_WRITE_LOCK
        .lock()
        .map_err(|_| CoWriterError::State)?;
    // Validate the returned projection before committing a deletion. A bad
    // configured entry must not turn a successful write into a reported error.
    let mut summaries = list(project_root)?;
    let mut store = read_profiles(project_root)?;
    if store.profiles.remove(profile_id).is_none() {
        return Err(CoWriterError::NotFound);
    }
    write_profiles(project_root, &store)?;
    summaries.retain(|profile| profile.id != profile_id);
    Ok(summaries)
}

fn validate_name(name: &str) -> Result<String, CoWriterError> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(CoWriterError::InvalidName);
    }
    Ok(name.to_owned())
}

fn profile_id(name: &str) -> String {
    format!(
        "cowriter-{:x}",
        Sha256::digest(name.to_lowercase().as_bytes())
    )
}

fn profile_summary(profile: &StoredCoWriterProfile) -> Result<CoWriterSummary, CoWriterError> {
    Ok(match profile {
        StoredCoWriterProfile::Context(profile) => CoWriterSummary {
            id: profile.id.clone(),
            name: profile.name.clone(),
            source_document_id: profile.source_document_id.clone(),
            context_bytes: profile.markdown.len() as u64,
            attachment_count: u32::try_from(profile.attachment_ids.len()).unwrap_or(u32::MAX),
            created_at_unix_ms: profile.created_at_unix_ms,
            updated_at_unix_ms: profile.updated_at_unix_ms,
            configured: false,
        },
        StoredCoWriterProfile::Frozen(profile) => CoWriterSummary {
            id: profile.id.clone(),
            name: profile.name.clone(),
            source_document_id: profile
                .frozen
                .context
                .lineage()
                .source
                .as_ref()
                .ok_or(CoWriterError::Invalid)?
                .document_id
                .clone(),
            context_bytes: profile.frozen.markdown()?.len() as u64,
            attachment_count: u32::try_from(profile.frozen.context.metadata().attachment_ids.len())
                .unwrap_or(u32::MAX),
            created_at_unix_ms: profile.created_at_unix_ms,
            updated_at_unix_ms: profile.updated_at_unix_ms,
            configured: false,
        },
    })
}

fn read_profiles(project_root: &Path) -> Result<CoWriterProfiles, CoWriterError> {
    let path = profile_path(project_root)?;
    let Some(bytes) = crate::private_sidecar::read(project_root, &path, MAX_STORE_BYTES as u64)?
    else {
        return Ok(CoWriterProfiles {
            schema: PROFILE_SCHEMA.into(),
            profiles: BTreeMap::new(),
        });
    };
    let mut store: CoWriterProfiles = serde_json::from_slice(&bytes)?;
    if !matches!(
        store.schema.as_str(),
        PROFILE_SCHEMA | CONTEXT_PROFILE_SCHEMA
    ) || store.profiles.len() > MAX_PROFILES
    {
        return Err(CoWriterError::Invalid);
    }
    for (id, profile) in &store.profiles {
        let summary = profile_summary(profile)?;
        if id != &summary.id
            || validate_name(&summary.name).is_err()
            || profile_id(&summary.name) != summary.id
        {
            return Err(CoWriterError::Invalid);
        }
        match profile {
            StoredCoWriterProfile::Frozen(profile) => {
                if store.schema == CONTEXT_PROFILE_SCHEMA
                    || profile.frozen.context.document_id() != id
                {
                    return Err(CoWriterError::Invalid);
                }
                profile.frozen.validate()?;
            }
            StoredCoWriterProfile::Context(profile) => {
                if profile.markdown.len() > MAX_MARKDOWN_BYTES
                    || profile.attachment_ids.len() > 32
                    || profile.attachment_ids.iter().any(|id| !is_sha256(id))
                    || profile.text_sources.len() > 256
                {
                    return Err(CoWriterError::Invalid);
                }
            }
        }
    }
    // Only an explicit mutation writes the canonical envelope. Context-only
    // entries retain their original fields, with no invented frozen profile.
    store.schema = PROFILE_SCHEMA.into();
    Ok(store)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_profiles(project_root: &Path, store: &CoWriterProfiles) -> Result<(), CoWriterError> {
    let bytes = serde_json::to_vec_pretty(store)?;
    if bytes.len() > MAX_STORE_BYTES {
        return Err(CoWriterError::Limit);
    }
    let path = profile_path(project_root)?;
    let namespace = crate::private_sidecar::namespace(project_root, &path)?;
    let stored =
        crate::private_sidecar::PayloadCodec::open(project_root)?.seal(&namespace, &bytes)?;
    let mut file = AtomicWriteFile::options().open(&path)?;
    file.write_all(&stored)?;
    file.commit()?;
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = fs::metadata(parent)?;
    }
    Ok(())
}

fn profile_path(project_root: &Path) -> Result<PathBuf, CoWriterError> {
    let loom = project_root.join(".loom");
    match fs::symlink_metadata(&loom) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(CoWriterError::Invalid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&loom)?,
        Err(error) => return Err(error.into()),
    }
    let path = loom.join("co-writers.json");
    if fs::symlink_metadata(&path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(CoWriterError::Invalid);
    }
    Ok(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use desktop_generation_policy::SamplingOverrides;

    #[test]
    #[cfg(unix)]
    fn secured_co_writers_reopen_and_apply_exact_frozen_context_without_plaintext() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join(".loom")).unwrap();
        desktop_vault::ProjectVault::initialize_with_key(project.path(), [41; 32]).unwrap();
        let source = "Private co-writer cadence: precise, patient, and spare.";
        set_document_context_snapshot(project.path(), "source", source, &[]).unwrap();
        let saved = save_from_document(project.path(), "source", "Private Voice", 1).unwrap();
        let path = profile_path(project.path()).unwrap();
        let sealed = fs::read(&path).unwrap();
        assert!(sealed.starts_with(b"MINEENC\x01"));
        assert!(
            !sealed
                .windows(source.len())
                .any(|window| window == source.as_bytes())
        );
        assert!(serde_json::from_slice::<serde_json::Value>(&sealed).is_err());
        assert_eq!(list(project.path()).unwrap()[0].id, saved.id);
        set_document_context_snapshot(project.path(), "source", "Later source edit.", &[]).unwrap();
        apply_to_document(project.path(), "target", &saved.id).unwrap();
        assert_eq!(
            document_context_snapshot(project.path(), "target")
                .unwrap()
                .markdown,
            source
        );
        assert_eq!(fs::read(&path).unwrap(), sealed);

        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        fs::write(&path, tampered).unwrap();
        assert!(list(project.path()).is_err());
        let plaintext = crate::private_sidecar::PayloadCodec::open(project.path())
            .unwrap()
            .open_bytes(".loom/co-writers.json", &sealed, MAX_STORE_BYTES as u64)
            .unwrap();
        fs::write(&path, &plaintext).unwrap();
        assert!(list(project.path()).is_err());
        assert!(delete(project.path(), &saved.id).is_err());
        assert_eq!(fs::read(path).unwrap(), plaintext);
    }

    /// The fields emitted by current main's v1 writer, independent of the new
    /// codec. A byte comparison catches accidental writes while reading it.
    fn write_context_library(
        project: &Path,
        names: &[&str],
        snapshot: &DocumentContextSnapshot,
    ) -> (PathBuf, Vec<u8>) {
        let profiles = names
            .iter()
            .map(|name| {
                let id = profile_id(name);
                let value = serde_json::json!({
                    "id": id,
                    "name": name,
                    "source_document_id": "source",
                    "markdown": snapshot.markdown,
                    "attachment_ids": snapshot.attachments.iter().map(|item| &item.id).collect::<Vec<_>>(),
                    "text_sources": snapshot.text_sources,
                    "created_at_unix_ms": 10,
                    "updated_at_unix_ms": 20
                });
                (id, value)
            })
            .collect::<BTreeMap<_, _>>();
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "loom.co-writer-profiles.v1",
            "profiles": profiles
        }))
        .unwrap();
        let path = project.join(".loom/co-writers.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    #[test]
    fn current_main_library_reads_and_applies_without_rewriting_or_historical_settings() {
        use crate::context_attachments::{add_document_context_snapshot, import_path};

        let project = tempfile::tempdir().unwrap();
        let text_path = project.path().join("voice.md");
        fs::write(&text_path, "Retained source evidence.").unwrap();
        let text = import_path(project.path(), &text_path).unwrap();
        let snapshot = add_document_context_snapshot(project.path(), "source", &[text.id]).unwrap();
        let snapshot = crate::context_attachments::set_document_context_snapshot_with_sources(
            project.path(),
            "source",
            "  Original α.\r\n",
            &[],
            Some(&snapshot.text_sources),
        )
        .unwrap();
        let (path, source_bytes) = write_context_library(project.path(), &["Voice"], &snapshot);
        let choices = list(project.path()).unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].source_document_id, "source");
        assert_eq!(choices[0].context_bytes, snapshot.markdown.len() as u64);
        assert_eq!(choices[0].created_at_unix_ms, 10);
        assert_eq!(fs::read(&path).unwrap(), source_bytes);

        fs::write(
            project.path().join(".mine.toml"),
            "[generation]\nmanual_writing='voice'\n[profiles.voice.sampling]\ntemperature=0.375",
        )
        .unwrap();
        let applied = apply_to_document(project.path(), "target", &choices[0].id).unwrap();
        assert_eq!(applied.markdown, snapshot.markdown);
        assert_eq!(applied.text_sources, snapshot.text_sources);
        assert_eq!(fs::read(&path).unwrap(), source_bytes);
        let frozen = applied_co_writer(project.path(), "target")
            .unwrap()
            .unwrap();
        assert_eq!(frozen.generation.sampling.temperature, Some(0.375));
        assert!(
            frozen
                .context
                .lineage()
                .revision
                .as_ref()
                .unwrap()
                .parent_revision_id
                .is_none()
        );
        assert!(matches!(
            read_profiles(project.path())
                .unwrap()
                .profiles
                .remove(&choices[0].id)
                .unwrap(),
            StoredCoWriterProfile::Context(_)
        ));
        fs::write(project.path().join(".mine.toml"), "invalid=true").unwrap();
        let (retained, _) = crate::generation_profiles::freeze_for_document(
            project.path(),
            "target",
            GenerationTask::AutomaticProse,
        )
        .unwrap();
        assert_eq!(retained.sampling.temperature, Some(0.375));
    }

    #[test]
    fn explicit_save_updates_one_current_main_entry_without_inventing_other_profiles() {
        let project = tempfile::tempdir().unwrap();
        let snapshot =
            set_document_context_snapshot(project.path(), "source", "Original.\r\n", &[]).unwrap();
        let (path, _) = write_context_library(project.path(), &["Voice", "Untouched"], &snapshot);
        set_document_context_snapshot(project.path(), "source", "Edited.\r\n", &[]).unwrap();
        let updated = save_from_document(project.path(), "source", "voice", 30).unwrap();
        assert_eq!(updated.id, profile_id("Voice"));
        assert_eq!(updated.created_at_unix_ms, 10);
        assert_eq!(updated.updated_at_unix_ms, 30);
        let disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(disk["schema"], PROFILE_SCHEMA);
        let untouched = &disk["profiles"][profile_id("Untouched")];
        assert_eq!(untouched["markdown"], "Original.\r\n");
        assert_eq!(untouched["updated_at_unix_ms"], 20);
        assert!(untouched.get("frozen").is_none());
        let stored = read_profiles(project.path()).unwrap();
        let StoredCoWriterProfile::Frozen(updated) = &stored.profiles[&updated.id] else {
            panic!("explicit save must freeze its source");
        };
        assert!(
            updated
                .frozen
                .context
                .lineage()
                .revision
                .as_ref()
                .unwrap()
                .parent_revision_id
                .is_none()
        );
        assert_eq!(list(project.path()).unwrap().len(), 2);

        // Invalid settings do not stop an independent saved-library deletion.
        fs::write(project.path().join(".mine.toml"), "invalid=true").unwrap();
        let remaining = delete(project.path(), &profile_id("voice")).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].name, "Untouched");
        let disk: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(&disk["profiles"][profile_id("Untouched")], untouched);
    }

    #[test]
    fn invalid_configured_persona_does_not_hide_saved_or_valid_configured_entries() {
        let project = tempfile::tempdir().unwrap();
        set_document_context_snapshot(project.path(), "source", "Saved context.", &[]).unwrap();
        let saved = save_from_document(project.path(), "source", "Saved", 1).unwrap();
        fs::create_dir_all(project.path().join(".mine/personas")).unwrap();
        fs::write(
            project.path().join(".mine/personas/valid.md"),
            "Configured context.",
        )
        .unwrap();
        fs::write(project.path().join(".mine.toml"), "[profiles.valid]\ncontext_file='.mine/personas/valid.md'\n[profiles.missing]\ncontext_file='.mine/personas/missing.md'").unwrap();
        let choices = list(project.path()).unwrap();
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().any(|item| item.id == saved.id));
        assert!(choices.iter().any(|item| item.id == "configured-valid"));
        assert!(matches!(
            apply_to_document(project.path(), "target", "configured-missing"),
            Err(CoWriterError::Configuration(_))
        ));
        fs::write(project.path().join(".mine.toml"), "invalid=true").unwrap();
        assert_eq!(list(project.path()).unwrap(), vec![saved.clone()]);
        apply_to_document(project.path(), "target", &saved.id).unwrap();
        assert!(delete(project.path(), &saved.id).unwrap().is_empty());
    }

    #[test]
    fn current_main_delete_preserves_remaining_context_without_freezing_missing_settings() {
        let project = tempfile::tempdir().unwrap();
        let snapshot =
            set_document_context_snapshot(project.path(), "source", "Original.\r\n", &[]).unwrap();
        let (path, bytes) = write_context_library(project.path(), &["Delete", "Keep"], &snapshot);
        let mut disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // v1 explicitly permits this field to be absent.
        disk["profiles"][profile_id("Keep")]
            .as_object_mut()
            .unwrap()
            .remove("text_sources");
        fs::write(&path, serde_json::to_vec(&disk).unwrap()).unwrap();
        fs::write(project.path().join(".mine.toml"), "invalid=true").unwrap();
        let remaining = delete(project.path(), &profile_id("Delete")).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].name, "Keep");
        assert_eq!(remaining[0].updated_at_unix_ms, 20);
        let disk: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(disk["schema"], PROFILE_SCHEMA);
        assert_eq!(disk["profiles"].as_object().unwrap().len(), 1);
        let kept = &disk["profiles"][profile_id("Keep")];
        assert_eq!(kept["markdown"], "Original.\r\n");
        assert!(kept.get("frozen").is_none());
        assert!(matches!(
            read_profiles(project.path())
                .unwrap()
                .profiles
                .remove(&profile_id("Keep"))
                .unwrap(),
            StoredCoWriterProfile::Context(_)
        ));
    }

    #[test]
    fn invalid_current_main_receipt_leaves_target_and_library_unchanged() {
        let project = tempfile::tempdir().unwrap();
        let snapshot =
            set_document_context_snapshot(project.path(), "source", "Original.", &[]).unwrap();
        let (path, bytes) = write_context_library(project.path(), &["Voice"], &snapshot);
        let mut disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        disk["profiles"][profile_id("Voice")]["attachment_ids"] =
            serde_json::json!(["a".repeat(64)]);
        let bytes = serde_json::to_vec(&disk).unwrap();
        fs::write(&path, &bytes).unwrap();
        set_document_context_snapshot(project.path(), "target", "Keep this.", &[]).unwrap();
        assert!(apply_to_document(project.path(), "target", &profile_id("Voice")).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            document_context_snapshot(project.path(), "target")
                .unwrap()
                .markdown,
            "Keep this."
        );
        assert!(
            applied_co_writer(project.path(), "target")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn applied_document_and_sampling_survive_library_updates_and_deletion() {
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join(".mine.toml"),
            "[generation]\nmanual_writing='voice'\n[profiles.voice.sampling]\ntemperature=0.25",
        )
        .unwrap();
        set_document_context_snapshot(project.path(), "source", "  Original.\r\n", &[]).unwrap();
        let first = save_from_document(project.path(), "source", "Voice", 10).unwrap();
        apply_to_document(project.path(), "target", &first.id).unwrap();
        let original = applied_co_writer(project.path(), "target")
            .unwrap()
            .unwrap();
        let revision = original
            .context
            .lineage()
            .revision
            .as_ref()
            .unwrap()
            .revision_id
            .clone();
        set_document_context_snapshot(project.path(), "source", "Edited library body", &[])
            .unwrap();
        save_from_document(project.path(), "source", "Voice", 20).unwrap();
        let StoredCoWriterProfile::Frozen(newer) = read_profiles(project.path())
            .unwrap()
            .profiles
            .remove(&first.id)
            .unwrap()
        else {
            panic!("explicit save must freeze the source");
        };
        assert_eq!(
            newer
                .frozen
                .context
                .lineage()
                .revision
                .as_ref()
                .unwrap()
                .parent_revision_id
                .as_deref(),
            Some(revision.as_str())
        );
        delete(project.path(), &first.id).unwrap();
        fs::write(project.path().join(".mine.toml"), "invalid=true").unwrap();
        let (profile, applied) = crate::generation_profiles::freeze_for_document(
            project.path(),
            "target",
            GenerationTask::AutomaticProse,
        )
        .unwrap();
        assert_eq!(applied.unwrap().markdown().unwrap(), "  Original.\r\n");
        assert_eq!(
            profile
                .resolve(SamplingOverrides::default())
                .unwrap()
                .temperature
                .to_bits(),
            0.25_f32.to_bits()
        );
        assert_eq!(
            document_context_snapshot(project.path(), "target")
                .unwrap()
                .markdown,
            "  Original.\r\n"
        );
    }

    #[test]
    fn configured_persona_applies_exactly_once_and_keeps_its_source_after_file_edits() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir_all(project.path().join(".mine/personas")).unwrap();
        fs::write(
            project.path().join(".mine/personas/voice.md"),
            "  Persona α.\r\n",
        )
        .unwrap();
        fs::write(project.path().join(".mine.toml"), "[profiles.voice]\ncontext_file='.mine/personas/voice.md'\n[profiles.voice.sampling]\ntop_k=7").unwrap();
        let choices = list(project.path()).unwrap();
        assert!(choices[0].configured);
        apply_to_document(project.path(), "target", &choices[0].id).unwrap();
        let (profile, applied) = crate::generation_profiles::freeze_for_document(
            project.path(),
            "target",
            GenerationTask::Chat,
        )
        .unwrap();
        assert!(profile.context_in_document);
        assert!(crate::generation_profiles::preamble(&profile).is_empty());
        assert_eq!(
            profile.resolve(SamplingOverrides::default()).unwrap().top_k,
            7
        );
        let applied = applied.unwrap();
        assert_eq!(applied.markdown().unwrap(), "  Persona α.\r\n");
        assert_eq!(applied.generation.context_text(), "  Persona α.\r\n");
        assert_eq!(
            applied
                .context
                .lineage()
                .source
                .as_ref()
                .unwrap()
                .document_id,
            ".mine.toml#profiles.voice"
        );
        fs::remove_file(project.path().join(".mine/personas/voice.md")).unwrap();
        assert_eq!(
            document_context_snapshot(project.path(), "target")
                .unwrap()
                .markdown,
            "  Persona α.\r\n"
        );
        assert!(matches!(
            delete(project.path(), &choices[0].id),
            Err(CoWriterError::Configured)
        ));
    }

    #[test]
    fn invalid_attachment_receipt_rejects_the_entire_atomic_apply() {
        let project = tempfile::tempdir().unwrap();
        set_document_context_snapshot(project.path(), "target", "Keep this.", &[]).unwrap();
        let profile = MineConfig::default()
            .freeze(project.path(), GenerationTask::ManualWriting)
            .unwrap();
        let frozen = freeze_context(
            "cowriter-test",
            "source",
            "Replace this.".into(),
            vec!["a".repeat(64)],
            vec![],
            profile,
            None,
        )
        .unwrap();
        assert!(set_document_context_with_co_writer(project.path(), "target", &frozen).is_err());
        assert_eq!(
            document_context_snapshot(project.path(), "target")
                .unwrap()
                .markdown,
            "Keep this."
        );
        assert!(
            applied_co_writer(project.path(), "target")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn incompatible_library_is_rejected_without_rewriting_source() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join(".loom")).unwrap();
        let source = b"{\"schema\":\"loom.co-writer-profiles.v0\",\"profiles\":{}}";
        let path = project.path().join(".loom/co-writers.json");
        fs::write(&path, source).unwrap();
        assert!(matches!(list(project.path()), Err(CoWriterError::Invalid)));
        assert_eq!(fs::read(path).unwrap(), source);
    }

    #[test]
    fn named_profile_round_trips_and_replaces_document_context() {
        let project = tempfile::tempdir().expect("project fixture");
        set_document_context_snapshot(project.path(), "first", "Quiet, exact prose.", &[])
            .expect("seed context");
        let saved =
            save_from_document(project.path(), "first", "Night editor", 41).expect("save profile");
        assert_eq!(saved.name, "Night editor");
        assert_eq!(
            list(project.path()).expect("list profiles"),
            vec![saved.clone()]
        );

        set_document_context_snapshot(project.path(), "second", "Replace me.", &[])
            .expect("seed target");
        let applied =
            apply_to_document(project.path(), "second", &saved.id).expect("apply profile");
        assert_eq!(applied.markdown, "Quiet, exact prose.");
        assert!(applied.attachments.is_empty());

        assert!(
            delete(project.path(), &saved.id)
                .expect("delete profile")
                .is_empty()
        );
        assert!(matches!(
            apply_to_document(project.path(), "second", &saved.id),
            Err(CoWriterError::NotFound)
        ));
    }

    #[test]
    fn same_normalized_name_updates_without_allocating_a_second_profile() {
        let project = tempfile::tempdir().expect("project fixture");
        set_document_context_snapshot(project.path(), "doc", "First.", &[])
            .expect("seed first context");
        let first =
            save_from_document(project.path(), "doc", "  Voice  ", 10).expect("save first profile");
        set_document_context_snapshot(project.path(), "doc", "Second.", &[])
            .expect("seed second context");
        let second =
            save_from_document(project.path(), "doc", "voice", 20).expect("update profile");
        assert_eq!(first.id, second.id);
        assert_eq!(second.created_at_unix_ms, 10);
        assert_eq!(second.updated_at_unix_ms, 20);
        assert_eq!(list(project.path()).expect("list profiles").len(), 1);
    }
}
