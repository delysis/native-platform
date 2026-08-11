use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use llama_native_engine::{GenerationTicket, NativeModelHandle};
use llama_native_host::{
    HostCachePolicy, HostSlotShutdown, JoinedHostSlot, JoinedNativeHost, NativeHost,
    NativeHostConfig, ProcessExitJoinedNativeHost,
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

#[derive(Debug)]
pub enum ModelRelease {
    AlreadyAbsent,
    Released { proof: CompleteModelRelease },
}

/// Proof that every native slot matched by one model release was removed.
///
/// The count is stored once so a receipt cannot represent different matched
/// and released counts. Construction remains inside this crate and additionally
/// requires a post-release host snapshot with no matching survivors.
#[derive(Debug)]
pub struct CompleteModelRelease {
    evidence: CompleteModelReleaseEvidence,
}

#[derive(Debug)]
enum CompleteModelReleaseEvidence {
    Native(Vec<JoinedHostSlot>),
    #[cfg(test)]
    Fixture(NonZeroUsize),
}

impl CompleteModelRelease {
    #[cfg(test)]
    pub(crate) const fn from_complete_count(slots: NonZeroUsize) -> Self {
        Self {
            evidence: CompleteModelReleaseEvidence::Fixture(slots),
        }
    }

    #[must_use]
    pub fn matched_slots(&self) -> NonZeroUsize {
        self.released_slots()
    }

    #[must_use]
    pub fn released_slots(&self) -> NonZeroUsize {
        match &self.evidence {
            CompleteModelReleaseEvidence::Native(slots) => NonZeroUsize::new(slots.len())
                .expect("native release evidence is constructed from a non-empty slot family"),
            #[cfg(test)]
            CompleteModelReleaseEvidence::Fixture(slots) => *slots,
        }
    }
}

/// Linear evidence that one exact native runtime permanently closed model
/// admission and joined every resident worker.
///
/// The native proof is deliberately private and this type is neither `Clone`
/// nor `Copy`. Code using an erased [`BatchRuntime`] therefore cannot construct
/// or delegate shutdown authority. This proves native model workers only;
/// application-owned event-forwarder and download workers require their own
/// joined evidence.
#[derive(Debug)]
#[must_use = "joined native-runtime authority must be consumed by application shutdown"]
pub struct JoinedLlamaRuntime {
    native: JoinedNativeHost,
}

impl JoinedLlamaRuntime {
    #[must_use]
    pub const fn joined_worker_count(&self) -> usize {
        self.native.joined_worker_count()
    }

    /// Returns true only for the concrete runtime instance that joined.
    #[must_use]
    pub fn belongs_to(&self, runtime: &NativeHostRuntime) -> bool {
        self.native.belongs_to(&runtime.host)
    }
}

/// Replayable terminal fact for an exact native runtime after its
/// poison-recovering process-exit drain joined every resident worker.
#[derive(Debug)]
#[must_use = "process teardown must retain the joined native-runtime drain fact"]
pub struct ProcessExitJoinedLlamaRuntime {
    native: ProcessExitJoinedNativeHost,
}

impl ProcessExitJoinedLlamaRuntime {
    /// Number of native owners consumed by this drain invocation.
    #[must_use]
    pub const fn joined_worker_count(&self) -> usize {
        self.native.joined_worker_count()
    }

    /// Returns true only for the concrete runtime instance that drained.
    #[must_use]
    pub fn belongs_to(&self, runtime: &NativeHostRuntime) -> bool {
        self.native.belongs_to(&runtime.host)
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
}

#[derive(Debug, Default)]
struct ResidencyLedger {
    // The compatibility `slots()` and `unload()` APIs erase host-lock failures
    // into empty/false. Holding this mutex across every host acquire, snapshot,
    // release, and shutdown gives us an independent ownership fact and makes
    // any in-runtime panic poison this boundary too. Do not add a
    // residency-mutating host call outside it.
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

    /// Acquire the revocable command client for headless research inference.
    /// The host retains unique worker ownership; callers cannot detach or
    /// manufacture a worker join token from this handle.
    pub fn acquire_research_handle(
        &self,
        profile: &LocalModelProfile,
    ) -> Result<NativeModelHandle, NativeError> {
        let mut residency = self.lock_residency()?;
        let handle = self.host.acquire(profile.as_native_config())?;
        residency.model_paths.insert(profile.model_path.clone());
        Ok(handle)
    }

    /// Join the exact resident addressed by `handle` after a campaign has
    /// retained all per-call lineage witnesses. This is lifecycle evidence,
    /// not a prerequisite for individual call authorship admission.
    pub fn shutdown_research_handle_joined(
        &self,
        handle: &NativeModelHandle,
    ) -> Result<JoinedHostSlot, NativeError> {
        let mut residency = self.lock_residency()?;
        let (slot_id, model_path) = self
            .host
            .slots()
            .into_iter()
            .find_map(|slot| {
                self.host
                    .handle(slot.slot_id)
                    .filter(|resident| resident.is_same_worker(handle))
                    .map(|_| (slot.slot_id, slot.model_path))
            })
            .ok_or_else(|| {
                NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "research handle does not belong to a live slot in this host",
                )
            })?;
        match self.host.shutdown_slot_joined(slot_id)? {
            HostSlotShutdown::Joined(joined) if joined.belongs_to(&self.host) => {
                let has_survivor = self
                    .host
                    .slots()
                    .iter()
                    .any(|slot| slot.model_path == model_path);
                if !has_survivor {
                    residency.model_paths.remove(&model_path);
                }
                Ok(joined)
            }
            HostSlotShutdown::Joined(_) => Err(NativeError::new(
                NativeErrorCode::Internal,
                "native host returned joined evidence for a different host instance",
            )),
            HostSlotShutdown::Vacant => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "research resident became vacant before joined shutdown",
            )),
        }
    }

    /// Permanently closes this exact host and returns only after every native
    /// model worker owned by it has been joined.
    pub fn shutdown_joined(&self) -> Result<JoinedLlamaRuntime, NativeError> {
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

        let native = self.host.shutdown_joined()?;
        if !native.belongs_to(&self.host) {
            return Err(NativeError::new(
                NativeErrorCode::Internal,
                "native host returned shutdown authority for a different host instance",
            ));
        }
        residency.model_paths.clear();
        Ok(JoinedLlamaRuntime { native })
    }

    /// Final process-exit drain for the exact native host.
    ///
    /// This waits for any in-flight residency operation, recovers a poisoned
    /// residency ledger, permanently closes host admission, and cannot return
    /// before every host-owned model worker has been joined.
    pub fn shutdown_for_process_exit(&self) -> ProcessExitJoinedLlamaRuntime {
        let mut residency = self
            .residency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let native = self.host.shutdown_for_process_exit();
        residency.model_paths.clear();
        ProcessExitJoinedLlamaRuntime { native }
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
        let mut joined_slots = Vec::with_capacity(matched);
        for slot_id in &slot_ids {
            match self.host.shutdown_slot_joined(*slot_id)? {
                HostSlotShutdown::Joined(joined) if joined.belongs_to(&self.host) => {
                    joined_slots.push(joined);
                }
                HostSlotShutdown::Joined(_) | HostSlotShutdown::Vacant => {
                    return Err(incomplete_release_error(
                        matched,
                        joined_slots.len(),
                        self.host.slots().len(),
                    ));
                }
            }
        }
        let survivors = self
            .host
            .slots()
            .into_iter()
            .filter(|slot| slot.model_path == profile.model_path)
            .count();
        let proof = prove_complete_model_release(matched, joined_slots, survivors)?;
        residency.model_paths.remove(&profile.model_path);
        Ok(ModelRelease::Released { proof })
    }
}

fn prove_complete_model_release(
    matched: usize,
    joined_slots: Vec<JoinedHostSlot>,
    survivors: usize,
) -> Result<CompleteModelRelease, NativeError> {
    let Some(matched_nonzero) = NonZeroUsize::new(matched) else {
        return Err(incomplete_release_error(
            matched,
            joined_slots.len(),
            survivors,
        ));
    };
    if joined_slots.len() != matched_nonzero.get() || survivors != 0 {
        return Err(incomplete_release_error(
            matched_nonzero.get(),
            joined_slots.len(),
            survivors,
        ));
    }
    Ok(CompleteModelRelease {
        evidence: CompleteModelReleaseEvidence::Native(joined_slots),
    })
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
    fn fixture_release_proof_is_affine_and_has_one_canonical_count() {
        let proof = CompleteModelRelease::from_complete_count(
            NonZeroUsize::new(2).expect("non-zero fixture count"),
        );
        assert_eq!(proof.matched_slots().get(), 2);
        assert_eq!(proof.released_slots().get(), 2);
    }

    #[test]
    fn empty_native_runtime_returns_exact_joined_authority() {
        let runtime = NativeHostRuntime::default();
        let other_runtime = NativeHostRuntime::default();
        let joined = runtime.shutdown_joined().expect("fresh runtime joins");
        assert_eq!(joined.joined_worker_count(), 0);
        assert!(joined.belongs_to(&runtime));
        assert!(!joined.belongs_to(&other_runtime));
        let _other_joined = other_runtime
            .shutdown_joined()
            .expect("other runtime joins");
    }

    #[test]
    fn process_exit_runtime_recovers_poison_and_returns_exact_drain_fact() {
        let runtime = NativeHostRuntime::default();
        let other_runtime = NativeHostRuntime::default();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = runtime.residency.lock().expect("residency ledger");
            panic!("poison residency ledger");
        }));

        let joined = runtime.shutdown_for_process_exit();
        assert_eq!(joined.joined_worker_count(), 0);
        assert!(joined.belongs_to(&runtime));
        assert!(!joined.belongs_to(&other_runtime));
        let joined_again = runtime.shutdown_for_process_exit();
        assert_eq!(joined_again.joined_worker_count(), 0);
        assert!(joined_again.belongs_to(&runtime));
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
        assert!(runtime.shutdown_joined().is_err());
    }
}
