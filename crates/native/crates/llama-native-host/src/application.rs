//! One application owner with request-only product clients.
//! Storage and startup policy are supplied by the composition root. An unused
//! owner never constructs a native host, so editing need not unlock a store.

use crate::{
    HostSlotShutdown, HostSlotStatus, JoinedHostSlot, JoinedNativeHost, NativeHost,
    ProcessExitJoinedNativeHost,
};
use llama_native_engine::NativeModelHandle;
use llama_native_types::{NativeError, NativeErrorCode, NativeModelConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug)]
pub struct ApplicationHost {
    pub host: Arc<NativeHost>,
    /// Immutable product configuration/storage binding chosen at construction.
    pub binding: String,
    pub memory_budget_bytes: u64,
}

type HostFactory = dyn Fn() -> Result<ApplicationHost, NativeError> + Send + Sync;

struct State {
    host: Option<ApplicationHost>,
    closed: bool,
    next_client: u64,
    claims: BTreeMap<u64, BTreeSet<usize>>,
}

struct Shared {
    factory: Box<HostFactory>,
    state: Mutex<State>,
}

/// Sole construction and host-wide finalization capability for an application.
pub struct ApplicationNativeOwner {
    shared: Arc<Shared>,
}

/// Composition-root hook: product work drains before the sole native join.
pub trait ApplicationNativeFinalizer: std::fmt::Debug + Send + Sync {
    fn shutdown_joined(&self) -> Result<JoinedApplicationNative, NativeError>;
    fn shutdown_for_process_exit(&self) -> ProcessExitJoinedApplicationNative;
}

impl std::fmt::Debug for ApplicationNativeOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationNativeOwner")
            .finish_non_exhaustive()
    }
}

impl ApplicationNativeOwner {
    #[must_use]
    pub fn new(
        factory: impl Fn() -> Result<ApplicationHost, NativeError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                factory: Box::new(factory),
                state: Mutex::new(State {
                    host: None,
                    closed: false,
                    next_client: 0,
                    claims: BTreeMap::new(),
                }),
            }),
        }
    }

    pub fn client(&self) -> Result<NativeClient, NativeError> {
        let mut state = self.shared.state.lock().map_err(poisoned)?;
        ensure_open(&state)?;
        if state.claims.len() >= 8 {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                "application native client limit reached",
            ));
        }
        let id = state.next_client.checked_add(1).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native client identity exhausted",
            )
        })?;
        state.next_client = id;
        state.claims.insert(id, BTreeSet::new());
        Ok(NativeClient {
            shared: Arc::downgrade(&self.shared),
            id,
        })
    }

    pub fn shutdown_joined(&self) -> Result<JoinedApplicationNative, NativeError> {
        let mut state = self.shared.state.lock().map_err(poisoned)?;
        ensure_open(&state)?;
        state.closed = true;
        let native = state
            .host
            .as_ref()
            .map(|host| host.host.shutdown_joined())
            .transpose()?;
        state.claims.values_mut().for_each(BTreeSet::clear);
        Ok(JoinedApplicationNative {
            shared: Arc::clone(&self.shared),
            native,
        })
    }

    #[must_use]
    pub fn shutdown_for_process_exit(&self) -> ProcessExitJoinedApplicationNative {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        let native = state
            .host
            .as_ref()
            .map(|host| host.host.shutdown_for_process_exit());
        state.claims.values_mut().for_each(BTreeSet::clear);
        ProcessExitJoinedApplicationNative {
            shared: Arc::clone(&self.shared),
            native,
        }
    }
}

impl Drop for ApplicationNativeOwner {
    fn drop(&mut self) {
        let _joined = self.shutdown_for_process_exit();
    }
}

/// Product request capability. It cannot construct or finalize a native host.
/// Clones retain the same client's residency claims.
#[derive(Clone)]
pub struct NativeClient {
    shared: Weak<Shared>,
    id: u64,
}

impl std::fmt::Debug for NativeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeClient")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl NativeClient {
    fn with_state<T>(
        &self,
        initialize: bool,
        operation: impl FnOnce(&mut State) -> Result<T, NativeError>,
    ) -> Result<T, NativeError> {
        let shared = self.shared.upgrade().ok_or_else(stopped)?;
        let mut state = shared.state.lock().map_err(poisoned)?;
        ensure_open(&state)?;
        if initialize && state.host.is_none() {
            state.host = Some((shared.factory)()?);
        }
        operation(&mut state)
    }

    pub fn binding(&self) -> Result<String, NativeError> {
        self.with_state(true, |state| Ok(initialized(state)?.binding.clone()))
    }

    pub fn memory_budget_bytes(&self) -> Result<u64, NativeError> {
        self.with_state(true, |state| Ok(initialized(state)?.memory_budget_bytes))
    }

    pub fn acquire(&self, config: NativeModelConfig) -> Result<NativeModelHandle, NativeError> {
        self.with_state(true, |state| {
            let handle = initialized(state)?.host.acquire(config)?;
            claim_handle(state, self.id, &handle)?;
            Ok(handle)
        })
    }

    pub fn resident(
        &self,
        config: &NativeModelConfig,
    ) -> Result<Option<NativeModelHandle>, NativeError> {
        self.with_state(false, |state| {
            let handle = state
                .host
                .as_ref()
                .map(|host| host.host.resident(config))
                .transpose()?
                .flatten();
            if let Some(handle) = &handle {
                claim_handle(state, self.id, handle)?;
            }
            Ok(handle)
        })
    }

    pub fn handle(&self, slot: usize) -> Result<Option<NativeModelHandle>, NativeError> {
        self.with_state(false, |state| {
            let handle = state
                .host
                .as_ref()
                .map(|host| host.host.try_handle(slot))
                .transpose()?
                .flatten();
            if let Some(handle) = &handle {
                claim_handle(state, self.id, handle)?;
            }
            Ok(handle)
        })
    }

    /// Observe only slots already claimed by this client. Looking up a worker
    /// must not create residency claims on another product's workers.
    pub fn slot_for_handle(
        &self,
        handle: &NativeModelHandle,
    ) -> Result<Option<HostSlotStatus>, NativeError> {
        self.with_state(false, |state| {
            let Some(host) = &state.host else {
                return Ok(None);
            };
            let claims = state.claims.get(&self.id).ok_or_else(stopped)?;
            for slot in host.host.try_slots()? {
                if claims.contains(&slot.slot_id)
                    && host
                        .host
                        .try_handle(slot.slot_id)?
                        .is_some_and(|resident| resident.is_same_worker(handle))
                {
                    return Ok(Some(slot));
                }
            }
            Ok(None)
        })
    }

    pub fn load_into_slot(
        &self,
        slot: usize,
        config: NativeModelConfig,
    ) -> Result<NativeModelHandle, NativeError> {
        self.with_state(true, |state| {
            reject_foreign_claims(state, self.id, &[slot])?;
            let handle = initialized(state)?.host.load_into_slot(slot, config)?;
            claim_handle(state, self.id, &handle)?;
            Ok(handle)
        })
    }

    pub fn slots(&self) -> Result<Vec<HostSlotStatus>, NativeError> {
        self.with_state(false, |state| {
            state
                .host
                .as_ref()
                .map_or_else(|| Ok(Vec::new()), |host| host.host.try_slots())
        })
    }

    pub fn claimed_slots(&self) -> Result<Vec<usize>, NativeError> {
        self.with_state(false, |state| {
            Ok(state
                .claims
                .get(&self.id)
                .ok_or_else(stopped)?
                .iter()
                .copied()
                .collect())
        })
    }

    #[must_use]
    pub fn owns_joined_slot(&self, proof: &JoinedHostSlot) -> bool {
        self.with_state(false, |state| {
            Ok(state
                .host
                .as_ref()
                .is_some_and(|host| proof.belongs_to(&host.host)))
        })
        .unwrap_or(false)
    }

    /// Checks the whole requested family before any unload. Another product's
    /// claim is an explicit conflict, never release evidence.
    pub fn release_slots(&self, slots: &[usize]) -> Result<Vec<JoinedHostSlot>, NativeError> {
        self.with_state(false, |state| {
            reject_foreign_claims(state, self.id, slots)?;
            let requested = slots.iter().copied().collect::<BTreeSet<_>>();
            let claims = state.claims.get(&self.id).ok_or_else(stopped)?;
            if requested.len() != slots.len() || !requested.is_subset(claims) {
                return Err(NativeError::new(
                    NativeErrorCode::InvalidConfig,
                    "slot release requires unique slots claimed by this client",
                ));
            }
            if !slots.is_empty() {
                let resident = initialized(state)?
                    .host
                    .try_slots()?
                    .into_iter()
                    .map(|slot| slot.slot_id)
                    .collect::<BTreeSet<_>>();
                if !requested.is_subset(&resident) {
                    return Err(NativeError::new(
                        NativeErrorCode::Internal,
                        "claimed native family changed before release",
                    ));
                }
            }
            let mut joined = Vec::with_capacity(slots.len());
            for slot in slots {
                let host = &initialized(state)?.host;
                match host.shutdown_slot_joined(*slot)? {
                    HostSlotShutdown::Joined(proof) if proof.belongs_to(host) => joined.push(proof),
                    _ => {
                        return Err(NativeError::new(
                            NativeErrorCode::Internal,
                            "claimed native slot disappeared during release",
                        ));
                    }
                }
                for claims in state.claims.values_mut() {
                    claims.remove(slot);
                }
            }
            Ok(joined)
        })
    }

    pub fn cancel(&self, request: &str, branch: Option<&str>) -> usize {
        self.with_state(false, |state| {
            Ok(state
                .host
                .as_ref()
                .map_or(0, |host| host.host.cancel(request, branch)))
        })
        .unwrap_or(0)
    }

    pub fn skip_reasoning(&self, request: &str, branch: Option<&str>) -> usize {
        self.with_state(false, |state| {
            Ok(state
                .host
                .as_ref()
                .map_or(0, |host| host.host.skip_reasoning(request, branch)))
        })
        .unwrap_or(0)
    }

    pub fn clear_cache(&self) -> Result<usize, NativeError> {
        self.with_state(false, |state| {
            state
                .host
                .as_ref()
                .map_or(Ok(0), |host| host.host.clear_cache())
        })
    }

    pub fn invalidate_live_cache_owner(&self, owner: &str) -> Result<usize, NativeError> {
        self.with_state(false, |state| {
            state
                .host
                .as_ref()
                .map_or(Ok(0), |host| host.host.invalidate_live_cache_owner(owner))
        })
    }
}

fn claim_handle(
    state: &mut State,
    client: u64,
    handle: &NativeModelHandle,
) -> Result<(), NativeError> {
    let host = &initialized(state)?.host;
    let mut selected = None;
    for slot in host.try_slots()? {
        if host
            .try_handle(slot.slot_id)?
            .is_some_and(|resident| resident.is_same_worker(handle))
        {
            selected = Some(slot.slot_id);
            break;
        }
    }
    let slot = selected.ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::Internal,
            "acquired native worker has no observable slot",
        )
    })?;
    state
        .claims
        .get_mut(&client)
        .ok_or_else(stopped)?
        .insert(slot);
    Ok(())
}

fn reject_foreign_claims(state: &State, client: u64, slots: &[usize]) -> Result<(), NativeError> {
    if state
        .claims
        .iter()
        .any(|(id, claims)| *id != client && slots.iter().any(|slot| claims.contains(slot)))
    {
        return Err(NativeError::new(
            NativeErrorCode::ModelInUse,
            "native model is retained by another application client",
        ));
    }
    Ok(())
}

fn initialized(state: &State) -> Result<&ApplicationHost, NativeError> {
    state.host.as_ref().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::ModelNotLoaded,
            "application native host has not been initialized",
        )
    })
}

fn ensure_open(state: &State) -> Result<(), NativeError> {
    if state.closed { Err(stopped()) } else { Ok(()) }
}

fn stopped() -> NativeError {
    NativeError::new(
        NativeErrorCode::WorkerStopped,
        "application native admission is closed",
    )
}
fn poisoned<T>(_: std::sync::PoisonError<T>) -> NativeError {
    NativeError::new(
        NativeErrorCode::Internal,
        "application native ownership state is poisoned",
    )
}

pub struct JoinedApplicationNative {
    shared: Arc<Shared>,
    native: Option<JoinedNativeHost>,
}
pub struct ProcessExitJoinedApplicationNative {
    shared: Arc<Shared>,
    native: Option<ProcessExitJoinedNativeHost>,
}

macro_rules! joined_proof {
    ($proof:ty) => {
        impl $proof {
            #[must_use]
            pub fn belongs_to(&self, client: &NativeClient) -> bool {
                Weak::ptr_eq(&Arc::downgrade(&self.shared), &client.shared)
            }
            #[must_use]
            pub fn joined_worker_count(&self) -> usize {
                self.native
                    .as_ref()
                    .map_or(0, |native| native.joined_worker_count())
            }
        }
        impl std::fmt::Debug for $proof {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($proof))
                    .field("joined_worker_count", &self.joined_worker_count())
                    .finish_non_exhaustive()
            }
        }
    };
}
joined_proof!(JoinedApplicationNative);
joined_proof!(ProcessExitJoinedApplicationNative);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeHostConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn clients_initialize_one_host_and_cannot_reopen_after_join() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let owner = ApplicationNativeOwner::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(ApplicationHost {
                host: Arc::new(NativeHost::new(NativeHostConfig::default())),
                binding: "encrypted-store-policy".into(),
                memory_budget_bytes: 1024,
            })
        });
        let document = owner.client().expect("document client");
        let chat = owner.client().expect("chat client");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            document.binding().expect("document host"),
            chat.binding().expect("chat host")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let joined = owner.shutdown_joined().expect("sole joined owner");
        assert!(joined.belongs_to(&document) && joined.belongs_to(&chat));
        assert!(chat.binding().is_err());
        assert!(owner.client().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn foreign_claim_rejects_the_entire_release_before_mutation() {
        let state = State {
            host: None,
            closed: false,
            next_client: 2,
            claims: BTreeMap::from([(1, BTreeSet::from([0, 1])), (2, BTreeSet::from([1]))]),
        };
        assert_eq!(
            reject_foreign_claims(&state, 1, &[0, 1])
                .expect_err("shared slot is busy")
                .code,
            NativeErrorCode::ModelInUse
        );
        assert!(reject_foreign_claims(&state, 1, &[0]).is_ok());
        assert_eq!(state.claims[&1], BTreeSet::from([0, 1]));
    }

    #[test]
    fn unused_owner_closes_without_initializing_storage_or_model() {
        let owner = ApplicationNativeOwner::new(|| panic!("shutdown must not initialize"));
        let client = owner.client().expect("client");
        let joined = owner.shutdown_joined().expect("unused owner joins");
        assert!(joined.belongs_to(&client));
        assert_eq!(joined.joined_worker_count(), 0);
    }
}
