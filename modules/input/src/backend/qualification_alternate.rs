//! Independently implemented device-free backend for replacement qualification.
use super::Backend;
use crate::{
    feedback::{FeedbackProducer, FeedbackQueue},
    model::{Event, InputResult},
};
use amaterasu_gamestream::control::HostFeedback;
use std::collections::BTreeMap;

#[unsafe(no_mangle)]
pub extern "C" fn amaterasu_input_alternate_qualification_marker() -> u32 {
    0xa17e_0001
}

pub struct AlternateBackend {
    feedback: FeedbackQueue,
    lease_generation: u64,
    controllers: BTreeMap<u16, FeedbackProducer>,
}
impl AlternateBackend {
    pub fn new() -> Self {
        Self {
            feedback: FeedbackQueue::default(),
            lease_generation: 0,
            controllers: BTreeMap::new(),
        }
    }
    fn retire(&mut self, slot: u16) {
        self.controllers.remove(&slot);
        self.feedback.retire_controller(slot);
    }
}
impl Backend for AlternateBackend {
    fn name(&self) -> &'static str {
        "qualification-alternate"
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
    fn feedback_device_active(&self, slot: u16) -> bool {
        self.controllers.contains_key(&slot)
    }
    fn feedback_device_offer(&self, slot: u16) -> &'static [&'static str] {
        if self.controllers.contains_key(&slot) {
            &["org.amaterasu.feedback.rgb_led"]
        } else {
            &[]
        }
    }
    fn set_feedback_queue(&mut self, queue: FeedbackQueue) {
        self.feedback = queue;
    }
    fn set_feedback_generation(&mut self, generation: u64) {
        self.lease_generation = generation;
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
        match event {
            Event::ControllerArrival {
                number,
                capabilities,
                ..
            } => {
                self.retire(*number);
                let producer =
                    self.feedback
                        .producer(self.lease_generation, *number, *capabilities);
                producer.push(HostFeedback::RgbLed {
                    controller: *number,
                    red: 4,
                    green: 5,
                    blue: 6,
                });
                self.controllers.insert(*number, producer);
            }
            Event::Controller { active_mask, .. } => {
                let retired: Vec<_> = self
                    .controllers
                    .keys()
                    .copied()
                    .filter(|slot| active_mask & (1 << slot) == 0)
                    .collect();
                for slot in retired {
                    self.retire(slot);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn disable_controller_input(&mut self) -> InputResult<()> {
        let retired: Vec<_> = self.controllers.keys().copied().collect();
        for slot in retired {
            self.retire(slot);
        }
        Ok(())
    }
    fn stop(&mut self) -> InputResult<()> {
        self.disable_controller_input()
    }
}
