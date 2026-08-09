use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use llama_native_engine::GenerationTicket;
use llama_native_host::{HostCachePolicy, NativeHost, NativeHostConfig};
use llama_native_types::{
    GenerationBatchRequest, GenerationEvent, GenerationOutput, NativeError, NativeErrorCode,
};

use crate::model::{LocalModelProfile, RuntimeModelInspection, native_missing_fingerprint};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEvidenceClass {
    RealNative,
    TestFixture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRelease {
    AlreadyAbsent,
    Released { proof: CompleteModelRelease },
}

/// Proof that every native slot matched by one model release was removed.
///
/// The count is stored once so a receipt cannot represent different matched
/// and released counts. Construction remains inside this crate and additionally
/// requires a post-release host snapshot with no matching survivors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompleteModelRelease {
    matched_and_released_slots: NonZeroUsize,
}

impl CompleteModelRelease {
    pub(crate) const fn from_complete_count(slots: NonZeroUsize) -> Self {
        Self {
            matched_and_released_slots: slots,
        }
    }

    #[must_use]
    pub const fn matched_slots(self) -> NonZeroUsize {
        self.matched_and_released_slots
    }

    #[must_use]
    pub const fn released_slots(self) -> NonZeroUsize {
        self.matched_and_released_slots
    }
}

/// Proof that a runtime-owned host was observed empty after releasing every
/// resident slot known to that runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostShutdownReceipt {
    matched_and_released_slots: usize,
}

impl HostShutdownReceipt {
    const fn from_complete_count(slots: usize) -> Self {
        Self {
            matched_and_released_slots: slots,
        }
    }

    #[must_use]
    pub const fn matched_slots(self) -> usize {
        self.matched_and_released_slots
    }

    #[must_use]
    pub const fn released_slots(self) -> usize {
        self.matched_and_released_slots
    }
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

    /// Releases resident native slots associated with a profile.
    ///
    /// `AlreadyAbsent` is distinct from a proved release so callers that own
    /// a loaded-model authority cannot silently treat missing native state as
    /// successful teardown.
    fn release_model(&self, _profile: &LocalModelProfile) -> Result<ModelRelease, NativeError> {
        Ok(ModelRelease::AlreadyAbsent)
    }

    /// Releases every runtime-owned native slot and proves the host empty.
    ///
    /// Runtimes without a typed ownership and verification boundary must fail
    /// closed instead of returning an empty receipt.
    fn shutdown_and_verify_empty(&self) -> Result<HostShutdownReceipt, NativeError> {
        Err(NativeError::new(
            NativeErrorCode::Internal,
            "this batch runtime cannot prove that its native host is empty",
        ))
    }
}

#[derive(Debug, Default)]
struct ResidencyLedger {
    // c616 `slots()` and `unload()` erase host-lock poisoning into empty/false.
    // Holding this mutex across every host acquire, snapshot, and release gives
    // us an independent ownership fact and makes any in-runtime panic poison
    // this boundary too. Do not add a residency-mutating host call outside it.
    model_paths: BTreeSet<PathBuf>,
}

#[derive(Debug)]
pub struct NativeHostRuntime {
    host: NativeHost,
    residency: Mutex<ResidencyLedger>,
}

impl NativeHostRuntime {
    #[must_use]
    pub fn new(config: NativeHostConfig) -> Self {
        Self {
            host: NativeHost::new(config),
            residency: Mutex::new(ResidencyLedger::default()),
        }
    }

    fn lock_residency(&self) -> Result<MutexGuard<'_, ResidencyLedger>, NativeError> {
        self.residency.lock().map_err(|_| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native host residency ownership is poisoned; refusing to infer teardown",
            )
        })
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
        let mut residency = self.lock_residency()?;
        let handle = self.host.acquire(profile.as_native_config())?;
        residency.model_paths.insert(profile.model_path.clone());
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
        let mut residency = self.lock_residency()?;
        let handle = self.host.acquire(profile.as_native_config())?;
        residency.model_paths.insert(profile.model_path.clone());
        let ticket = handle.generate_batch(request)?;
        Ok(Arc::new(NativeBatchExecution {
            ticket: Arc::new(ticket),
        }))
    }

    fn release_model(&self, profile: &LocalModelProfile) -> Result<ModelRelease, NativeError> {
        let mut residency = self.lock_residency()?;
        let slot_ids = self
            .host
            .slots()
            .into_iter()
            .filter(|slot| slot.model_path == profile.model_path)
            .map(|slot| slot.slot_id)
            .collect::<Vec<_>>();
        if slot_ids.is_empty() {
            if residency.model_paths.contains(&profile.model_path) {
                return Err(unobservable_residency_error(
                    "a tracked model had no observable native slot",
                ));
            }
            return Ok(ModelRelease::AlreadyAbsent);
        }

        residency.model_paths.insert(profile.model_path.clone());
        let matched = slot_ids.len();
        let mut released = 0usize;
        for slot_id in &slot_ids {
            released = released.saturating_add(usize::from(self.host.unload(*slot_id)));
        }
        let survivors = self
            .host
            .slots()
            .into_iter()
            .filter(|slot| slot.model_path == profile.model_path)
            .count();
        let proof = prove_complete_model_release(matched, released, survivors)?;
        residency.model_paths.remove(&profile.model_path);
        Ok(ModelRelease::Released { proof })
    }

    fn shutdown_and_verify_empty(&self) -> Result<HostShutdownReceipt, NativeError> {
        let mut residency = self.lock_residency()?;
        let slots = self.host.slots();
        if slots.is_empty() && !residency.model_paths.is_empty() {
            return Err(unobservable_residency_error(
                "tracked models remained while the native host reported no slots",
            ));
        }
        let observed_paths = slots
            .iter()
            .map(|slot| slot.model_path.clone())
            .collect::<BTreeSet<_>>();
        if !residency.model_paths.is_subset(&observed_paths) {
            return Err(unobservable_residency_error(
                "the native slot snapshot omitted tracked model ownership",
            ));
        }

        let matched = slots.len();
        let mut released = 0usize;
        for slot in &slots {
            released = released.saturating_add(usize::from(self.host.unload(slot.slot_id)));
        }
        let survivors = self.host.slots().len();
        let receipt = prove_empty_host_shutdown(matched, released, survivors)?;
        residency.model_paths.clear();
        Ok(receipt)
    }
}

fn prove_complete_model_release(
    matched: usize,
    released: usize,
    survivors: usize,
) -> Result<CompleteModelRelease, NativeError> {
    let Some(matched) = NonZeroUsize::new(matched) else {
        return Err(incomplete_release_error(matched, released, survivors));
    };
    if released != matched.get() || survivors != 0 {
        return Err(incomplete_release_error(matched.get(), released, survivors));
    }
    Ok(CompleteModelRelease::from_complete_count(matched))
}

fn prove_empty_host_shutdown(
    matched: usize,
    released: usize,
    survivors: usize,
) -> Result<HostShutdownReceipt, NativeError> {
    if released != matched || survivors != 0 {
        return Err(incomplete_release_error(matched, released, survivors));
    }
    Ok(HostShutdownReceipt::from_complete_count(matched))
}

fn incomplete_release_error(matched: usize, released: usize, survivors: usize) -> NativeError {
    NativeError::new(
        NativeErrorCode::Internal,
        format!(
            "native teardown was incomplete: matched {matched} slot(s), released {released}, observed {survivors} survivor(s)"
        ),
    )
}

fn unobservable_residency_error(context: &str) -> NativeError {
    NativeError::new(
        NativeErrorCode::Internal,
        format!(
            "{context}; the pinned native host API cannot distinguish an empty registry from poisoned state"
        ),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_release_proof_requires_every_match_and_no_survivors() {
        let proof = prove_complete_model_release(2, 2, 0).expect("complete release");
        assert_eq!(proof.matched_slots().get(), 2);
        assert_eq!(proof.released_slots().get(), 2);

        assert!(prove_complete_model_release(2, 1, 1).is_err());
        assert!(prove_complete_model_release(2, 2, 1).is_err());
        assert!(prove_complete_model_release(0, 0, 0).is_err());
    }

    #[test]
    fn host_shutdown_proof_accepts_empty_and_rejects_partial_release() {
        let empty = prove_empty_host_shutdown(0, 0, 0).expect("already empty host");
        assert_eq!(empty.matched_slots(), 0);
        assert_eq!(empty.released_slots(), 0);

        let released = prove_empty_host_shutdown(3, 3, 0).expect("complete host release");
        assert_eq!(released.matched_slots(), 3);
        assert_eq!(released.released_slots(), 3);

        assert!(prove_empty_host_shutdown(3, 2, 1).is_err());
        assert!(prove_empty_host_shutdown(3, 3, 1).is_err());
    }

    #[test]
    fn empty_native_runtime_returns_a_zero_residency_receipt() {
        let runtime = NativeHostRuntime::default();
        let receipt = runtime
            .shutdown_and_verify_empty()
            .expect("fresh runtime is provably empty");
        assert_eq!(receipt.matched_slots(), 0);
        assert_eq!(receipt.released_slots(), 0);
    }

    #[test]
    fn tracked_ownership_never_accepts_an_empty_untyped_host_snapshot() {
        let runtime = NativeHostRuntime::default();
        let profile = LocalModelProfile::for_gguf("tracked-but-unobservable.gguf");
        runtime
            .residency
            .lock()
            .expect("residency ledger")
            .model_paths
            .insert(profile.model_path.clone());

        assert!(runtime.release_model(&profile).is_err());
        assert!(runtime.shutdown_and_verify_empty().is_err());
    }
}
