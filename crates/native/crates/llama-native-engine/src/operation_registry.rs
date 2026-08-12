use llama_native_types::{NativeError, NativeErrorCode};
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

type NativeResult<T> = Result<T, NativeError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestClass {
    Generation,
    ControlledGeneration,
    Embedding,
}

#[derive(Debug)]
pub(crate) enum RequestControls {
    Generation {
        cancellations: Vec<(String, Arc<AtomicBool>)>,
        reasoning_forces: Vec<(String, Arc<AtomicBool>)>,
    },
    ControlledGeneration {
        cancellations: Vec<(String, Arc<AtomicBool>)>,
    },
    Embedding {
        cancellation: Arc<AtomicBool>,
    },
}

impl RequestControls {
    fn cancel_all(&self) -> usize {
        match self {
            Self::Generation { cancellations, .. }
            | Self::ControlledGeneration { cancellations } => {
                set_all(cancellations.iter().map(|(_, flag)| flag))
            }
            Self::Embedding { cancellation } => {
                cancellation.store(true, Ordering::Release);
                1
            }
        }
    }

    fn cancel_named(&self, name: &str) -> bool {
        match self {
            Self::Generation { cancellations, .. }
            | Self::ControlledGeneration { cancellations } => set_named(cancellations, name),
            Self::Embedding { .. } => false,
        }
    }

    fn force_reasoning_exit(&self, name: &str) -> bool {
        match self {
            Self::Generation {
                reasoning_forces, ..
            } => set_named(reasoning_forces, name),
            Self::ControlledGeneration { .. } | Self::Embedding { .. } => false,
        }
    }

    fn force_all_reasoning_exits(&self) -> usize {
        match self {
            Self::Generation {
                reasoning_forces, ..
            } => set_all(reasoning_forces.iter().map(|(_, flag)| flag)),
            Self::ControlledGeneration { .. } | Self::Embedding { .. } => 0,
        }
    }
}

fn set_named(flags: &[(String, Arc<AtomicBool>)], name: &str) -> bool {
    flags
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, flag)| {
            flag.store(true, Ordering::Release);
            true
        })
        .unwrap_or(false)
}

fn set_all<'a>(flags: impl Iterator<Item = &'a Arc<AtomicBool>>) -> usize {
    flags
        .map(|flag| {
            flag.store(true, Ordering::Release);
            1_usize
        })
        .sum()
}

#[derive(Debug)]
pub(crate) struct ActiveRequest {
    request_id: String,
    class: RequestClass,
    controls: RequestControls,
    reservation_nonce: u64,
}

impl ActiveRequest {
    #[must_use]
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub(crate) const fn class(&self) -> RequestClass {
        self.class
    }

    pub(crate) fn cancel_all(&self) -> usize {
        self.controls.cancel_all()
    }

    pub(crate) fn cancel_named(&self, name: &str) -> bool {
        self.controls.cancel_named(name)
    }

    pub(crate) fn force_reasoning_exit(&self, name: &str) -> bool {
        self.controls.force_reasoning_exit(name)
    }

    pub(crate) fn force_all_reasoning_exits(&self) -> usize {
        self.controls.force_all_reasoning_exits()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistryPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Debug)]
struct RegistryState {
    phase: RegistryPhase,
    next_nonce: u64,
    active: HashMap<String, Arc<ActiveRequest>>,
}

#[derive(Debug)]
pub(crate) struct RequestRegistry {
    state: Mutex<RegistryState>,
    drained: Condvar,
    #[cfg(test)]
    releases: AtomicU64,
}

impl RequestRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                phase: RegistryPhase::Running,
                next_nonce: 0,
                active: HashMap::new(),
            }),
            drained: Condvar::new(),
            #[cfg(test)]
            releases: AtomicU64::new(0),
        }
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        request_id: impl Into<String>,
        class: RequestClass,
        controls: RequestControls,
    ) -> NativeResult<(Arc<ActiveRequest>, RequestLease)> {
        let request_id = request_id.into();
        let mut state = self.state.lock().map_err(|_| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native request registry is poisoned",
            )
        })?;
        if state.phase != RegistryPhase::Running {
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native request admission is closed",
            ));
        }
        if state.active.contains_key(&request_id) {
            return Err(NativeError::new(
                NativeErrorCode::DuplicateActiveRequest,
                format!("native request ID {request_id:?} is already active"),
            ));
        }
        let reservation_nonce = state.next_nonce;
        state.next_nonce = state.next_nonce.checked_add(1).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native request reservation sequence overflowed",
            )
        })?;
        let entry = Arc::new(ActiveRequest {
            request_id: request_id.clone(),
            class,
            controls,
            reservation_nonce,
        });
        state.active.insert(request_id, Arc::clone(&entry));
        Ok((
            Arc::clone(&entry),
            RequestLease {
                registry: Arc::clone(self),
                entry,
            },
        ))
    }

    pub(crate) fn active(&self, request_id: &str) -> Option<Arc<ActiveRequest>> {
        self.lock_recovering_poison()
            .active
            .get(request_id)
            .cloned()
    }

    pub(crate) fn begin_quiesce_and_cancel_all(&self) {
        let active = {
            let mut state = self.lock_recovering_poison();
            if state.phase == RegistryPhase::Closed {
                return;
            }
            state.phase = RegistryPhase::Quiescing;
            state.active.values().cloned().collect::<Vec<_>>()
        };
        for entry in active {
            entry.cancel_all();
        }
    }

    pub(crate) fn mark_closed(&self) -> NativeResult<()> {
        let mut state = self.lock_recovering_poison();
        if !state.active.is_empty() {
            return Err(NativeError::new(
                NativeErrorCode::Internal,
                format!(
                    "native worker joined with {} active request reservation(s)",
                    state.active.len()
                ),
            ));
        }
        state.phase = RegistryPhase::Closed;
        self.drained.notify_all();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn wait_until_drained(&self) {
        let mut state = self.lock_recovering_poison();
        while !state.active.is_empty() {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    fn contains(&self, request_id: &str) -> bool {
        self.lock_recovering_poison()
            .active
            .contains_key(request_id)
    }

    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.lock_recovering_poison().active.len()
    }

    #[cfg(test)]
    fn release_count(&self) -> u64 {
        self.releases.load(Ordering::Acquire)
    }

    fn release_if_current(&self, entry: &Arc<ActiveRequest>) {
        let mut state = self.lock_recovering_poison();
        let current_matches = state.active.get(entry.request_id()).is_some_and(|current| {
            Arc::ptr_eq(current, entry) && current.reservation_nonce == entry.reservation_nonce
        });
        if current_matches {
            state.active.remove(entry.request_id());
            #[cfg(test)]
            self.releases.fetch_add(1, Ordering::AcqRel);
        }
        if state.active.is_empty() {
            self.drained.notify_all();
        }
    }

    fn lock_recovering_poison(&self) -> MutexGuard<'_, RegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub(crate) struct RequestLease {
    registry: Arc<RequestRegistry>,
    entry: Arc<ActiveRequest>,
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        // Request identity belongs to the executor command. Ticket drop only
        // requests cancellation; terminal publication lets this lease fall.
        self.registry.release_if_current(&self.entry);
    }
}

#[cfg(all(test, feature = "unstable-w1-contract-tests"))]
mod w1_contract_adapter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn controls() -> RequestControls {
        RequestControls::Generation {
            cancellations: vec![("case".to_owned(), Arc::new(AtomicBool::new(false)))],
            reasoning_forces: vec![("case".to_owned(), Arc::new(AtomicBool::new(false)))],
        }
    }

    #[test]
    fn executor_lease_not_ticket_interest_owns_request_identity() {
        let registry = Arc::new(RequestRegistry::new());
        let (ticket_control, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("first request reserves its identity");

        drop(ticket_control);
        assert_eq!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .expect_err("ticket interest cannot release executor identity")
                .code,
            llama_native_types::NativeErrorCode::DuplicateActiveRequest
        );

        drop(lease);
        assert_eq!(registry.release_count(), 1);
        assert!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .is_ok()
        );
    }

    #[test]
    fn stale_lease_cannot_remove_a_newer_reservation() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, first_lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("first request");
        let stale_entry = Arc::clone(&first_lease.entry);
        drop(first_lease);

        let (_, second_lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("second request");
        registry.release_if_current(&stale_entry);

        assert!(registry.contains("request"));
        assert_eq!(registry.release_count(), 1);
        drop(second_lease);
        assert!(!registry.contains("request"));
        assert_eq!(registry.release_count(), 2);
    }

    #[test]
    fn begin_quiesce_rejects_new_reservations_and_cancels_existing() {
        let registry = Arc::new(RequestRegistry::new());
        let cancellation = Arc::new(AtomicBool::new(false));
        let (_, lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&cancellation),
                },
            )
            .expect("request reserves");

        registry.begin_quiesce_and_cancel_all();
        assert!(cancellation.load(Ordering::Acquire));
        assert!(
            registry
                .reserve("other", RequestClass::Generation, controls())
                .is_err()
        );
        drop(lease);
        registry.wait_until_drained();
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn cancellation_routes_only_to_the_current_operation() {
        let registry = Arc::new(RequestRegistry::new());
        let old = Arc::new(AtomicBool::new(false));
        let (_, old_lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&old),
                },
            )
            .expect("old request");
        let stale_control = Arc::clone(&old_lease.entry);
        drop(old_lease);

        let current = Arc::new(AtomicBool::new(false));
        let (_, _current_lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&current),
                },
            )
            .expect("current request");

        stale_control.cancel_all();
        assert!(old.load(Ordering::Acquire));
        assert!(!current.load(Ordering::Acquire));
    }

    #[test]
    fn registry_drain_waits_until_the_executor_lease_drops() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("request reserves");
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(0);
        let waiter_registry = Arc::clone(&registry);
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).expect("test coordinator remains live");
            waiter_registry.wait_until_drained();
            drained_tx.send(()).expect("test coordinator remains live");
        });

        started_rx.recv().expect("drain waiter started");
        assert!(
            drained_rx
                .recv_timeout(std::time::Duration::from_millis(10))
                .is_err(),
            "drain cannot finish while the executor lease remains live"
        );
        drop(lease);
        drained_rx.recv().expect("lease release wakes drain waiter");
        waiter.join().expect("drain waiter joins");
        assert_eq!(registry.release_count(), 1);
    }

    #[test]
    fn closed_registry_requires_zero_executor_leases_and_rejects_reopening() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("request reserves");

        assert_eq!(
            registry
                .mark_closed()
                .expect_err("an active executor lease prevents a joined proof")
                .code,
            NativeErrorCode::Internal
        );
        drop(lease);
        registry.mark_closed().expect("drained registry closes");
        assert_eq!(registry.release_count(), 1);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .expect_err("a closed registry cannot be reopened")
                .code,
            NativeErrorCode::WorkerStopped
        );
    }
}
