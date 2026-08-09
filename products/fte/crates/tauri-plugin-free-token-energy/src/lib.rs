//! Rust-only Tauri 2 embedding surface for Free Token Energy.

use fte_loopback::{LoopbackConfig, LoopbackServer};
#[cfg(feature = "hosted-providers")]
use fte_providers::{HostedProviderBackend, HostedProviderConfig};
use fte_router::{Gateway, GatewayDefaults};
use fte_store::{ResponseStore, SecretResolver, SqliteStore};
use fte_types::{
    CancelTarget, GatewayBackend, GatewayError, GatewayEvent, GatewayRequest, GatewayResponse,
    GatewayStatus, LoopbackStatus, ModelDescriptor, RequestId,
};
use serde::Serialize;
use std::sync::{Arc, Condvar, Mutex};
use tauri::ipc::Channel;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, RunEvent, Runtime, State};

pub struct Builder {
    gateway: Arc<Gateway>,
    store: Option<Arc<dyn ResponseStore>>,
    secret_resolver: Option<Arc<dyn SecretResolver>>,
    loopback: Option<LoopbackConfig>,
    default_loopback: bool,
    #[cfg(feature = "native-llama")]
    native_backend: Option<Arc<fte_backend_llama::LlamaNativeBackend>>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            gateway: Arc::new(Gateway::new(GatewayDefaults::default())),
            store: None,
            secret_resolver: None,
            loopback: None,
            default_loopback: false,
            #[cfg(feature = "native-llama")]
            native_backend: None,
        }
    }

    #[must_use]
    pub fn with_defaults(mut self, defaults: GatewayDefaults) -> Self {
        self.gateway = Arc::new(Gateway::new(defaults));
        self
    }

    #[must_use]
    pub fn with_gateway(mut self, gateway: Arc<Gateway>) -> Self {
        self.gateway = gateway;
        self
    }

    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ResponseStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Supplies the application-owned hosted credential boundary. Provider
    /// secrets are resolved once by provider adapters and never exposed over
    /// Tauri IPC or loopback.
    #[must_use]
    pub fn with_secret_resolver(mut self, resolver: Arc<dyn SecretResolver>) -> Self {
        self.secret_resolver = Some(resolver);
        self
    }

    /// Registers a hosted provider using the previously supplied secret
    /// resolver. This keeps provider assembly independent of Tauri state.
    #[cfg(feature = "hosted-providers")]
    pub fn register_provider(self, config: HostedProviderConfig) -> Result<Self, GatewayError> {
        let resolver = self.secret_resolver.clone().ok_or_else(|| {
            GatewayError::invalid_request(
                &RequestId::new(),
                "secret_resolver_missing",
                "call with_secret_resolver before registering a hosted provider",
            )
        })?;
        self.gateway
            .register_backend(Arc::new(HostedProviderBackend::new(config, resolver)?))?;
        Ok(self)
    }

    /// Registers a product-owned in-process llama host. Enable the
    /// `native-llama` feature and add one or more exact model profiles with
    /// `register_native_model` before serving requests.
    #[cfg(feature = "native-llama")]
    pub fn with_native_host(
        mut self,
        host: Arc<llama_native_host::NativeHost>,
    ) -> Result<Self, GatewayError> {
        let backend = Arc::new(fte_backend_llama::LlamaNativeBackend::new(host));
        self.gateway.register_backend(backend.clone())?;
        self.native_backend = Some(backend);
        Ok(self)
    }

    #[cfg(feature = "native-llama")]
    pub fn register_native_model(
        self,
        model: llama_native_types::NativeModelConfig,
    ) -> Result<Self, GatewayError> {
        let backend = self.native_backend.as_ref().ok_or_else(|| {
            GatewayError::invalid_request(
                &RequestId::new(),
                "native_host_missing",
                "call with_native_host before registering a local model",
            )
        })?;
        backend.configure_model(model)?;
        Ok(self)
    }

    #[must_use]
    pub fn with_loopback(mut self, config: LoopbackConfig) -> Self {
        self.loopback = Some(config);
        self
    }

    /// Makes loopback available for explicit start without starting a listener.
    /// The bearer token and response database live in the Tauri app-data directory.
    #[must_use]
    pub fn with_default_loopback(mut self) -> Self {
        self.default_loopback = true;
        self
    }

    pub fn register_backend(self, backend: Arc<dyn GatewayBackend>) -> Result<Self, GatewayError> {
        self.gateway.register_backend(backend)?;
        Ok(self)
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        let gateway = self.gateway;
        let store = self.store;
        let loopback_config = self.loopback;
        let default_loopback = self.default_loopback;
        PluginBuilder::new("free-token-energy")
            .invoke_handler(tauri::generate_handler![
                gateway_status,
                gateway_models,
                gateway_generate,
                gateway_stream,
                gateway_cancel,
                loopback_status,
                loopback_start,
                loopback_stop,
                loopback_rotate_token,
            ])
            .setup(move |app, _api| {
                let app_data_dir = app.path().app_data_dir()?;
                std::fs::create_dir_all(&app_data_dir)?;
                let store: Arc<dyn ResponseStore> = match store {
                    Some(store) => store,
                    None => Arc::new(SqliteStore::open(app_data_dir.join("gateway-v2.db"))?),
                };
                let loopback_config = loopback_config.or_else(|| {
                    default_loopback
                        .then(|| LoopbackConfig::app_private(app_data_dir.join("loopback-token")))
                });
                app.manage(PluginState {
                    gateway,
                    store,
                    loopback_config,
                    loopback: Mutex::new(None),
                    cleanup: Arc::new(CleanupCoordinator::default()),
                });
                Ok(())
            })
            // Tauri dispatches plugin events before the embedding app's
            // `App::run` callback. Draining here guarantees that the app may
            // subsequently close its application-owned native host without a
            // gateway bridge or provider task still using it.
            .on_event(|app, event| {
                if matches!(event, RunEvent::Exit)
                    && let Some(state) = app.try_state::<PluginState>()
                {
                    state.cleanup_blocking();
                }
            })
            .on_drop(|app| {
                // Setup may fail before PluginState is managed. Plugin drop
                // must remain harmless on that startup error path.
                if let Some(state) = app.try_state::<PluginState>() {
                    state.cleanup_blocking();
                }
            })
            .build()
    }
}

pub struct PluginState {
    gateway: Arc<Gateway>,
    store: Arc<dyn ResponseStore>,
    loopback_config: Option<LoopbackConfig>,
    loopback: Mutex<Option<LoopbackServer>>,
    cleanup: Arc<CleanupCoordinator>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCleanupPhase {
    #[default]
    Pending,
    Draining,
    Complete,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PluginCleanupStatus {
    pub phase: PluginCleanupPhase,
    pub error: Option<GatewayError>,
}

#[derive(Default)]
struct CleanupState {
    phase: PluginCleanupPhase,
    active_operations: usize,
    error: Option<GatewayError>,
}

#[derive(Default)]
struct CleanupCoordinator {
    state: Mutex<CleanupState>,
    changed: Condvar,
}

impl CleanupCoordinator {
    fn begin_operation(
        self: &Arc<Self>,
        request_id: &RequestId,
    ) -> Result<CleanupOperation, GatewayError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.phase != PluginCleanupPhase::Pending {
            return Err(plugin_quiescing_error(request_id));
        }
        state.active_operations = state.active_operations.saturating_add(1);
        Ok(CleanupOperation {
            cleanup: Arc::clone(self),
        })
    }

    fn while_pending<T>(
        &self,
        request_id: &RequestId,
        operation: impl FnOnce() -> Result<T, GatewayError>,
    ) -> Result<T, GatewayError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.phase != PluginCleanupPhase::Pending {
            return Err(plugin_quiescing_error(request_id));
        }
        operation()
    }

    fn start_or_wait(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match state.phase {
                PluginCleanupPhase::Pending => {
                    state.phase = PluginCleanupPhase::Draining;
                    while state.active_operations != 0 {
                        state = self
                            .changed
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    return true;
                }
                PluginCleanupPhase::Draining => {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                PluginCleanupPhase::Complete => return false,
            }
        }
    }

    fn finish(&self, error: Option<GatewayError>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.phase = PluginCleanupPhase::Complete;
        state.error = error;
        self.changed.notify_all();
    }

    fn status(&self) -> PluginCleanupStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PluginCleanupStatus {
            phase: state.phase,
            error: state.error.clone(),
        }
    }
}

struct CleanupOperation {
    cleanup: Arc<CleanupCoordinator>,
}

impl Drop for CleanupOperation {
    fn drop(&mut self) {
        let mut state = self
            .cleanup
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_operations = state.active_operations.saturating_sub(1);
        self.cleanup.changed.notify_all();
    }
}

impl PluginState {
    fn cleanup_blocking(&self) {
        if !self.cleanup.start_or_wait() {
            return;
        }
        let (server, mut error) = match self.loopback.lock() {
            Ok(mut loopback) => (loopback.take(), None),
            Err(poisoned) => (poisoned.into_inner().take(), Some(plugin_state_error())),
        };
        let gateway = Arc::clone(&self.gateway);
        // `on_drop` may be reached through dynamic plugin removal from a
        // Tokio task. Calling `Handle::block_on` from that task panics, so the
        // synchronous Tauri lifecycle boundary delegates the async drain to a
        // dedicated OS thread and joins it.
        let gateway_result = std::thread::Builder::new()
            .name("fte-plugin-cleanup".to_string())
            .spawn(move || {
                tauri::async_runtime::block_on(async move {
                    if let Some(server) = server {
                        // Stop accepting loopback traffic while gateway
                        // quiescing cancels tickets owned by active HTTP/SSE
                        // handlers. Sequential waits can deadlock on a stream.
                        let ((), gateway_result) =
                            tokio::join!(server.shutdown(), gateway.shutdown());
                        gateway_result
                    } else {
                        gateway.shutdown().await
                    }
                })
            })
            .map_err(|error| plugin_cleanup_thread_error("start", error))
            .and_then(|thread| {
                thread
                    .join()
                    .map_err(|_| plugin_cleanup_thread_error("join", "cleanup thread panicked"))?
            });
        if let Err(gateway_error) = gateway_result {
            error.get_or_insert(gateway_error);
        }
        if let Some(cleanup_error) = &error {
            eprintln!(
                "Free Token Energy cleanup failed ({}): {}",
                cleanup_error.code, cleanup_error.safe_detail
            );
        }
        self.cleanup.finish(error);
    }
}

pub trait FreeTokenEnergyExt<R: Runtime> {
    fn free_token_energy(&self) -> Arc<Gateway>;
    fn free_token_energy_cleanup_status(&self) -> PluginCleanupStatus;
}

impl<R: Runtime, T: Manager<R>> FreeTokenEnergyExt<R> for T {
    fn free_token_energy(&self) -> Arc<Gateway> {
        Arc::clone(&self.state::<PluginState>().gateway)
    }

    fn free_token_energy_cleanup_status(&self) -> PluginCleanupStatus {
        self.state::<PluginState>().cleanup.status()
    }
}

#[derive(Debug, Clone, Serialize)]
struct CancelResult {
    cancelled: usize,
}

#[tauri::command]
fn gateway_status(state: State<'_, PluginState>) -> GatewayStatus {
    state.gateway.status()
}

#[tauri::command]
fn gateway_models(state: State<'_, PluginState>) -> Vec<ModelDescriptor> {
    state.gateway.models()
}

#[tauri::command]
async fn gateway_generate(
    request: GatewayRequest,
    state: State<'_, PluginState>,
) -> Result<GatewayResponse, GatewayError> {
    let mut ticket = state.gateway.execute(request).await?;
    while let Some(event) = ticket.events.recv().await {
        if event.is_terminal() {
            break;
        }
    }
    ticket.final_response().await
}

/// Streams the canonical typed event model over a bounded Tauri IPC channel.
/// Closing the receiving channel drops the ticket and cancels only this request.
#[tauri::command]
async fn gateway_stream(
    request: GatewayRequest,
    on_event: Channel<GatewayEvent>,
    state: State<'_, PluginState>,
) -> Result<GatewayResponse, GatewayError> {
    let request_id = request.request_id.clone();
    let mut ticket = state.gateway.execute(request).await?;
    while let Some(event) = ticket.events.recv().await {
        let terminal = event.is_terminal();
        on_event.send(event).map_err(|_| {
            GatewayError::unavailable(
                &request_id,
                "tauri_event_consumer_closed",
                "the Tauri event consumer closed before generation completed",
            )
        })?;
        if terminal {
            break;
        }
    }
    ticket.final_response().await
}

#[tauri::command]
fn gateway_cancel(
    request_id: String,
    output_index: Option<usize>,
    state: State<'_, PluginState>,
) -> CancelResult {
    let target = output_index.map_or(CancelTarget::Request, CancelTarget::Output);
    CancelResult {
        cancelled: state.gateway.cancel(&RequestId(request_id), target),
    }
}

#[tauri::command]
fn loopback_status(state: State<'_, PluginState>) -> LoopbackStatus {
    state
        .loopback
        .lock()
        .ok()
        .and_then(|loopback| {
            loopback.as_ref().map(|server| LoopbackStatus {
                enabled: true,
                addresses: server.addresses().iter().map(ToString::to_string).collect(),
                token_path: Some(server.token_path().display().to_string()),
            })
        })
        .unwrap_or(LoopbackStatus {
            enabled: false,
            addresses: Vec::new(),
            token_path: state
                .loopback_config
                .as_ref()
                .map(|config| config.token_path.display().to_string()),
        })
}

#[tauri::command]
async fn loopback_start(
    port: Option<u16>,
    state: State<'_, PluginState>,
) -> Result<LoopbackStatus, GatewayError> {
    let request_id = RequestId::new();
    let _operation = state.cleanup.begin_operation(&request_id)?;
    let mut config = state.loopback_config.clone().ok_or_else(|| {
        GatewayError::invalid_request(
            &request_id,
            "loopback_not_configured",
            "the embedding application did not configure loopback access",
        )
    })?;
    if let Some(port) = port {
        config.port = port;
    }
    let existing_matches = state
        .loopback
        .lock()
        .map_err(plugin_lock_error)?
        .as_ref()
        .is_some_and(|server| {
            config.port != 0
                && server
                    .addresses()
                    .iter()
                    .any(|address| address.port() == config.port)
        });
    if existing_matches {
        return Ok(loopback_status(state));
    }
    let server =
        LoopbackServer::start(Arc::clone(&state.gateway), Arc::clone(&state.store), config).await?;
    let result = LoopbackStatus {
        enabled: true,
        addresses: server.addresses().iter().map(ToString::to_string).collect(),
        token_path: Some(server.token_path().display().to_string()),
    };
    let mut server = Some(server);
    let install = state.cleanup.while_pending(&request_id, || {
        let mut loopback = state.loopback.lock().map_err(plugin_lock_error)?;
        let server = server.take().ok_or_else(|| {
            GatewayError::unavailable(
                &request_id,
                "loopback_install_state_invalid",
                "the loopback listener could not be installed",
            )
        })?;
        Ok(loopback.replace(server))
    });
    let previous = match install {
        Ok(previous) => previous,
        Err(error) => {
            if let Some(server) = server {
                server.shutdown().await;
            }
            return Err(error);
        }
    };
    if let Some(previous) = previous {
        previous.shutdown().await;
    }
    Ok(result)
}

#[tauri::command]
async fn loopback_stop(state: State<'_, PluginState>) -> Result<LoopbackStatus, GatewayError> {
    let _operation = state.cleanup.begin_operation(&RequestId::new())?;
    let server = state.loopback.lock().map_err(plugin_lock_error)?.take();
    if let Some(server) = server {
        server.shutdown().await;
    }
    Ok(LoopbackStatus {
        enabled: false,
        addresses: Vec::new(),
        token_path: state
            .loopback_config
            .as_ref()
            .map(|config| config.token_path.display().to_string()),
    })
}

#[tauri::command]
fn loopback_rotate_token(state: State<'_, PluginState>) -> Result<LoopbackStatus, GatewayError> {
    let _operation = state.cleanup.begin_operation(&RequestId::new())?;
    let loopback = state.loopback.lock().map_err(plugin_lock_error)?;
    let server = loopback.as_ref().ok_or_else(|| {
        GatewayError::invalid_request(
            &RequestId::new(),
            "loopback_not_running",
            "start the loopback listener before rotating its token",
        )
    })?;
    server.rotate_token()?;
    Ok(LoopbackStatus {
        enabled: true,
        addresses: server.addresses().iter().map(ToString::to_string).collect(),
        token_path: Some(server.token_path().display().to_string()),
    })
}

fn plugin_lock_error<T>(_error: std::sync::PoisonError<T>) -> GatewayError {
    plugin_state_error()
}

fn plugin_state_error() -> GatewayError {
    GatewayError::unavailable(
        &RequestId::new(),
        "plugin_state_unavailable",
        "Free Token Energy plugin state is unavailable",
    )
}

fn plugin_cleanup_thread_error(stage: &str, error: impl std::fmt::Display) -> GatewayError {
    GatewayError::unavailable(
        &RequestId::new(),
        "plugin_cleanup_thread_failed",
        &format!("Free Token Energy could not {stage} its cleanup thread: {error}"),
    )
}

fn plugin_quiescing_error(request_id: &RequestId) -> GatewayError {
    GatewayError::unavailable(
        request_id,
        "plugin_quiescing",
        "Free Token Energy is draining and no longer accepts lifecycle changes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_coordinator_is_idempotent_and_retains_safe_error() {
        let cleanup = CleanupCoordinator::default();
        assert!(cleanup.start_or_wait());
        assert_eq!(cleanup.status().phase, PluginCleanupPhase::Draining);
        let error = GatewayError::unavailable(
            &RequestId("cleanup-test".to_string()),
            "cleanup_failed",
            "the deterministic cleanup fixture failed",
        );
        cleanup.finish(Some(error.clone()));

        assert!(!cleanup.start_or_wait());
        assert_eq!(
            cleanup.status(),
            PluginCleanupStatus {
                phase: PluginCleanupPhase::Complete,
                error: Some(error),
            }
        );
    }

    #[test]
    fn cleanup_waits_for_in_flight_loopback_ownership() {
        let cleanup = Arc::new(CleanupCoordinator::default());
        let operation = cleanup
            .begin_operation(&RequestId("loopback-start".to_string()))
            .expect("begin loopback operation");
        let cleanup_for_thread = Arc::clone(&cleanup);
        let cleanup_thread = std::thread::spawn(move || {
            assert!(cleanup_for_thread.start_or_wait());
            cleanup_for_thread.finish(None);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while cleanup.status().phase != PluginCleanupPhase::Draining {
            assert!(
                std::time::Instant::now() < deadline,
                "cleanup must enter its draining phase"
            );
            std::thread::yield_now();
        }
        assert!(!cleanup_thread.is_finished());
        drop(operation);
        cleanup_thread.join().expect("cleanup thread");
        assert_eq!(cleanup.status().phase, PluginCleanupPhase::Complete);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_cleanup_is_safe_inside_an_async_runtime() {
        let state = PluginState {
            gateway: Arc::new(Gateway::new(GatewayDefaults::default())),
            store: Arc::new(SqliteStore::in_memory().expect("in-memory response store")),
            loopback_config: None,
            loopback: Mutex::new(None),
            cleanup: Arc::new(CleanupCoordinator::default()),
        };

        state.cleanup_blocking();

        assert_eq!(state.cleanup.status().phase, PluginCleanupPhase::Complete);
        assert!(state.cleanup.status().error.is_none());
    }
}
