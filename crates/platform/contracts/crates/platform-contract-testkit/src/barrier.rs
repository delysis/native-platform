use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[derive(Debug)]
struct State {
    arrived: usize,
    released: bool,
}

#[derive(Clone, Debug)]
pub struct DeterministicBarrier {
    expected: usize,
    inner: Arc<(Mutex<State>, Condvar)>,
}

impl DeterministicBarrier {
    #[must_use]
    pub fn new(expected: usize) -> Self {
        assert!(expected > 0, "barrier requires at least one participant");
        Self {
            expected,
            inner: Arc::new((
                Mutex::new(State {
                    arrived: 0,
                    released: false,
                }),
                Condvar::new(),
            )),
        }
    }

    pub fn arrive_and_wait(&self) {
        let (lock, changed) = &*self.inner;
        let mut state = recover_lock(lock);
        state.arrived = state
            .arrived
            .checked_add(1)
            .expect("barrier participant count exhausted");
        assert!(
            state.arrived <= self.expected,
            "more participants arrived than declared"
        );
        changed.notify_all();
        while !state.released {
            state = changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub fn wait_until_all_arrived(&self) {
        let (lock, changed) = &*self.inner;
        let mut state = recover_lock(lock);
        while state.arrived != self.expected {
            state = changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub fn release(&self) {
        let (lock, changed) = &*self.inner;
        let mut state = recover_lock(lock);
        assert_eq!(state.arrived, self.expected, "release before all arrivals");
        state.released = true;
        changed.notify_all();
    }
}

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_occurs_only_after_exact_arrival_count() {
        let barrier = DeterministicBarrier::new(2);
        let first = barrier.clone();
        let second = barrier.clone();
        let a = std::thread::spawn(move || first.arrive_and_wait());
        let b = std::thread::spawn(move || second.arrive_and_wait());
        barrier.wait_until_all_arrived();
        barrier.release();
        a.join().expect("first participant");
        b.join().expect("second participant");
    }
}
