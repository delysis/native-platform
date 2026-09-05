use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

#[cfg(test)]
use crate::context_attachments::set_document_context_snapshot;
use crate::context_attachments::{
    ContextAttachmentError, ContextTextSourcePresentation, DocumentContextSnapshot,
    document_context_snapshot, set_document_context_snapshot_with_sources,
};

const PROFILE_SCHEMA: &str = "loom.co-writer-profiles.v1";
const MAX_PROFILES: usize = 64;
const MAX_NAME_BYTES: usize = 96;
const MAX_MARKDOWN_BYTES: usize = 256 * 1024;
static PROFILE_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredCoWriterProfile {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
}

#[derive(Debug, Error)]
pub(crate) enum CoWriterError {
    #[error("a co-writer name must be 1 to 96 bytes and contain no control characters")]
    InvalidName,
    #[error("the selected completion context is empty")]
    Empty,
    #[error("the project already contains Loom's 64 co-writer limit")]
    Limit,
    #[error("the selected co-writer does not exist")]
    NotFound,
    #[error("the co-writer store is corrupt")]
    Invalid,
    #[error("co-writer storage state is unavailable")]
    State,
    #[error("completion context failed: {0}")]
    Context(#[from] ContextAttachmentError),
    #[error("co-writer storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("co-writer metadata failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub(crate) fn list(project_root: &Path) -> Result<Vec<CoWriterSummary>, CoWriterError> {
    Ok(read_profiles(project_root)?
        .profiles
        .into_values()
        .map(profile_summary)
        .collect())
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
    let id = profile_id(&name);
    let mut store = read_profiles(project_root)?;
    if !store.profiles.contains_key(&id) && store.profiles.len() >= MAX_PROFILES {
        return Err(CoWriterError::Limit);
    }
    let created_at_unix_ms = store
        .profiles
        .get(&id)
        .map_or(now_unix_ms, |profile| profile.created_at_unix_ms);
    let text_sources = snapshot.text_sources;
    let attachment_ids = snapshot
        .attachments
        .into_iter()
        .map(|attachment| attachment.id)
        .collect::<Vec<_>>();
    let profile = StoredCoWriterProfile {
        id: id.clone(),
        name,
        source_document_id: document_id.to_owned(),
        markdown: snapshot.markdown,
        attachment_ids,
        text_sources,
        created_at_unix_ms,
        updated_at_unix_ms: now_unix_ms,
    };
    let summary = profile_summary(profile.clone());
    store.profiles.insert(id, profile);
    write_profiles(project_root, &store)?;
    Ok(summary)
}

pub(crate) fn apply_to_document(
    project_root: &Path,
    document_id: &str,
    profile_id: &str,
) -> Result<DocumentContextSnapshot, CoWriterError> {
    let profile = read_profiles(project_root)?
        .profiles
        .get(profile_id)
        .cloned()
        .ok_or(CoWriterError::NotFound)?;
    set_document_context_snapshot_with_sources(
        project_root,
        document_id,
        &profile.markdown,
        &profile.attachment_ids,
        Some(&profile.text_sources),
    )
    .map_err(Into::into)
}

pub(crate) fn delete(
    project_root: &Path,
    profile_id: &str,
) -> Result<Vec<CoWriterSummary>, CoWriterError> {
    let _guard = PROFILE_WRITE_LOCK
        .lock()
        .map_err(|_| CoWriterError::State)?;
    let mut store = read_profiles(project_root)?;
    if store.profiles.remove(profile_id).is_none() {
        return Err(CoWriterError::NotFound);
    }
    write_profiles(project_root, &store)?;
    Ok(store.profiles.into_values().map(profile_summary).collect())
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

fn profile_summary(profile: StoredCoWriterProfile) -> CoWriterSummary {
    CoWriterSummary {
        id: profile.id,
        name: profile.name,
        source_document_id: profile.source_document_id,
        context_bytes: u64::try_from(profile.markdown.len()).unwrap_or(u64::MAX),
        attachment_count: u32::try_from(profile.attachment_ids.len()).unwrap_or(u32::MAX),
        created_at_unix_ms: profile.created_at_unix_ms,
        updated_at_unix_ms: profile.updated_at_unix_ms,
    }
}

fn read_profiles(project_root: &Path) -> Result<CoWriterProfiles, CoWriterError> {
    let path = profile_path(project_root)?;
    match fs::read(path) {
        Ok(bytes) => {
            let store: CoWriterProfiles = serde_json::from_slice(&bytes)?;
            if store.schema != PROFILE_SCHEMA
                || store.profiles.len() > MAX_PROFILES
                || store.profiles.iter().any(|(id, profile)| {
                    id != &profile.id
                        || validate_name(&profile.name).is_err()
                        || profile_id(&profile.name) != profile.id
                        || profile.markdown.len() > MAX_MARKDOWN_BYTES
                        || profile.attachment_ids.len() > 32
                        || profile
                            .attachment_ids
                            .iter()
                            .any(|attachment_id| !is_sha256(attachment_id))
                })
            {
                return Err(CoWriterError::Invalid);
            }
            Ok(store)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CoWriterProfiles {
            schema: PROFILE_SCHEMA.to_owned(),
            profiles: BTreeMap::new(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_profiles(project_root: &Path, store: &CoWriterProfiles) -> Result<(), CoWriterError> {
    let path = profile_path(project_root)?;
    let mut file = AtomicWriteFile::options().open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(store)?)?;
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
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(CoWriterError::Invalid);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
