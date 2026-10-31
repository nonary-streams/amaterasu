//! Compile-time-only backend for real-module qualification. It never opens a
//! device or forwards input. This feature must not be enabled in production.
use super::Backend;
use crate::model::{Event, InputResult};
use std::sync::atomic::{AtomicU64, Ordering};

static INJECTED: AtomicU64 = AtomicU64::new(0);
static STOPS: AtomicU64 = AtomicU64::new(0);

/// Counts events delivered to this fake backend, not native input effects.
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_input_qualification_injected() -> u64 {
    INJECTED.load(Ordering::Relaxed)
}

/// Counts fake backend stop calls, including lease cleanup.
#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_input_qualification_stops() -> u64 {
    STOPS.load(Ordering::Relaxed)
}

pub struct QualificationBackend;
impl QualificationBackend {
    pub fn new() -> Self {
        Self
    }
}
impl Backend for QualificationBackend {
    fn name(&self) -> &'static str {
        "qualification-fake"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        true
    }
    fn key_encoding(&self) -> &'static str {
        "linux_evdev"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()
    }
    fn start(&mut self) -> InputResult<()> {
        Ok(())
    }
    fn inject(&mut self, _event: &Event) -> InputResult<()> {
        INJECTED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn stop(&mut self) -> InputResult<()> {
        STOPS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Key, KeyState};

    #[test]
    fn qualification_backend_counts_without_native_devices() {
        let before_injected = amaterasu_input_qualification_injected();
        let before_stops = amaterasu_input_qualification_stops();
        let mut backend = QualificationBackend::new();
        assert_eq!(backend.name(), "qualification-fake");
        assert!(backend.supported());
        backend.start().unwrap();
        let event = Event::Key {
            key: Key::LinuxEvdev { code: 30 },
            state: KeyState::Down,
        };
        backend.validate(&event).unwrap();
        backend.inject(&event).unwrap();
        backend.stop().unwrap();
        assert_eq!(
            amaterasu_input_qualification_injected(),
            before_injected + 1
        );
        assert_eq!(amaterasu_input_qualification_stops(), before_stops + 1);
    }
}
