//! Compile-time-only backend for real-module qualification. It never opens a
//! device or forwards input. This feature must not be enabled in production.
use super::Backend;
use crate::{
    feedback::FeedbackQueue,
    model::{Event, InputResult},
};
use amaterasu_gamestream::control::HostFeedback;
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

pub struct QualificationBackend {
    feedback: FeedbackQueue,
    generation: u64,
    active: [bool; 16],
}
impl QualificationBackend {
    pub fn new() -> Self {
        Self {
            feedback: FeedbackQueue::default(),
            generation: 0,
            active: [false; 16],
        }
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
    fn controller_input_supported(&self) -> bool {
        true
    }
    fn feedback_device_active(&self, controller: u16) -> bool {
        self.active
            .get(usize::from(controller))
            .copied()
            .unwrap_or(false)
    }
    fn feedback_device_offer(&self, controller: u16) -> &'static [&'static str] {
        if self.feedback_device_active(controller) {
            &["org.amaterasu.feedback.rgb_led"]
        } else {
            &[]
        }
    }
    fn set_feedback_queue(&mut self, queue: FeedbackQueue) {
        self.feedback = queue;
    }
    fn set_feedback_generation(&mut self, generation: u64) {
        self.generation = generation;
    }
    fn disable_controller_input(&mut self) -> InputResult<()> {
        for (slot, active) in self.active.iter_mut().enumerate() {
            if *active {
                self.feedback.retire_controller(slot as u16);
                *active = false;
            }
        }
        Ok(())
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
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        event.validate()?;
        if let Event::Controller { active_mask, .. } = event {
            for (slot, active) in self.active.iter_mut().enumerate() {
                if *active && active_mask & (1 << slot) == 0 {
                    self.feedback.retire_controller(slot as u16);
                    *active = false;
                }
            }
        }
        if let Event::ControllerArrival {
            number,
            capabilities,
            ..
        } = event
        {
            self.feedback.retire_controller(*number);
            self.active[usize::from(*number)] = true;
            let producer = self
                .feedback
                .producer(self.generation, *number, *capabilities);
            producer.push(HostFeedback::RgbLed {
                controller: *number,
                red: 1,
                green: 2,
                blue: 3,
            });
        }
        INJECTED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn stop(&mut self) -> InputResult<()> {
        self.disable_controller_input()?;
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
        assert!(amaterasu_input_qualification_injected() > before_injected);
        assert!(amaterasu_input_qualification_stops() > before_stops);
    }

    #[test]
    fn qualification_controller_emits_bounded_feedback_and_retires_its_incarnation() {
        let mut backend = QualificationBackend::new();
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        backend.set_feedback_queue(queue.clone());
        backend.set_feedback_generation(generation);
        backend.start().unwrap();
        let arrival = Event::ControllerArrival {
            number: 2,
            controller_type: 0,
            capabilities: 0,
            supported_buttons: 0,
        };
        backend.inject(&arrival).unwrap();
        assert!(backend.feedback_device_active(2));
        assert_eq!(
            backend.feedback_device_offer(2),
            &["org.amaterasu.feedback.rgb_led"]
        );
        assert_eq!(queue.device_observation(2), Some((1, 0)));
        assert_eq!(queue.peek(16).len(), 1);
        let old = queue.producer(generation, 2, 0);
        backend.inject(&arrival).unwrap();
        assert_eq!(queue.device_observation(2), Some((2, 0)));
        assert!(!old.push(HostFeedback::RgbLed {
            controller: 2,
            red: 4,
            green: 5,
            blue: 6
        }));
        let removal = Event::Controller {
            number: 2,
            active_mask: 0,
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
        };
        backend.inject(&removal).unwrap();
        assert!(!backend.feedback_device_active(2));
        assert_eq!(queue.device_observation(2), None);
        backend.inject(&arrival).unwrap();
        assert_eq!(queue.device_observation(2), Some((3, 0)));
        backend.stop().unwrap();
        assert!(!backend.feedback_device_active(2));
        assert_eq!(queue.device_observation(2), None);
    }

    #[test]
    fn qualification_backend_works_through_the_selected_input_controller() {
        use crate::{controller::Controller, model::Operation};
        use serde_json::json;
        use std::time::Instant;

        let owner = "amaterasu.stream-session";
        let now = Instant::now();
        let mut controller = Controller::new(QualificationBackend::new(), true);
        let acquire: Operation =
            serde_json::from_value(json!({"action":"acquire", "ttl_ms":1000, "controller":true}))
                .unwrap();
        let result = controller.handle(owner, acquire, now, &mut || Ok(()));
        assert_eq!(result["ok"], true, "{result}");
        let lease = result["lease"].as_str().unwrap().to_owned();
        let batch: Operation = serde_json::from_value(json!({"action":"batch", "lease":lease,
            "events":[{"kind":"controller_arrival", "number":2, "controller_type":0,
            "capabilities":0, "supported_buttons":0}]}))
        .unwrap();
        let result = controller.handle(owner, batch, now, &mut || Ok(()));
        assert_eq!(result["ok"], true, "{result}");
        assert!(controller.feedback_policy_is_immutable());
        assert_eq!(
            controller
                .feedback_device_observation(owner, &lease, 2)
                .unwrap(),
            (1, 0)
        );
        assert_eq!(
            controller
                .feedback_offer_observation(owner, &lease, 2)
                .unwrap(),
            ["org.amaterasu.feedback.rgb_led"]
        );
        assert!(
            controller
                .feedback_device_observation("foreign", &lease, 2)
                .is_err()
        );
        let result = controller.handle(
            owner,
            Operation::FeedbackPeek { lease, limit: 1 },
            now,
            &mut || Ok(()),
        );
        assert_eq!(
            result["events"][0]["feedback"]["type"], "rgb_led",
            "{result}"
        );
    }
}
