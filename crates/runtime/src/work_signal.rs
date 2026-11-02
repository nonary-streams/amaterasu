//! Generation-based wakeups close the notification-during-tick race. No queue,
//! frame bytes or provider identities pass through this scheduling primitive.
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct WorkSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}
impl WorkSignal {
    pub fn observe(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn notify(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = generation.wrapping_add(1);
        self.changed.notify_one();
    }
    pub fn wait(&self, observed: u64, timeout: Duration) {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = self
            .changed
            .wait_timeout_while(generation, timeout, |generation| *generation == observed)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Instant};
    #[test]
    fn notification_during_a_tick_is_not_lost_before_wait() {
        let signal = WorkSignal::default();
        let observed = signal.observe();
        signal.notify();
        let started = Instant::now();
        signal.wait(observed, Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn notification_from_a_worker_interrupts_the_idle_deadline() {
        let signal = Arc::new(WorkSignal::default());
        let observed = signal.observe();
        let worker_signal = signal.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            worker_signal.notify();
        });
        let started = Instant::now();
        signal.wait(observed, Duration::from_secs(5));
        worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        let next = signal.observe();
        assert_ne!(next, observed);
        let started = Instant::now();
        signal.wait(next, Duration::from_millis(10));
        assert!(started.elapsed() >= Duration::from_millis(10));
    }
}
