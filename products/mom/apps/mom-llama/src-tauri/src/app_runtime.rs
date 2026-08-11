use fte_backend_llama::LlamaNativeBackend;
use fte_router::Gateway;
use llama_native_host::{NativeHost, ProcessExitJoinedNativeHost};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, OnceCell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Debug)]
struct AppLifecycle {
    phase: AppPhase,
    active_work: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownSummary {
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub gateway_drained: bool,
    pub native_host_joined: bool,
    pub joined_native_worker_count: usize,
    pub application_work_drained: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownError {
    pub summary: AppShutdownSummary,
    pub gateway_error: Option<String>,
    pub native_error: Option<String>,
}

impl std::fmt::Display for AppShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Mom Llama shutdown did not fully succeed")?;
        if let Some(error) = &self.gateway_error {
            write!(formatter, "; gateway: {error}")?;
        }
        if let Some(error) = &self.native_error {
            write!(formatter, "; native: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppShutdownError {}

struct AppRuntime {
    lifecycle: Mutex<AppLifecycle>,
    work_drained: Notify,
    gateway: Arc<Gateway>,
    native_backend: Arc<LlamaNativeBackend>,
    native_host: Arc<NativeHost>,
    shutdown: OnceCell<Result<AppShutdownSummary, AppShutdownError>>,
    joined_native_host: Mutex<Option<ProcessExitJoinedNativeHost>>,
}

#[derive(Clone)]
pub struct AppRuntimeHandle(Arc<AppRuntime>);

pub struct AppWorkLease(Option<Arc<AppRuntime>>);

impl Drop for AppWorkLease {
    fn drop(&mut self) {
        let Some(runtime) = self.0.take() else {
            return;
        };
        let drained = {
            let mut lifecycle = runtime
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lifecycle.active_work = lifecycle
                .active_work
                .checked_sub(1)
                .expect("an application work lease must be released exactly once");
            lifecycle.active_work == 0
        };
        if drained {
            runtime.work_drained.notify_waiters();
        }
    }
}

impl AppRuntimeHandle {
    pub fn new(
        gateway: Arc<Gateway>,
        native_backend: Arc<LlamaNativeBackend>,
        native_host: Arc<NativeHost>,
    ) -> Self {
        Self(Arc::new(AppRuntime {
            lifecycle: Mutex::new(AppLifecycle {
                phase: AppPhase::Running,
                active_work: 0,
            }),
            work_drained: Notify::new(),
            gateway,
            native_backend,
            native_host,
            shutdown: OnceCell::new(),
            joined_native_host: Mutex::new(None),
        }))
    }

    pub fn gateway(&self) -> Arc<Gateway> {
        Arc::clone(&self.0.gateway)
    }

    pub fn ensure_running(&self) -> Result<(), String> {
        let lifecycle = self
            .0
            .lifecycle
            .lock()
            .map_err(|_| "Mom Llama application state is unavailable".to_string())?;
        if lifecycle.phase == AppPhase::Running {
            Ok(())
        } else {
            Err("Mom Llama is shutting down; new work is not admitted".to_string())
        }
    }

    pub fn admit_work(&self) -> Result<AppWorkLease, String> {
        let mut lifecycle = self
            .0
            .lifecycle
            .lock()
            .map_err(|_| "Mom Llama application state is unavailable".to_string())?;
        if lifecycle.phase != AppPhase::Running {
            return Err("Mom Llama is shutting down; new work is not admitted".to_string());
        }
        lifecycle.active_work = lifecycle
            .active_work
            .checked_add(1)
            .ok_or_else(|| "Mom Llama has too many active operations".to_string())?;
        Ok(AppWorkLease(Some(Arc::clone(&self.0))))
    }

    /// Returns true only to the caller that closes application admission.
    pub fn begin_quiesce(&self) -> bool {
        let Ok(mut lifecycle) = self.0.lifecycle.lock() else {
            return false;
        };
        if lifecycle.phase != AppPhase::Running {
            return false;
        }
        lifecycle.phase = AppPhase::Quiescing;
        true
    }

    pub fn refresh_native_model(&self) -> Result<(), String> {
        self.ensure_running()?;
        let model = mom_llama_runtime::gateway_native_model_configuration(&self.0.native_host)
            .map_err(|error| format!("local gateway configuration failed: {error}"))?;
        self.0
            .native_backend
            .replace_configuration(Arc::clone(&self.0.native_host), model)
            .map_err(|error| format!("local gateway configuration failed: {error}"))
    }

    pub async fn shutdown(&self) -> Result<AppShutdownSummary, AppShutdownError> {
        self.begin_quiesce();
        self.0
            .shutdown
            .get_or_init(|| async {
                let started = Instant::now();
                let started_at_unix_ms = unix_time_ms();
                let gateway_error = self
                    .0
                    .gateway
                    .shutdown()
                    .await
                    .err()
                    .map(|error| error.to_string());
                let joined = mom_llama_runtime::shutdown_product_runtime_for_process_exit(
                    &self.0.native_host,
                );
                let (native_error, joined_native_worker_count) = match joined {
                    Ok(receipt) => {
                        let count = receipt.joined_worker_count();
                        self.0
                            .joined_native_host
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .replace(receipt);
                        (None, count)
                    }
                    Err(error) => (Some(error.to_string()), 0),
                };
                self.wait_for_work_drained().await;
                self.0
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .phase = AppPhase::Closed;
                let summary = AppShutdownSummary {
                    started_at_unix_ms,
                    completed_at_unix_ms: unix_time_ms(),
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    gateway_drained: gateway_error.is_none(),
                    native_host_joined: native_error.is_none(),
                    joined_native_worker_count,
                    application_work_drained: true,
                };
                if gateway_error.is_none() && native_error.is_none() {
                    Ok(summary)
                } else {
                    Err(AppShutdownError {
                        summary,
                        gateway_error,
                        native_error,
                    })
                }
            })
            .await
            .clone()
    }

    async fn wait_for_work_drained(&self) {
        loop {
            let notified = self.0.work_drained.notified();
            if self
                .0
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active_work
                == 0
            {
                return;
            }
            notified.await;
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::AppRuntimeHandle;
    use fte_backend_llama::LlamaNativeBackend;
    use fte_router::{Gateway, GatewayDefaults};
    use llama_native_host::{NativeHost, NativeHostConfig};
    use std::sync::Arc;

    fn runtime() -> AppRuntimeHandle {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        AppRuntimeHandle::new(
            Arc::new(Gateway::new(GatewayDefaults {
                catalog_version: "test".to_string(),
            })),
            Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host))),
            host,
        )
    }

    #[test]
    fn quiesce_closes_admission_once() {
        let runtime = runtime();
        assert!(runtime.ensure_running().is_ok());
        assert!(runtime.begin_quiesce());
        assert!(!runtime.begin_quiesce());
        assert!(runtime.ensure_running().is_err());
    }

    #[test]
    fn cloned_handles_share_one_admission_gate() {
        let first = runtime();
        let second = first.clone();
        assert!(first.begin_quiesce());
        assert!(!second.begin_quiesce());
        assert!(second.ensure_running().is_err());
    }

    #[tokio::test]
    async fn quiesce_waits_for_every_previously_admitted_operation() {
        let runtime = runtime();
        let first = runtime.admit_work().expect("admit first operation");
        let second = runtime.admit_work().expect("admit second operation");
        assert!(runtime.begin_quiesce());
        assert!(runtime.admit_work().is_err());

        let waiter = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.wait_for_work_drained().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(first);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(second);
        waiter.await.expect("work drain task");
    }
}
