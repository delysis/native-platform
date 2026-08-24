use crate::operation_registry::ActiveRequest;
use llama_native_types::{NativeError, NativeErrorCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const SPECULATIVE_PREEMPTION_LIMIT: Duration = Duration::from_millis(200);

type NativeResult<T> = Result<T, NativeError>;

/// The only public observation of the resident worker's speculative lane.
///
/// This deliberately reports no timings, queue depths, or scheduler state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeculativeAdmissionStatus {
    Available,
    Occupied,
    DisabledForResidentFingerprint,
    Closing,
}

pub(crate) trait AdmissionClock: Send + Sync {
    fn now(&self) -> Duration;
}

#[derive(Debug)]
pub(crate) struct SystemAdmissionClock {
    epoch: Instant,
}

impl Default for SystemAdmissionClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl AdmissionClock for SystemAdmissionClock {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }
}

struct PreemptionWatch {
    foreground_sequence: u64,
    cancelled_at: Duration,
}

struct AdmittedSpeculative {
    sequence: u64,
    control: Arc<ActiveRequest>,
}

#[derive(Default)]
struct AdmissionState {
    disabled: bool,
    admitted: Option<AdmittedSpeculative>,
    preemption: Option<PreemptionWatch>,
}

pub(crate) struct SpeculativeAdmission {
    clock: Arc<dyn AdmissionClock>,
    state: Mutex<AdmissionState>,
}

impl std::fmt::Debug for SpeculativeAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpeculativeAdmission")
            .finish_non_exhaustive()
    }
}

impl SpeculativeAdmission {
    pub(crate) fn new(clock: Arc<dyn AdmissionClock>) -> Self {
        Self {
            clock,
            state: Mutex::new(AdmissionState::default()),
        }
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        control: Arc<ActiveRequest>,
    ) -> NativeResult<SpeculativePermit> {
        let sequence = control.identity().sequence;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disabled {
            return Err(NativeError::new(
                NativeErrorCode::QueueFull,
                "speculative generation is disabled for this resident model/runtime fingerprint",
            ));
        }
        if state.admitted.is_some() || state.preemption.is_some() {
            return Err(NativeError::new(
                NativeErrorCode::QueueFull,
                "this resident worker already owns its one admitted speculative generation",
            ));
        }
        state.admitted = Some(AdmittedSpeculative { sequence, control });
        Ok(SpeculativePermit {
            admission: Arc::clone(self),
            sequence,
        })
    }

    /// Cancel the exact admitted speculative request and bind the first
    /// foreground request responsible for measuring readmission latency.
    pub(crate) fn preempt_for_foreground(&self, foreground_sequence: u64) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.preemption.is_some() {
            return 0;
        }
        let Some(control) = state
            .admitted
            .as_ref()
            .map(|admitted| Arc::clone(&admitted.control))
        else {
            return 0;
        };
        let cancelled_at = self.clock.now();
        let cancelled = control.cancel_all();
        state.preemption = Some(PreemptionWatch {
            foreground_sequence,
            cancelled_at,
        });
        cancelled
    }

    /// Called only after the executor has transitioned this exact foreground
    /// request to Running.
    pub(crate) fn observe_foreground_running(&self, foreground_sequence: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(preemption) = state.preemption.as_ref() else {
            return;
        };
        if preemption.foreground_sequence != foreground_sequence {
            return;
        }
        let elapsed = self.clock.now().saturating_sub(preemption.cancelled_at);
        state.preemption = None;
        if elapsed > SPECULATIVE_PREEMPTION_LIMIT {
            state.disabled = true;
        }
    }

    pub(crate) fn status(&self, closing: bool) -> SpeculativeAdmissionStatus {
        if closing {
            return SpeculativeAdmissionStatus::Closing;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disabled {
            SpeculativeAdmissionStatus::DisabledForResidentFingerprint
        } else if state.admitted.is_some() || state.preemption.is_some() {
            SpeculativeAdmissionStatus::Occupied
        } else {
            SpeculativeAdmissionStatus::Available
        }
    }

    fn release(&self, sequence: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .admitted
            .as_ref()
            .is_some_and(|admitted| admitted.sequence == sequence)
        {
            state.admitted = None;
        }
    }
}

#[derive(Debug)]
pub(crate) struct SpeculativePermit {
    admission: Arc<SpeculativeAdmission>,
    sequence: u64,
}

impl Drop for SpeculativePermit {
    fn drop(&mut self) {
        self.admission.release(self.sequence);
    }
}
