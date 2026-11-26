//! Locking policy (issue #402). The runtime keeps unwinding panics (no `panic = "abort"` in any
//! profile) and tolerates poisoned locks, so one panicking thread never cascades into an abort
//! at every later `lock()`. See `docs/code-map.md`, "Panics and locks".

use std::sync::{Mutex, MutexGuard};

/// `lock()` that recovers the guard when another thread panicked while holding the lock.
pub trait LockExt<T> {
    /// The guard, poisoned or not: the data is what the panicking thread left behind.
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_lock_poisoned_by_a_panicking_thread_is_still_usable() {
        let m = Arc::new(Mutex::new(1));
        let m2 = Arc::clone(&m);
        let joined = std::thread::spawn(move || {
            let mut g = m2.lock_or_recover();
            *g = 2;
            panic!("worker panics holding the lock");
        })
        .join();
        assert!(joined.is_err(), "the worker thread panicked");
        assert!(m.is_poisoned());
        assert_eq!(*m.lock_or_recover(), 2);
        *m.lock_or_recover() = 3;
        assert_eq!(*m.lock_or_recover(), 3);
    }

    #[test]
    fn a_panic_caught_in_a_worker_leaves_the_process_and_locks_usable() {
        // The shape `relay.rs` uses for a Sync now worker: catch_unwind around the work.
        static SHARED: Mutex<u32> = Mutex::new(0);
        let outcome = std::panic::catch_unwind(|| {
            let _g = SHARED.lock_or_recover();
            panic!("worker bug");
        });
        assert!(outcome.is_err());
        *SHARED.lock_or_recover() += 1;
        assert_eq!(*SHARED.lock_or_recover(), 1);
    }
}
