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
    fn lock(&self) -> Result<File, Error> {
        std::fs::create_dir_all(&self.root)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.root.join("preferences.lock"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => Error::Busy,
            std::fs::TryLockError::Error(error) => Error::Io(error),
        })?;
        Ok(file)
    }
    fn load(&self) -> Result<State, Error> {
        let file = match File::open(self.root.join("preferences.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(e.into()),
        };
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
