//! Rust-only Tauri 2 embedding surface for Free Token Energy.

use fte_loopback::{LoopbackConfig, LoopbackServer};
#[cfg(feature = "hosted-providers")]
use fte_providers::{HostedProviderBackend, HostedProviderConfig};
use fte_router::{Gateway, GatewayDefaults};
use fte_speech_gateway::{SpeechGateway, SpeechGatewayError, SpeechGatewayStatus};
use fte_speech_router::SpeechRoutePlan;
use fte_speech_types::{
    AudioChunk, SpeechBackend, SpeechError, SpeechRequestId, SynthesisEvent, SynthesisRequest,
    SynthesisResponse, TranscriptionAudioSink, TranscriptionEvent, TranscriptionRequest,
    TranscriptionResponse,
};
use fte_store::{ResponseStore, SecretResolver, SqliteStore};
use fte_types::{
    CancelTarget, GatewayBackend, GatewayError, GatewayEvent, GatewayRequest, GatewayResponse,
    GatewayStatus, LoopbackStatus, ModelDescriptor, RequestId,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, Runtime, State};

pub struct Builder {
    gateway: Arc<Gateway>,
    speech: Arc<SpeechGateway>,
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
            speech: Arc::new(SpeechGateway::default()),
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
    pub fn with_speech_gateway(mut self, speech: Arc<SpeechGateway>) -> Self {
        self.speech = speech;
        self
    }

    pub fn register_speech_backend(
        self,
        backend: Arc<dyn SpeechBackend>,
    ) -> Result<Self, SpeechGatewayError> {
        self.speech.register_backend(backend)?;
        Ok(self)
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
        let speech = self.speech;
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
                speech_status,
                speech_plan_transcription,
                speech_plan_synthesis,
                speech_synthesize,
                speech_synthesize_stream,
                speech_transcribe,
                speech_transcribe_stream,
                speech_transcription_audio_push,
                speech_transcription_audio_finish,
                speech_cancel,
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
                    speech,
                    speech_inputs: Mutex::new(HashMap::new()),
                    store,
                    loopback_config,
                    loopback: Mutex::new(None),
                });
                Ok(())
            })
            .on_drop(|app| {
                let state = app.state::<PluginState>();
                let server = state
                    .loopback
                    .lock()
                    .ok()
                    .and_then(|mut loopback| loopback.take());
                let gateway = Arc::clone(&state.gateway);
                let speech = Arc::clone(&state.speech);
                tauri::async_runtime::block_on(async move {
                    if let Some(server) = server {
                        server.shutdown().await;
                    }
                    let _ = gateway.shutdown().await;
                    let _ = speech.shutdown().await;
                });
            })
            .build()
    }
}

pub struct PluginState {
    gateway: Arc<Gateway>,
    speech: Arc<SpeechGateway>,
    speech_inputs: Mutex<HashMap<SpeechRequestId, Arc<dyn TranscriptionAudioSink>>>,
    store: Arc<dyn ResponseStore>,
    loopback_config: Option<LoopbackConfig>,
    loopback: Mutex<Option<LoopbackServer>>,
}

pub trait FreeTokenEnergyExt<R: Runtime> {
    fn free_token_energy(&self) -> Arc<Gateway>;
    fn free_token_energy_speech(&self) -> Arc<SpeechGateway>;
}

impl<R: Runtime, T: Manager<R>> FreeTokenEnergyExt<R> for T {
    fn free_token_energy(&self) -> Arc<Gateway> {
        Arc::clone(&self.state::<PluginState>().gateway)
    }

    fn free_token_energy_speech(&self) -> Arc<SpeechGateway> {
        Arc::clone(&self.state::<PluginState>().speech)
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
fn speech_status(state: State<'_, PluginState>) -> Result<SpeechGatewayStatus, SpeechGatewayError> {
    state.speech.status()
}

#[tauri::command]
fn speech_plan_transcription(
    request: TranscriptionRequest,
    state: State<'_, PluginState>,
) -> Result<SpeechRoutePlan, SpeechGatewayError> {
    state.speech.plan_transcription(&request)
}

#[tauri::command]
fn speech_plan_synthesis(
    request: SynthesisRequest,
    state: State<'_, PluginState>,
) -> Result<SpeechRoutePlan, SpeechGatewayError> {
    state.speech.plan_synthesis(&request)
}

#[tauri::command]
async fn speech_synthesize(
    request: SynthesisRequest,
    state: State<'_, PluginState>,
) -> Result<SynthesisResponse, SpeechGatewayError> {
    let mut ticket = state.speech.synthesize(request).await?;
    while let Some(event) = ticket.events.recv().await {
        if event.is_terminal() {
            break;
        }
    }
    ticket.final_response().await.map_err(Into::into)
}

#[tauri::command]
async fn speech_synthesize_stream(
    request: SynthesisRequest,
    on_event: Channel<SynthesisEvent>,
    state: State<'_, PluginState>,
) -> Result<SynthesisResponse, SpeechGatewayError> {
    let request_id = request.context.request_id.clone();
    let mut ticket = state.speech.synthesize(request).await?;
    while let Some(event) = ticket.events.recv().await {
        let terminal = event.is_terminal();
        on_event
            .send(event)
            .map_err(|_| speech_channel_closed(&request_id))?;
        if terminal {
            break;
        }
    }
    ticket.final_response().await.map_err(Into::into)
}

#[tauri::command]
async fn speech_transcribe(
    request: TranscriptionRequest,
    state: State<'_, PluginState>,
) -> Result<TranscriptionResponse, SpeechGatewayError> {
    let mut ticket = state.speech.transcribe(request).await?;
    while let Some(event) = ticket.events.recv().await {
        if event.is_terminal() {
            break;
        }
    }
    ticket.final_response().await.map_err(Into::into)
}

#[tauri::command]
async fn speech_transcribe_stream(
    request: TranscriptionRequest,
    on_event: Channel<TranscriptionEvent>,
    state: State<'_, PluginState>,
) -> Result<TranscriptionResponse, SpeechGatewayError> {
    let request_id = request.context.request_id.clone();
    let mut ticket = state.speech.transcribe(request).await?;
    if let Some(sink) = ticket.audio_sink.clone() {
        state
            .speech_inputs
            .lock()
            .map_err(|_| speech_input_state_unavailable(&request_id))?
            .insert(request_id.clone(), sink);
    }
    let mut channel_error = None;
    while let Some(event) = ticket.events.recv().await {
        let terminal = event.is_terminal();
        if on_event.send(event).is_err() {
            channel_error = Some(speech_channel_closed(&request_id));
            break;
        }
        if terminal {
            break;
        }
    }
    if let Ok(mut inputs) = state.speech_inputs.lock() {
        inputs.remove(&request_id);
    }
    if let Some(error) = channel_error {
        return Err(error);
    }
    ticket.final_response().await.map_err(Into::into)
}

#[tauri::command]
async fn speech_transcription_audio_push(
    request_id: String,
    chunk: AudioChunk,
    state: State<'_, PluginState>,
) -> Result<(), SpeechGatewayError> {
    let request_id = SpeechRequestId(request_id);
    let sink = state
        .speech_inputs
        .lock()
        .map_err(|_| speech_input_state_unavailable(&request_id))?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| speech_input_missing(&request_id))?;
    sink.push(chunk).await.map_err(Into::into)
}

#[tauri::command]
async fn speech_transcription_audio_finish(
    request_id: String,
    state: State<'_, PluginState>,
) -> Result<(), SpeechGatewayError> {
    let request_id = SpeechRequestId(request_id);
    let sink = state
        .speech_inputs
        .lock()
        .map_err(|_| speech_input_state_unavailable(&request_id))?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| speech_input_missing(&request_id))?;
    sink.finish().await.map_err(Into::into)
}

#[tauri::command]
fn speech_cancel(request_id: String, state: State<'_, PluginState>) -> CancelResult {
    CancelResult {
        cancelled: state.speech.cancel(&SpeechRequestId(request_id)),
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
    let mut config = state.loopback_config.clone().ok_or_else(|| {
        GatewayError::invalid_request(
            &RequestId::new(),
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
    let previous = state
        .loopback
        .lock()
        .map_err(plugin_lock_error)?
        .replace(server);
    if let Some(previous) = previous {
        previous.shutdown().await;
    }
    Ok(result)
}

#[tauri::command]
async fn loopback_stop(state: State<'_, PluginState>) -> Result<LoopbackStatus, GatewayError> {
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
    GatewayError::unavailable(
        &RequestId::new(),
        "plugin_state_unavailable",
        "Free Token Energy plugin state is unavailable",
    )
}

fn speech_channel_closed(request_id: &SpeechRequestId) -> SpeechGatewayError {
    SpeechError::unavailable(
        request_id,
        "tauri_speech_event_consumer_closed",
        "the Tauri speech event consumer closed before the request completed",
    )
    .into()
}

fn speech_input_state_unavailable(request_id: &SpeechRequestId) -> SpeechGatewayError {
    SpeechError::unavailable(
        request_id,
        "tauri_speech_input_state_unavailable",
        "the Tauri streaming speech-input registry is unavailable",
    )
    .into()
}

fn speech_input_missing(request_id: &SpeechRequestId) -> SpeechGatewayError {
    SpeechError::unavailable(
        request_id,
        "tauri_speech_input_missing",
        "no active streaming transcription accepts audio for this request",
    )
    .into()
}
