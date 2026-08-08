use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use llama_native_engine::GenerationTicket;
use llama_native_host::{NativeHost, NativeHostConfig};
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
}

impl Default for NativeHostRuntime {
    fn default() -> Self {
        Self::new(NativeHostConfig::default())
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
        Ok(RuntimeModelInspection {
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
