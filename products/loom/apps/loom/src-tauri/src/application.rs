use std::sync::Arc;

use llama_native_host::{
    ApplicationNativeFinalizer, ApplicationNativeOwner, JoinedApplicationNative,
    ProcessExitJoinedApplicationNative,
};
use llama_native_types::{NativeError, NativeErrorCode};
use loom_backend_llama::NativeHostRuntime;
use mom_llama_app::EmbeddedMom;

/// Native construction and finalization belong to the normal Loom root.
/// Document workers drain through the Loom plugin before this coordinator
/// drains Mom's product services and joins the one shared host.
pub(crate) struct Application {
    owner: ApplicationNativeOwner,
    mom: EmbeddedMom,
}

impl std::fmt::Debug for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Application")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl Application {
    pub(crate) fn new() -> Result<(Arc<Self>, Arc<NativeHostRuntime>), NativeError> {
        let owner =
            ApplicationNativeOwner::new(mom_llama_runtime::native_runtime::application_host);
        let document = owner.client()?;
        let mom = EmbeddedMom::new(owner.client()?);
        let application = Arc::new(Self { owner, mom });
        let finalizer: Arc<dyn ApplicationNativeFinalizer> = application.clone();
        let document_runtime = Arc::new(NativeHostRuntime::from_application(document, finalizer));
        Ok((application, document_runtime))
    }

    pub(crate) fn configure(
        &self,
        builder: tauri::Builder<tauri::Wry>,
    ) -> tauri::Builder<tauri::Wry> {
        self.mom.configure(builder)
    }

    fn drain_mom(&self) -> Result<(), NativeError> {
        tauri::async_runtime::block_on(self.mom.drain()).map_err(|error| {
            NativeError::new(
                NativeErrorCode::Internal,
                format!("Mom service drain failed: {error}"),
            )
        })
    }
}

impl ApplicationNativeFinalizer for Application {
    fn shutdown_joined(&self) -> Result<JoinedApplicationNative, NativeError> {
        self.drain_mom()?;
        self.owner.shutdown_joined()
    }

    fn shutdown_for_process_exit(&self) -> ProcessExitJoinedApplicationNative {
        let drained = self.drain_mom();
        let joined = self.owner.shutdown_for_process_exit();
        if let Err(error) = drained {
            eprintln!(
                "Loom drained its native host but cannot prove Mom service shutdown: {}",
                error.message
            );
            std::process::abort();
        }
        joined
    }
}
