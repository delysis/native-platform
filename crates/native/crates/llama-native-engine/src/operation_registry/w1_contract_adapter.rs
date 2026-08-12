use super::*;
use platform_contract_testkit::LifecyclePhase;

/// Test-only view of facts owned by `RequestRegistry` itself.
///
/// This intentionally does not implement `OperationModelAdapter`: the registry
/// does not own queue/running transitions, terminal or progress publication,
/// retained tasks, or worker joins. Supplying those fields here would create a
/// second lifecycle model rather than test the production registry.
#[derive(Clone, Debug)]
struct NativeRegistryContractAdapter {
    registry: Arc<RequestRegistry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeRegistryFacts {
    lifecycle: LifecyclePhase,
    active_operations: usize,
    released_leases: u64,
}

impl NativeRegistryContractAdapter {
    fn new() -> Self {
        Self {
            registry: Arc::new(RequestRegistry::new()),
        }
    }

    fn reserve_embedding(
        &self,
        operation_id: &str,
        cancellation: Arc<AtomicBool>,
    ) -> NativeResult<(Arc<ActiveRequest>, RequestLease)> {
        self.registry.reserve(
            operation_id,
            RequestClass::Embedding,
            RequestControls::Embedding { cancellation },
        )
    }

    fn request_cancel(entry: &ActiveRequest) -> usize {
        entry.cancel_all()
    }

    fn quiesce(&self) {
        self.registry.begin_quiesce_and_cancel_all();
    }

    fn close_if_drained(&self) -> NativeResult<NativeRegistryFacts> {
        self.registry.mark_closed()?;
        Ok(self.facts())
    }

    fn wait_until_drained(&self) {
        self.registry.wait_until_drained();
    }

    fn facts(&self) -> NativeRegistryFacts {
        let state = self.registry.lock_recovering_poison();
        let lifecycle = match state.phase {
            RegistryPhase::Running => LifecyclePhase::Running,
            RegistryPhase::Quiescing => LifecyclePhase::Quiescing,
            RegistryPhase::Closed => LifecyclePhase::Closed,
        };
        NativeRegistryFacts {
            lifecycle,
            active_operations: state.active.len(),
            released_leases: self.registry.release_count(),
        }
    }
}

#[test]
fn real_reservation_cancel_and_lease_release_bind_contract_identity() {
    let adapter = NativeRegistryContractAdapter::new();
    let cancellation = Arc::new(AtomicBool::new(false));
    let (entry, lease) = adapter
        .reserve_embedding("contract-operation", Arc::clone(&cancellation))
        .expect("real registry admits the first operation");
    assert_eq!(entry.request_id(), "contract-operation");
    let reservation_nonce = entry.reservation_nonce;
    assert_eq!(adapter.facts().active_operations, 1);

    let duplicate = adapter
        .reserve_embedding("contract-operation", Arc::new(AtomicBool::new(false)))
        .expect_err("real registry rejects duplicate active identity");
    assert_eq!(duplicate.code, NativeErrorCode::DuplicateActiveRequest);
    assert_eq!(adapter.facts().active_operations, 1);

    assert_eq!(NativeRegistryContractAdapter::request_cancel(&entry), 1);
    assert!(cancellation.load(Ordering::Acquire));
    assert_eq!(adapter.facts().released_leases, 0);

    drop(entry);
    assert_eq!(adapter.facts().active_operations, 1);
    drop(lease);
    assert_eq!(adapter.facts().active_operations, 0);
    assert_eq!(adapter.facts().released_leases, 1);

    let replacement_cancel = Arc::new(AtomicBool::new(false));
    let (replacement, replacement_lease) = adapter
        .reserve_embedding("contract-operation", replacement_cancel)
        .expect("released identity may be reused");
    assert!(replacement.reservation_nonce > reservation_nonce);
    drop(replacement_lease);
    assert_eq!(adapter.facts().released_leases, 2);
}

#[test]
fn real_quiesce_drain_and_close_facts_are_registry_owned() {
    let adapter = NativeRegistryContractAdapter::new();
    let cancellation = Arc::new(AtomicBool::new(false));
    let (_entry, lease) = adapter
        .reserve_embedding("draining-operation", Arc::clone(&cancellation))
        .expect("real registry admits operation");

    adapter.quiesce();
    assert_eq!(adapter.facts().lifecycle, LifecyclePhase::Quiescing);
    assert!(cancellation.load(Ordering::Acquire));
    assert_eq!(adapter.facts().active_operations, 1);
    assert_eq!(
        adapter
            .reserve_embedding("late-operation", Arc::new(AtomicBool::new(false)))
            .expect_err("quiescing registry rejects admission")
            .code,
        NativeErrorCode::WorkerStopped
    );
    assert!(
        adapter.close_if_drained().is_err(),
        "an active real executor lease prevents closed facts"
    );

    let waiter = adapter.clone();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
    let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(0);
    let thread = std::thread::spawn(move || {
        started_tx
            .send(())
            .expect("test coordinator remains available");
        waiter.wait_until_drained();
        drained_tx
            .send(())
            .expect("test coordinator remains available");
    });
    started_rx.recv().expect("drain waiter started");
    assert!(
        drained_rx
            .recv_timeout(std::time::Duration::from_millis(10))
            .is_err(),
        "drain must wait for the production lease"
    );
    drop(lease);
    drained_rx.recv().expect("lease drop wakes real drain");
    thread.join().expect("drain waiter joins");

    let closed = adapter
        .close_if_drained()
        .expect("drained production registry closes");
    assert_eq!(
        closed,
        NativeRegistryFacts {
            lifecycle: LifecyclePhase::Closed,
            active_operations: 0,
            released_leases: 1,
        }
    );
    assert_eq!(adapter.close_if_drained(), Ok(closed));
}
