#![forbid(unsafe_code)]
//! Renderer-independent, bounded Loom application preferences. These values are
//! conveniences, never project, model-verification or generation authority.
use atomic_write_file::AtomicWriteFile;
use loom_types::{ProjectId, SuggestionActivation};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 128 * 1024;
const MAX_PROJECTS: usize = 1024;
const MAX_MODEL_PATH: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    /// Decimal string on the wire avoids JavaScript's integer precision limit.
    #[serde(with = "revision_wire")]
    pub revision: u64,
    pub last_local_model: Option<String>,
    pub project_suggestions: BTreeMap<ProjectId, bool>,
}
impl Preferences {
    /// A stored preference cannot enable suggestions without verified policy.
    pub fn suggestions_enabled(
        &self,
        project: ProjectId,
        verified: Option<SuggestionActivation>,
    ) -> bool {
        verified.is_some_and(|activation| {
            self.project_suggestions
                .get(&project)
                .copied()
                .unwrap_or(activation == SuggestionActivation::QuietDefault)
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreferenceChange {
    RememberModel {
        path: String,
    },
    /// Compare-and-clear cannot erase a newer remembered model.
    ForgetModel {
        expected_path: String,
    },
    Suggestions {
        project_id: ProjectId,
        enabled: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelChoice {
    None,
    Path(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    revision: u64,
    model: Option<ModelChoice>,
    project_suggestions: BTreeMap<ProjectId, Option<bool>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            model: None,
            project_suggestions: BTreeMap::new(),
        }
    }
}
impl State {
    fn validate(&self) -> Result<(), Error> {
        if self.version != 1 {
            return Err(Error::Version(self.version));
        }
        if self.project_suggestions.len() > MAX_PROJECTS {
            return Err(Error::Limit);
        }
        if let Some(ModelChoice::Path(path)) = &self.model {
            validate_model_path(path)?;
        }
        Ok(())
    }
    fn snapshot(&self) -> Preferences {
        Preferences {
            revision: self.revision,
            last_local_model: match &self.model {
                Some(ModelChoice::Path(path)) => Some(path.clone()),
                _ => None,
            },
            project_suggestions: self
                .project_suggestions
                .iter()
                .filter_map(|(project, value)| value.map(|enabled| (*project, enabled)))
                .collect(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Another view is updating preferences; retry shortly")]
    Busy,
    #[error("Preference storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Preference data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Preference file version {0} is unsupported")]
    Version(u32),
    #[error("Preference storage must be a private regular file")]
    FileKind,
    #[error("Preferences exceeded their bounded capacity")]
    Limit,
    #[error("A remembered model must have an absolute, valid path")]
    ModelPath,
    #[error("Temporary acceptance models cannot become startup preferences")]
    EphemeralModel,
}

/// All instances read current disk state under the same OS file lock. Atomic
/// replacement protects crash recovery; the stable sidecar lock protects against
/// lost updates between views/processes. Call from a blocking worker, not paint.
#[derive(Clone, Debug)]
pub struct PreferenceStore {
    root: PathBuf,
}

/// One successfully acquired transaction lock; the file never escapes production code.
#[derive(Debug)]
struct PreferenceLock {
    file: File,
}

impl Drop for PreferenceLock {
    fn drop(&mut self) {
        // A passive duplicated/inherited descriptor can outlive this transaction.
        // Drop cannot report an unlock error and must not panic or replace the
        // operation's result (which may describe an already-published update).
        // File close is the fallback, not a guarantee of prompt release if
        // unlocking fails and another descriptor still holds the description.
        let _ = self.file.unlock();
    }
}

impl PreferenceStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn read(&self) -> Result<Preferences, Error> {
        let _lock = self.lock()?;
        Ok(self.load()?.snapshot())
    }
    pub fn update(&self, change: PreferenceChange) -> Result<Preferences, Error> {
        self.change(|state| {
            match change {
                PreferenceChange::RememberModel { path } => {
                    validate_model_path(&path)?;
                    state.model = Some(ModelChoice::Path(path));
                }
                PreferenceChange::ForgetModel { expected_path } => {
                    if state.model == Some(ModelChoice::Path(expected_path)) {
                        state.model = Some(ModelChoice::None);
                    }
                }
                PreferenceChange::Suggestions {
                    project_id,
                    enabled,
                } => {
                    state.project_suggestions.insert(project_id, enabled);
                }
            }
            Ok(())
        })
    }
    fn lock(&self) -> Result<PreferenceLock, Error> {
        std::fs::create_dir_all(&self.root)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(self.root.join("preferences.lock"))?;
        validate_regular_file(&file)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => Error::Busy,
            std::fs::TryLockError::Error(error) => Error::Io(error),
        })?;
        Ok(PreferenceLock { file })
    }
    fn load(&self) -> Result<State, Error> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(self.root.join("preferences.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(e.into()),
        };
        validate_regular_file(&file)?;
        if file.metadata()?.len() > MAX_BYTES {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::Limit);
        }
        let state: State = serde_json::from_slice(&bytes)?;
        state.validate()?;
        Ok(state)
    }
    fn change(
        &self,
        mutate: impl FnOnce(&mut State) -> Result<(), Error>,
    ) -> Result<Preferences, Error> {
        let _lock = self.lock()?;
        let old = self.load()?;
        let mut state = old.clone();
        mutate(&mut state)?;
        state.validate()?;
        if state != old {
            state.revision = old.revision.checked_add(1).ok_or(Error::Limit)?;
            let bytes = serde_json::to_vec_pretty(&state)?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err(Error::Limit);
            }
            let mut file = AtomicWriteFile::open(self.root.join("preferences.json"))?;
            file.write_all(&bytes)?;
            file.commit()?;
        }
        Ok(state.snapshot())
    }
}

fn validate_regular_file(file: &File) -> Result<(), Error> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::FileKind);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(Error::FileKind);
        }
    }
    Ok(())
}

fn validate_model_path(path: &str) -> Result<(), Error> {
    if path.is_empty()
        || path.len() > MAX_MODEL_PATH
        || path.contains('\0')
        || !Path::new(path).is_absolute()
    {
        return Err(Error::ModelPath);
    }
    let normalized = path.replace('\\', "/");
    let components: Vec<_> = normalized.split('/').collect();
    if components.windows(4).any(|p| {
        p[0].strip_prefix("delysis-loom-smoke.")
            .is_some_and(|suffix| !suffix.is_empty())
            && p[1] == "product"
            && p[2] == "models"
            && !p[3].is_empty()
    }) {
        return Err(Error::EphemeralModel);
    }
    Ok(())
}

mod revision_wire {
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "Serde with-module serialization signature"
    )]
    pub fn serialize<S: Serializer>(revision: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&revision.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(all(test, unix))]
mod lock_lifetime {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn ended_store_lock_releases_passive_duplicate_without_masking_contention() {
        let root = tempfile::tempdir().expect("create owned preferences root");
        let store = PreferenceStore::new(root.path());
        let lock_path = root.path().join("preferences.lock");
        let owner = store.lock().expect("acquire actual store lock");
        let file = &owner.file;
        let passive = file.try_clone().expect("duplicate held description");
        assert_ne!(file.as_raw_fd(), passive.as_raw_fd());
        let held = file.metadata().expect("inspect held description");
        let duplicate = passive.metadata().expect("inspect passive description");
        let named = std::fs::symlink_metadata(&lock_path).expect("inspect lock entry");
        let identity = (held.dev(), held.ino());
        assert_eq!(identity, (duplicate.dev(), duplicate.ino()));
        assert_eq!(identity, (named.dev(), named.ino()));
        assert!(named.is_file());
        assert_eq!(named.nlink(), 1);
        eprintln!(
            "acquired actual store lock: dev={} ino={}",
            identity.0, identity.1
        );

        let change = || PreferenceChange::RememberModel {
            path: "/models/lock-lifetime-witness.gguf".into(),
        };
        assert!(matches!(store.update(change()), Err(Error::Busy)));
        assert!(matches!(store.read(), Err(Error::Busy)));
        // Failed acquisitions must neither bypass nor end the active ownership.
        assert!(matches!(store.update(change()), Err(Error::Busy)));

        drop(owner);
        let after_owner_ended = store.update(change());
        let read_with_passive = store.read();
        let named_after = std::fs::symlink_metadata(&lock_path);
        eprintln!("after owner end with passive descriptor alive: {after_owner_ended:?}");
        // Close the surrogate before the deliberate before-repair assertion fails.
        drop(passive);
        let recovered = store.read().expect("read after final passive close");
        assert!(
            after_owner_ended.is_ok(),
            "ended owner must release the store despite a passive duplicate: {after_owner_ended:?}"
        );
        let updated = after_owner_ended.expect("checked successful update");
        assert_eq!(updated.revision, 1);
        assert_eq!(
            updated.last_local_model.as_deref(),
            Some("/models/lock-lifetime-witness.gguf")
        );
        assert_eq!(read_with_passive.expect("read with passive alive"), updated);
        assert_eq!(recovered, updated);
        let named_after = named_after.expect("stable lock entry survives transaction");
        assert!(named_after.is_file());
        assert_eq!(named_after.nlink(), 1);
        assert_eq!(identity, (named_after.dev(), named_after.ino()));
    }

    #[test]
    fn change_error_releases_lock_without_publishing_mutated_state() {
        let root = tempfile::tempdir().expect("create owned preferences root");
        let store = PreferenceStore::new(root.path());
        let original = store
            .update(PreferenceChange::RememberModel {
                path: "/models/original.gguf".into(),
            })
            .expect("seed actual store");
        let path = root.path().join("preferences.json");
        let original_bytes = std::fs::read(&path).expect("retain original bytes");

        let result = store.change(|state| {
            assert!(matches!(store.read(), Err(Error::Busy)));
            state.model = Some(ModelChoice::Path("/models/uncommitted.gguf".into()));
            assert!(matches!(store.read(), Err(Error::Busy)));
            Err(Error::Limit)
        });
        assert!(matches!(result, Err(Error::Limit)));
        assert_eq!(store.read().expect("read after mutation error"), original);
        assert_eq!(
            std::fs::read(&path).expect("read retained bytes"),
            original_bytes
        );
        let next = store
            .update(PreferenceChange::RememberModel {
                path: "/models/next.gguf".into(),
            })
            .expect("public update after mutation error");
        assert_eq!(next.revision, original.revision + 1);
    }

    #[test]
    fn change_unwind_releases_lock_without_publishing_mutated_state() {
        let root = tempfile::tempdir().expect("create owned preferences root");
        let store = PreferenceStore::new(root.path());
        let original = store
            .update(PreferenceChange::RememberModel {
                path: "/models/original.gguf".into(),
            })
            .expect("seed actual store");
        let path = root.path().join("preferences.json");
        let original_bytes = std::fs::read(&path).expect("retain original bytes");

        let panic = std::panic::catch_unwind(|| {
            store.change(|state| {
                assert!(matches!(store.read(), Err(Error::Busy)));
                state.model = Some(ModelChoice::Path("/models/uncommitted.gguf".into()));
                assert!(matches!(store.read(), Err(Error::Busy)));
                panic!("preference transaction unwind witness");
            })
        })
        .expect_err("the deliberate transaction panic must reach the catcher");
        // An assertion panic before the intended point must not count as success.
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"preference transaction unwind witness")
        );
        assert_eq!(
            store.read().expect("read after transaction unwind"),
            original
        );
        assert_eq!(
            std::fs::read(&path).expect("read retained bytes"),
            original_bytes
        );
        let next = store
            .update(PreferenceChange::RememberModel {
                path: "/models/next.gguf".into(),
            })
            .expect("public update after transaction unwind");
        assert_eq!(next.revision, original.revision + 1);
    }

    #[test]
    fn publication_error_releases_lock_and_preserves_the_obstruction() {
        let root = tempfile::tempdir().expect("create owned preferences root");
        let store = PreferenceStore::new(root.path());
        let path = root.path().join("preferences.json");
        let marker = path.join("owned-obstruction");
        // The actual change path has loaded fresh state before this closure runs.
        // A nonempty directory cannot be replaced by its atomic file publication.
        let result = store.change(|state| {
            assert!(matches!(store.read(), Err(Error::Busy)));
            std::fs::create_dir(&path)?;
            std::fs::write(&marker, b"retain this owned obstruction")?;
            state.model = Some(ModelChoice::Path("/models/uncommitted.gguf".into()));
            Ok(())
        });
        assert!(matches!(result, Err(Error::Io(_))), "{result:?}");
        drop(store.lock().expect("acquire after publication-path error"));
        // This is a fresh public acquisition reaching validation, not Busy.
        assert!(matches!(store.read(), Err(Error::FileKind)));
        assert_eq!(
            std::fs::read(&marker).expect("read retained obstruction"),
            b"retain this owned obstruction"
        );
        // Remove only the exact synthetic entries this test created.
        std::fs::remove_file(&marker).expect("remove owned marker");
        std::fs::remove_dir(&path).expect("remove owned empty obstruction directory");
        assert_eq!(
            store.read().expect("read after owned cleanup"),
            Preferences::default()
        );
        let next = store
            .update(PreferenceChange::RememberModel {
                path: "/models/next.gguf".into(),
            })
            .expect("public update after publication-path error");
        assert_eq!(next.revision, 1);
    }
}
