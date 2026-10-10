//! Tauri transport only. Validation, merging and disk state live in the shared
//! preference service; the application admission lock covers the complete write.
use crate::{IpcFailure, PluginState, lock_application_admission};
use loom_preferences::{PreferenceChange, PreferenceStore, Preferences};
use loom_types::ProjectId;
use tauri::{AppHandle, Manager, Runtime};

async fn with_preferences<R: Runtime, T: Send + 'static>(
    app: AppHandle<R>,
    operation: impl FnOnce(PreferenceStore, &PluginState) -> Result<T, loom_preferences::Error>
    + Send
    + 'static,
) -> Result<T, IpcFailure> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<PluginState>();
        let _admission = lock_application_admission(&state, "application preferences")?;
        let root = state.app_local_data_root.as_ref().ok_or_else(|| {
            IpcFailure::new(
                "preferences_directory_unavailable",
                "The operating system did not provide an application data directory",
                true,
            )
        })?;
        operation(PreferenceStore::new(root.join("settings")), &state).map_err(|error| {
            let retryable = matches!(
                error,
                loom_preferences::Error::Io(_) | loom_preferences::Error::Busy
            );
            let code = if matches!(error, loom_preferences::Error::Busy) {
                "preferences_busy"
            } else {
                "preferences_failed"
            };
            IpcFailure::new(code, error.to_string(), retryable)
        })
    })
    .await
    .map_err(|error| IpcFailure::new("preferences_worker_failed", error.to_string(), false))?
}

#[tauri::command]
pub(crate) async fn preferences_get<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Preferences, IpcFailure> {
    with_preferences(app, |store, _| store.read()).await
}

#[tauri::command]
pub(crate) async fn preferences_update<R: Runtime>(
    app: AppHandle<R>,
    change: PreferenceChange,
) -> Result<Preferences, IpcFailure> {
    with_preferences(app, move |store, _| store.update(change)).await
}

#[tauri::command]
pub(crate) async fn preferences_suggestions_get<R: Runtime>(
    app: AppHandle<R>,
    project_id: ProjectId,
) -> Result<bool, IpcFailure> {
    with_preferences(app, move |store, state| {
        Ok(store
            .read()?
            .suggestions_enabled(project_id, Some(state.build_model_policy.activation())))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn adapter_uses_configured_root_and_refuses_writes_after_close_intent() {
        let root = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_app();
        app.manage(PluginState::with_app_local_data_root(
            Some(root.path().to_owned()),
            true,
            crate::BuildModelPolicy::default(),
        ));
        let reply = tauri::async_runtime::block_on(preferences_update(
            app.handle().clone(),
            PreferenceChange::RememberModel {
                path: "/models/first.gguf".into(),
            },
        ))
        .unwrap();
        assert_eq!(
            reply.last_local_model.as_deref(),
            Some("/models/first.gguf")
        );
        let store = PreferenceStore::new(root.path().join("settings"));
        assert_eq!(store.read().unwrap(), reply);
        app.state::<PluginState>()
            .close_requested
            .store(true, Ordering::Release);
        let error = tauri::async_runtime::block_on(preferences_update(
            app.handle().clone(),
            PreferenceChange::RememberModel {
                path: "/models/second.gguf".into(),
            },
        ))
        .unwrap_err();
        assert_eq!(error.code, "application_quiescing");
        assert_eq!(store.read().unwrap(), reply);
    }
}
