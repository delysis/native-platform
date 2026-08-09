use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use llama_native_engine::{GenerationTicket, NativeModelHandle};
use llama_native_host::{
    HostCachePolicy, HostSlotShutdown, JoinedHostSlot, NativeHost, NativeHostConfig,
};
use llama_native_types::{
    GenerationBatchRequest, GenerationEvent, GenerationOutput, NativeError, NativeErrorCode,
};

use crate::model::{LocalModelProfile, RuntimeModelInspection, native_missing_fingerprint};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEvidenceClass {
    RealNative,
    TestFixture,
}

pub trait BatchExecution: std::fmt::Debug + Send + Sync + 'static {
    fn cancel_case(&self, case_id: &str) -> bool;
    fn receive_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<GenerationEvent>, NativeError>;
    fn try_result(&self) -> Result<Option<Vec<GenerationOutput>>, NativeError>;
}

pub trait BatchRuntime: std::fmt::Debug + Send + Sync + 'static {
    fn evidence_class(&self) -> RuntimeEvidenceClass;
    fn inspect_model(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<RuntimeModelInspection, NativeError>;
    fn start_batch(
        &self,
        profile: &LocalModelProfile,
        request: GenerationBatchRequest,
    ) -> Result<Arc<dyn BatchExecution>, NativeError>;

    /// Releases resident native slots associated with a profile. Test and
    /// remote runtimes without resident model state may report `false`.
    fn release_model(&self, _profile: &LocalModelProfile) -> Result<bool, NativeError> {
        Ok(false)
    }
}

#[derive(Debug)]
pub struct NativeHostRuntime {
    host: NativeHost,
}

impl NativeHostRuntime {
    #[must_use]
    pub fn new(config: NativeHostConfig) -> Self {
        Self {
            host: NativeHost::new(config),
        }
    }

    /// Acquire the revocable command client for headless research inference.
    /// The host retains unique worker ownership; callers cannot detach or
    /// manufacture a worker join token from this handle.
    pub fn acquire_research_handle(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<NativeModelHandle, NativeError> {
        self.host.acquire(profile.as_native_config())
    }

    /// Join the exact resident addressed by `handle` after a campaign has
    /// retained all per-call lineage witnesses. This is lifecycle evidence,
    /// not a prerequisite for individual call authorship admission.
    pub fn shutdown_research_handle_joined(
        &self,
        handle: &NativeModelHandle,
    ) -> Result<JoinedHostSlot, NativeError> {
        let slot_id = self
            .host
            .slots()
            .into_iter()
            .find_map(|slot| {
                self.host
                    .handle(slot.slot_id)
                    .filter(|resident| resident.is_same_worker(handle))
                    .map(|_| slot.slot_id)
            })
            .ok_or_else(|| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "research handle does not belong to a live slot in this host",
                )
            })?;
        match self.host.shutdown_slot_joined(slot_id)? {
            HostSlotShutdown::Joined(joined) => Ok(joined),
            HostSlotShutdown::Vacant => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "research resident became vacant before joined shutdown",
            )),
        }
    }
}

impl Default for NativeHostRuntime {
    fn default() -> Self {
        Self::new(NativeHostConfig {
            cache_policy: HostCachePolicy::MemoryOnly,
            ..NativeHostConfig::default()
        })
    }
}

impl BatchRuntime for NativeHostRuntime {
    fn evidence_class(&self) -> RuntimeEvidenceClass {
        RuntimeEvidenceClass::RealNative
    }

    fn inspect_model(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<RuntimeModelInspection, NativeError> {
        let handle = self.host.acquire(profile.as_native_config())?;
        let status = handle.status();
        let live_model_path = status.model_path;
        Ok(RuntimeModelInspection {
            live_model_path,
            descriptor: status.descriptor.ok_or_else(|| {
                NativeError::new(
                    NativeErrorCode::Internal,
                    "loaded model did not expose its descriptor",
                )
            })?,
            fingerprint: status.fingerprint.ok_or_else(native_missing_fingerprint)?,
        })
    }

    fn start_batch(
        &self,
        profile: &LocalModelProfile,
        request: GenerationBatchRequest,
    ) -> Result<Arc<dyn BatchExecution>, NativeError> {
        let ticket = self
            .host
            .generate_batch(profile.as_native_config(), request)?;
        Ok(Arc::new(NativeBatchExecution {
            ticket: Arc::new(ticket),
        }))
    }

    fn release_model(&self, profile: &LocalModelProfile) -> Result<bool, NativeError> {
        let slot_ids = self
            .host
            .slots()
            .into_iter()
            .filter(|slot| slot.model_path == profile.model_path)
            .map(|slot| slot.slot_id)
            .collect::<Vec<_>>();
        let mut released = false;
        for slot_id in slot_ids {
            released |= self.host.unload(slot_id);
        }
        Ok(released)
    }
}

#[derive(Debug)]
struct NativeBatchExecution {
    ticket: Arc<GenerationTicket>,
}

impl BatchExecution for NativeBatchExecution {
    fn cancel_case(&self, case_id: &str) -> bool {
        self.ticket.cancel_branch(case_id)
    }

    fn receive_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<GenerationEvent>, NativeError> {
        match self.ticket.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn try_result(&self) -> Result<Option<Vec<GenerationOutput>>, NativeError> {
        self.ticket.try_wait()
    }
}
