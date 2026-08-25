use crate::config::{SETTINGS_NAMESPACE, Settings, resolve_settings};
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, ConversationDb, ConversationKind, load_db,
};
use crate::engine::{ValidationBlocker, validate_model_path};
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_DISCOVERED_MODELS: usize = 512;
const MAX_CACHE_SCAN_DEPTH: usize = 8;
const MAX_PROJECTOR_SIBLINGS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelectionIntent {
    scope: PathBuf,
    generation: u64,
}

#[derive(Debug, Default)]
struct DefaultModelSelectionOrder {
    next_generation: u64,
    latest_by_scope: BTreeMap<PathBuf, u64>,
}

impl DefaultModelSelectionOrder {
    fn begin(&mut self, scope: PathBuf) -> ModelSelectionIntent {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.latest_by_scope.insert(scope.clone(), generation);
        ModelSelectionIntent { scope, generation }
    }

    fn is_current(&self, token: &ModelSelectionIntent) -> bool {
        self.latest_by_scope.get(&token.scope) == Some(&token.generation)
    }
}

fn default_model_selection_order() -> &'static Mutex<DefaultModelSelectionOrder> {
    static ORDER: OnceLock<Mutex<DefaultModelSelectionOrder>> = OnceLock::new();
    ORDER.get_or_init(|| Mutex::new(DefaultModelSelectionOrder::default()))
}

pub fn begin_model_selection() -> Result<ModelSelectionIntent> {
    let scope = crate::config::resolve_data_dir();
    let mut order = default_model_selection_order()
        .lock()
        .map_err(|_| anyhow!("default model selection order lock was poisoned"))?;
    Ok(order.begin(scope))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub path: String,
    pub selected: bool,
    #[serde(default)]
    pub loaded: bool,
    pub size_bytes: Option<u64>,
}

pub fn model_list() -> Result<CommandResult<Vec<ModelInfo>>> {
    let settings = resolve_settings()?;
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(path) = settings.model_path.as_ref() {
        // A picker grants authority for the selected file, not for an
        // unbounded walk of its parent directory. Discover other models only
        // from the explicit cache root below.
        push_model(&mut models, &mut seen, path.clone(), true);
    }
    let resident_paths = crate::native_runtime::resident_slots()
        .into_iter()
        .map(|slot| slot.model_path)
        .collect::<Vec<_>>();
    for path in &resident_paths {
        let selected = settings.model_path.as_ref() == Some(path);
        push_model(&mut models, &mut seen, path.clone(), selected);
    }
    if let Some(cache_dir) = hugging_face_hub_cache_dir() {
        let mut cached = Vec::new();
        collect_cached_models(&cache_dir, 0, &mut cached);
        cached.sort_by(|left, right| {
            model_file_name(left)
                .cmp(&model_file_name(right))
                .then_with(|| left.cmp(right))
        });
        for path in cached {
            let selected = settings.model_path.as_ref() == Some(&path);
            push_model(&mut models, &mut seen, path, selected);
        }
    }
    let loaded = resident_paths
        .into_iter()
        .map(|path| fs::canonicalize(&path).unwrap_or(path))
        .collect::<BTreeSet<_>>();
    for model in &mut models {
        let path = PathBuf::from(&model.path);
        model.loaded = loaded.contains(&fs::canonicalize(&path).unwrap_or(path));
    }
    Ok(CommandResult::passed(
        "mom_llama.model_list",
        "contracted",
        models,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn hugging_face_hub_cache_dir() -> Option<PathBuf> {
    if let Some(path) =
        nonempty_env_path("HF_HUB_CACHE").or_else(|| nonempty_env_path("HUGGINGFACE_HUB_CACHE"))
    {
        return Some(path);
    }
    if let Some(path) = nonempty_env_path("HF_HOME") {
        return Some(path.join("hub"));
    }
    if let Some(path) = nonempty_env_path("XDG_CACHE_HOME") {
        return Some(path.join("huggingface").join("hub"));
    }
    user_home_dir().map(|home| home.join(".cache").join("huggingface").join("hub"))
}

fn nonempty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn user_home_dir() -> Option<PathBuf> {
    nonempty_env_path("HOME")
        .or_else(|| nonempty_env_path("USERPROFILE"))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty())?;
            let path = std::env::var_os("HOMEPATH").filter(|value| !value.is_empty())?;
            let mut home = drive;
            home.push(path);
            Some(PathBuf::from(home))
        })
}

fn collect_cached_models(directory: &Path, depth: usize, models: &mut Vec<PathBuf>) {
    if depth > MAX_CACHE_SCAN_DEPTH || models.len() >= MAX_DISCOVERED_MODELS {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if models.len() >= MAX_DISCOVERED_MODELS {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_cached_models(&path, depth + 1, models);
        } else if (file_type.is_file()
            || (file_type.is_symlink()
                && fs::metadata(&path).is_ok_and(|metadata| metadata.is_file())))
            && is_model_gguf(&path)
        {
            models.push(path);
        }
    }
}

fn push_model(
    models: &mut Vec<ModelInfo>,
    seen: &mut BTreeSet<PathBuf>,
    path: PathBuf,
    selected: bool,
) {
    let identity = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if !seen.insert(identity) {
        if selected
            && let Some(existing) = models
                .iter_mut()
                .find(|model| model.path == path.display().to_string())
        {
            existing.selected = true;
        }
        return;
    }
    models.push(ModelInfo {
        id: model_file_name(&path),
        path: path.display().to_string(),
        selected,
        loaded: false,
        size_bytes: fs::metadata(&path).ok().map(|metadata| metadata.len()),
    });
}

fn model_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("model.gguf")
        .to_string()
}

fn is_model_gguf(path: &Path) -> bool {
    if !is_gguf(path) {
        return false;
    }
    let name = model_file_name(path).to_ascii_lowercase();
    !name.starts_with("mmproj-") && !name.contains("-mtp.")
}

pub fn model_select(model_path: PathBuf) -> Result<CommandResult<crate::Settings>> {
    // Claim the intent before validation or loading. A later invocation must
    // supersede this one even when it chooses an invalid path.
    let selection = begin_model_selection()?;
    model_select_with_intent(model_path, selection)
}

pub fn model_select_with_intent(
    model_path: PathBuf,
    selection: ModelSelectionIntent,
) -> Result<CommandResult<crate::Settings>> {
    let prepared = match settings_for_model_selection(model_path) {
        Ok(settings) => settings,
        Err(blocked) => return Ok(blocked_model_selection(blocked)),
    };
    if let Err(blocked) = crate::native_runtime::resident_model_for_profile(
        &prepared,
        prepared
            .model_path
            .as_deref()
            .expect("model selection always installs a model path"),
        prepared.mmproj_path.as_deref(),
    ) {
        return Ok(blocked_model_selection(blocked));
    }
    let (settings, path) = match persist_default_model_selection(&prepared, &selection)? {
        Ok(persisted) => persisted,
        Err(blocked) => return Ok(blocked_model_selection(blocked)),
    };
    Ok(CommandResult::passed(
        "mom_llama.model_select",
        "host_integrated",
        settings,
        vec![path.display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

/// Changes the frozen model identity of one existing conversation. This is the
/// composer boundary: selecting a model in an old chat must not silently mutate
/// only the default for future chats.
pub fn conversation_model_select_and_load(
    conversation_id: &str,
    model_path: PathBuf,
) -> Result<CommandResult<crate::Settings>> {
    let snapshot = load_db()?;
    let Some(conversation) = snapshot
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.model_select",
            "stub_blocked",
            Blocker::new(
                "conversation_not_found",
                "That conversation no longer exists.",
                vec!["Refresh the conversation list and try again.".to_string()],
            ),
        ));
    };
    if conversation.kind != ConversationKind::Chat {
        return Ok(CommandResult::blocked(
            "mom_llama.model_select",
            "stub_blocked",
            Blocker::new(
                "conversation_model_selection_not_chat",
                "Choose a model after starting a chat from this Persona.",
                vec!["Start a chat, then choose its model in the composer.".to_string()],
            ),
        ));
    }
    let expected_profile_version = conversation.execution_profile.version;
    let settings = match settings_for_model_selection(model_path) {
        Ok(settings) => settings,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.model_select",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    if let Err(blocked) = crate::native_runtime::resident_model_for_profile(
        &settings,
        settings
            .model_path
            .as_deref()
            .expect("model selection always installs a model path"),
        settings.mmproj_path.as_deref(),
    ) {
        return Ok(CommandResult::blocked(
            "mom_llama.model_select",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let store = RuntimeStore::current()?;
    let selected_model_path = settings.model_path.clone();
    let selected_mmproj_path = settings.mmproj_path.clone();
    let changed = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |conversations, documents| {
            crate::personas::reject_removed_conversation_id_from_documents(
                conversation_id,
                documents,
            )?;
            crate::personas::reject_removed_conversation_writes_from_documents(
                conversations,
                documents,
            )?;
            let Some(conversation) = conversations
                .conversations
                .iter_mut()
                .find(|conversation| conversation.id == conversation_id)
            else {
                return Ok(Err(Blocker::new(
                    "conversation_not_found",
                    "That conversation no longer exists.",
                    vec!["Refresh the conversation list and try again.".to_string()],
                )));
            };
            if conversation.kind != ConversationKind::Chat
                || conversation.execution_profile.version != expected_profile_version
            {
                return Ok(Err(Blocker::new(
                    "conversation_profile_changed",
                    "This chat's model profile changed while the new model was loading.",
                    vec!["Refresh the chat and choose the model again.".to_string()],
                )));
            }
            conversation.selected_model_path = selected_model_path.clone();
            conversation.execution_profile.model_path = selected_model_path.clone();
            conversation.execution_profile.mmproj_path = selected_mmproj_path.clone();
            conversation.execution_profile.version =
                conversation.execution_profile.version.saturating_add(1);
            conversation.updated_at = crate::now_ms().to_string();
            Ok(Ok(()))
        },
    )?;
    if let Err(blocker) = changed {
        return Ok(CommandResult::blocked(
            "mom_llama.model_select",
            "stub_blocked",
            blocker,
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.model_select",
        "host_integrated",
        settings,
        vec![store.path().display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

fn blocked_model_selection(blocked: ValidationBlocker) -> CommandResult<crate::Settings> {
    CommandResult::blocked(
        "mom_llama.model_select",
        &blocked.readiness,
        blocked.blocker,
    )
}

fn settings_for_model_selection(
    model_path: PathBuf,
) -> std::result::Result<crate::Settings, ValidationBlocker> {
    validate_model_path(&model_path)?;
    reject_conflicting_runtime_override(
        &model_path,
        crate::config::runtime_model_override().as_deref(),
    )?;
    let mmproj_path = discover_projector_for_model(&model_path)?;
    let mut settings = resolve_settings().map_err(|error| {
        error
            .downcast_ref::<ValidationBlocker>()
            .cloned()
            .unwrap_or_else(|| ValidationBlocker {
                readiness: "blocked_settings".to_string(),
                blocker: Blocker::new(
                    "settings_store_unavailable",
                    "The local model selection could not be saved.",
                    vec![error.to_string()],
                ),
            })
    })?;
    bind_model_pair(&mut settings, model_path, mmproj_path);
    Ok(settings)
}

fn reject_conflicting_runtime_override(
    selected: &Path,
    runtime_override: Option<&Path>,
) -> std::result::Result<(), ValidationBlocker> {
    if runtime_override.is_none_or(|runtime_override| runtime_override == selected) {
        return Ok(());
    }
    Err(ValidationBlocker {
        readiness: "blocked_runtime_override".to_string(),
        blocker: Blocker::new(
            "model_runtime_override_active",
            "A launch-time model override is active, so this model choice cannot become effective.",
            vec!["Relaunch without MOM_LLAMA_MODEL_PATH to choose models in the app.".to_string()],
        ),
    })
}

fn bind_model_pair(settings: &mut Settings, model_path: PathBuf, mmproj_path: Option<PathBuf>) {
    settings.model_path = Some(model_path);
    // A projector belongs to one exact model snapshot. Never carry a stale
    // projector across a model change.
    settings.mmproj_path = mmproj_path.clone();
    settings.upstream_settings.insert(
        "mmprojPath".to_string(),
        json!(
            mmproj_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        ),
    );
}

fn persist_default_model_selection(
    prepared: &Settings,
    selection: &ModelSelectionIntent,
) -> Result<std::result::Result<(Settings, PathBuf), ValidationBlocker>> {
    let store = RuntimeStore::open(&prepared.data_dir)?;
    let data_dir = prepared.data_dir.clone();
    let model_path = prepared.model_path.clone();
    let mmproj_path = prepared.mmproj_path.clone();
    let expected_device = prepared.native_device;
    let expected_context_tokens = prepared.context_tokens;
    let expected_batch_tokens = prepared.batch_tokens;
    let expected_parallel_sequences = prepared.max_parallel_sequences;
    let expected_memory_budget = prepared.resident_memory_budget_bytes;
    // Hold this short process-local order lock through the encrypted settings
    // mutation. This closes the check/commit race without serializing model
    // loading: slow-old and fast-new loads still run concurrently, but only the
    // newest intent can commit.
    let order = default_model_selection_order()
        .lock()
        .map_err(|_| anyhow!("default model selection order lock was poisoned"))?;
    if !order.is_current(selection) {
        return Ok(Err(ValidationBlocker {
            readiness: "blocked_selection_superseded".to_string(),
            blocker: Blocker::new(
                "model_selection_superseded",
                "A newer model choice replaced this one while it was loading.",
                vec!["Use the model shown in the picker, or choose again.".to_string()],
            ),
        }));
    }
    let selected = store.mutate(
        SETTINGS_NAMESPACE,
        || Settings::defaults_for_data_dir(data_dir.clone()),
        |current: &mut Settings| {
            crate::config::reconcile_resident_memory_budget_for_runtime(current);
            let host_settings_unchanged = current.native_device == expected_device
                && current.context_tokens == expected_context_tokens
                && current.batch_tokens == expected_batch_tokens
                && current.max_parallel_sequences == expected_parallel_sequences
                && current.resident_memory_budget_bytes == expected_memory_budget;
            if !host_settings_unchanged {
                return Ok(Err(ValidationBlocker {
                    readiness: "blocked_settings_changed".to_string(),
                    blocker: Blocker::new(
                        "model_host_settings_changed",
                        "Native model settings changed while this model was loading.",
                        vec!["Refresh Settings and choose the model again.".to_string()],
                    ),
                }));
            }
            current.data_dir = data_dir.clone();
            current.model_path = model_path.clone();
            current.mmproj_path = mmproj_path.clone();
            current.upstream_settings.insert(
                "mmprojPath".to_string(),
                json!(
                    mmproj_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ),
            );
            Ok(Ok(current.clone()))
        },
    )?;
    Ok(selected.map(|settings| (settings, store.path().to_path_buf())))
}

/// Finds a projector only beside the selected path. In particular, this scans
/// the immutable Hugging Face snapshot path before canonicalizing its symlink
/// into the global blobs directory. It never recurses and never touches the
/// network.
pub fn discover_projector_for_model(
    model_path: &Path,
) -> std::result::Result<Option<PathBuf>, ValidationBlocker> {
    if model_path.as_os_str().is_empty() {
        return Ok(None);
    }
    let Some(directory) = model_path.parent() else {
        return Ok(None);
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(None);
    };
    let mut projectors = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_PROJECTOR_SIBLINGS {
            return Err(ValidationBlocker {
                readiness: "blocked_projector_scan_bound".to_string(),
                blocker: Blocker::new(
                    "projector_directory_too_large",
                    "This model folder contains too many files to pair a vision projector safely.",
                    vec![
                        "Move the model and its one matching projector into a smaller local folder."
                            .to_string(),
                    ],
                ),
            });
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_file = file_type.is_file()
            || (file_type.is_symlink()
                && fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()));
        if is_file && is_projector_gguf(&path) {
            projectors.push(path);
        }
    }
    projectors.sort_by(|left, right| {
        model_file_name(left)
            .cmp(&model_file_name(right))
            .then_with(|| left.cmp(right))
    });
    projectors.dedup();
    match projectors.len() {
        0 => Ok(None),
        1 => Ok(projectors.pop()),
        count => Err(ValidationBlocker {
            readiness: "blocked_ambiguous_projector".to_string(),
            blocker: Blocker::new(
                "mmproj_path_ambiguous",
                format!(
                    "This model has {count} possible vision projectors, so Mom cannot safely choose one."
                ),
                vec![
                    "Keep one matching projector beside the model, then choose the model again."
                        .to_string(),
                ],
            ),
        }),
    }
}

fn is_projector_gguf(path: &Path) -> bool {
    if !is_gguf(path) {
        return false;
    }
    let name = model_file_name(path).to_ascii_lowercase();
    name.contains("mmproj")
}

fn is_gguf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::collect_cached_models;
    use super::{
        DefaultModelSelectionOrder, MAX_PROJECTOR_SIBLINGS, bind_model_pair,
        discover_projector_for_model, hugging_face_hub_cache_dir,
        reject_conflicting_runtime_override,
    };
    use std::path::PathBuf;

    #[test]
    fn default_hugging_face_cache_uses_the_shared_desktop_location() {
        if std::env::var_os("HF_HUB_CACHE").is_none()
            && std::env::var_os("HUGGINGFACE_HUB_CACHE").is_none()
            && std::env::var_os("HF_HOME").is_none()
            && std::env::var_os("XDG_CACHE_HOME").is_none()
        {
            let cache = hugging_face_hub_cache_dir()
                .expect("the platform home directory should resolve a cache path");
            assert!(
                cache.ends_with(
                    std::path::Path::new(".cache")
                        .join("huggingface")
                        .join("hub")
                )
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn cache_discovery_does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary =
            std::env::temp_dir().join(format!("mom-llama-model-cache-symlink-{}", crate::now_ms()));
        let cache = temporary.join("hub");
        let outside = temporary.join("outside");
        std::fs::create_dir_all(&cache).expect("cache directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        let linked_model = outside.join("linked.gguf");
        std::fs::write(outside.join("hidden.gguf"), b"GGUF").expect("hidden model");
        std::fs::write(&linked_model, b"GGUF").expect("linked model");
        symlink(&outside, cache.join("linked-directory")).expect("directory symlink");
        symlink(&outside, cache.join("linked-directory.gguf")).expect("GGUF directory symlink");
        symlink(&linked_model, cache.join("linked-file.gguf")).expect("model symlink");
        symlink(
            outside.join("missing.gguf"),
            cache.join("dangling-file.gguf"),
        )
        .expect("dangling model symlink");

        let mut models = Vec::new();
        collect_cached_models(&cache, 0, &mut models);

        assert_eq!(models, vec![cache.join("linked-file.gguf")]);
        std::fs::remove_dir_all(&temporary).expect("remove temporary cache");
    }

    #[test]
    fn projector_discovery_is_bounded_to_zero_one_or_typed_ambiguity() {
        let temporary =
            std::env::temp_dir().join(format!("mom-llama-projector-discovery-{}", crate::now_ms()));
        std::fs::create_dir_all(&temporary).expect("projector directory");
        let model = temporary.join("Qwen3.5-27B-Q8_0.gguf");
        std::fs::write(&model, b"GGUF").expect("model");

        assert_eq!(
            discover_projector_for_model(&model).expect("zero projectors"),
            None
        );

        let projector = temporary.join("mmproj-F16.gguf");
        std::fs::write(&projector, b"GGUF").expect("projector");
        assert_eq!(
            discover_projector_for_model(&model).expect("one projector"),
            Some(projector.clone())
        );

        std::fs::write(temporary.join("mmproj-BF16.gguf"), b"GGUF").expect("second projector");
        let blocked = discover_projector_for_model(&model).expect_err("ambiguous projectors");
        assert_eq!(blocked.readiness, "blocked_ambiguous_projector");
        assert_eq!(blocked.blocker.code, "mmproj_path_ambiguous");
        assert!(
            !blocked
                .blocker
                .message
                .contains(&temporary.display().to_string())
        );

        // MTP files are draft models, not vision projectors.
        std::fs::remove_file(temporary.join("mmproj-BF16.gguf")).expect("remove ambiguity");
        std::fs::write(temporary.join("Qwen3.5-27B-Q8_0-MTP.gguf"), b"GGUF").expect("MTP model");
        assert_eq!(
            discover_projector_for_model(&model).expect("MTP ignored"),
            Some(projector)
        );

        std::fs::remove_dir_all(&temporary).expect("remove temporary projector directory");
    }

    #[test]
    fn selecting_a_model_clears_or_replaces_the_previous_projector_pair() {
        let mut settings = crate::Settings::defaults_for_data_dir(std::env::temp_dir());
        settings.mmproj_path = Some(PathBuf::from("/old/mmproj.gguf"));
        settings.upstream_settings.insert(
            "mmprojPath".to_string(),
            serde_json::json!("/old/mmproj.gguf"),
        );

        bind_model_pair(&mut settings, PathBuf::from("/models/text.gguf"), None);
        assert_eq!(settings.mmproj_path, None);
        assert_eq!(
            settings.upstream_settings["mmprojPath"],
            serde_json::json!("")
        );

        bind_model_pair(
            &mut settings,
            PathBuf::from("/models/vision.gguf"),
            Some(PathBuf::from("/models/mmproj-F16.gguf")),
        );
        assert_eq!(
            settings.mmproj_path,
            Some(PathBuf::from("/models/mmproj-F16.gguf"))
        );
        assert_eq!(
            settings.upstream_settings["mmprojPath"],
            serde_json::json!("/models/mmproj-F16.gguf")
        );
    }

    #[test]
    fn latest_default_model_intent_wins_when_fast_new_finishes_before_slow_old() {
        let scope = PathBuf::from("/data/mom");
        let mut order = DefaultModelSelectionOrder::default();
        let slow_old = order.begin(scope.clone());
        let fast_new = order.begin(scope.clone());

        let mut committed = None;
        if order.is_current(&fast_new) {
            committed = Some("fast-new");
        }
        if order.is_current(&slow_old) {
            committed = Some("slow-old");
        }

        assert_eq!(committed, Some("fast-new"));
        assert!(order.is_current(&fast_new));
        assert!(!order.is_current(&slow_old));

        let independent = order.begin(PathBuf::from("/data/other-mom"));
        assert!(order.is_current(&fast_new));
        assert!(order.is_current(&independent));
    }

    #[test]
    fn launch_time_model_override_remains_authoritative() {
        let override_path = PathBuf::from("/models/override.gguf");
        assert!(reject_conflicting_runtime_override(&override_path, None).is_ok());
        assert!(reject_conflicting_runtime_override(&override_path, Some(&override_path)).is_ok());
        let blocked = reject_conflicting_runtime_override(
            PathBuf::from("/models/other.gguf").as_path(),
            Some(&override_path),
        )
        .expect_err("a different persisted choice cannot outrank the runtime override");
        assert_eq!(blocked.readiness, "blocked_runtime_override");
        assert_eq!(blocked.blocker.code, "model_runtime_override_active");
    }

    #[test]
    fn projector_discovery_fails_closed_when_the_sibling_bound_is_truncated() {
        let temporary =
            std::env::temp_dir().join(format!("mom-llama-projector-bound-{}", crate::now_ms()));
        std::fs::create_dir_all(&temporary).expect("bounded directory");
        let model = temporary.join("model.gguf");
        std::fs::write(&model, b"GGUF").expect("model");
        for index in 0..MAX_PROJECTOR_SIBLINGS {
            std::fs::write(temporary.join(format!("note-{index:03}.txt")), b"x")
                .expect("bounded sibling");
        }
        let blocked = discover_projector_for_model(&model).expect_err("scan must fail closed");
        assert_eq!(blocked.blocker.code, "projector_directory_too_large");
        assert!(
            !blocked
                .blocker
                .message
                .contains(&temporary.display().to_string())
        );
        std::fs::remove_dir_all(&temporary).expect("remove bounded directory");
    }
}
