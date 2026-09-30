//! Bounded feedback mailbox owned by the active input lease.
use amaterasu_gamestream::control::{HostFeedback, encode_host_feedback};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_FEEDBACK_EVENTS: usize = 256;
const HAPTICS_PCM_CAPABILITY: u32 = 0x8000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedFeedback {
    pub sequence: u64,
    pub feedback: HostFeedback,
}

#[derive(Default)]
struct FeedbackState {
    generation: u64,
    active: bool,
    next_sequence: u64,
    last_peeked_sequence: u64,
    events: VecDeque<QueuedFeedback>,
}

/// Shared between the input controller and native callback threads. Every
/// producer carries the generation and client-relative slot captured when its
/// device was created, so a stale callback cannot enter a later lease.
#[derive(Clone, Default)]
pub(crate) struct FeedbackQueue(Arc<Mutex<FeedbackState>>);

impl FeedbackQueue {
    pub(crate) fn begin_lease(&self) -> u64 {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1).max(1);
        state.active = true;
        state.next_sequence = 0;
        state.last_peeked_sequence = 0;
        state.events.clear();
        state.generation
    }

    pub(crate) fn end_lease(&self) {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1).max(1);
        state.active = false;
        state.next_sequence = 0;
        state.last_peeked_sequence = 0;
        state.events.clear();
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.lock().generation
    }

    pub(crate) fn producer(
        &self,
        generation: u64,
        controller: u16,
        capabilities: u32,
    ) -> FeedbackProducer {
        FeedbackProducer {
            queue: self.clone(),
            generation,
            controller,
            capabilities,
        }
    }

    pub(crate) fn peek(&self, limit: usize) -> Vec<QueuedFeedback> {
        let mut state = self.lock();
        if !state.active || limit == 0 {
            return Vec::new();
        }
        let events = state.events.iter().take(limit).cloned().collect::<Vec<_>>();
        if let Some(last) = events.last() {
            state.last_peeked_sequence = state.last_peeked_sequence.max(last.sequence);
        }
        events
    }

    pub(crate) fn acknowledge(&self, through_sequence: u64) -> bool {
        let mut state = self.lock();
        if !state.active || through_sequence > state.last_peeked_sequence {
            return false;
        }
        while state
            .events
            .front()
            .is_some_and(|event| event.sequence <= through_sequence)
        {
            state.events.pop_front();
        }
        true
    }

    fn push(&self, generation: u64, feedback: HostFeedback) -> bool {
        if encode_host_feedback(feedback.clone()).is_err() {
            return false;
        }
        let mut state = self.lock();
        if !state.active || state.generation != generation {
            return false;
        }

        if let Some(key) = feedback.coalescing_key()
            && let Some(index) = state
                .events
                .iter()
                .position(|event| event.feedback.coalescing_key() == Some(key))
        {
            state.events.remove(index);
        }

        if state.events.len() >= MAX_FEEDBACK_EVENTS {
            if feedback.is_best_effort_pcm() {
                return false;
            }
            let Some(index) = state
                .events
                .iter()
                .position(|event| event.feedback.is_best_effort_pcm())
            else {
                return false;
            };
            state.events.remove(index);
        }

        state.next_sequence = match state.next_sequence.checked_add(1) {
            Some(sequence) => sequence,
            None => return false,
        };
        let sequence = state.next_sequence;
        state
            .events
            .push_back(QueuedFeedback { sequence, feedback });
        true
    }

    fn lock(&self) -> MutexGuard<'_, FeedbackState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone)]
pub(crate) struct FeedbackProducer {
    queue: FeedbackQueue,
    generation: u64,
    controller: u16,
    capabilities: u32,
}

impl FeedbackProducer {
    pub(crate) fn controller(&self) -> u16 {
        self.controller
    }

    pub(crate) fn supports_haptics_pcm(&self) -> bool {
        self.capabilities & HAPTICS_PCM_CAPABILITY != 0
    }

    pub(crate) fn push(&self, feedback: HostFeedback) -> bool {
        if feedback.controller() != self.controller
            || (feedback.is_best_effort_pcm() && !self.supports_haptics_pcm())
        {
            return false;
        }
        self.queue.push(self.generation, feedback)
    }
}

trait HostFeedbackExt {
    fn controller(&self) -> u16;
    fn is_best_effort_pcm(&self) -> bool;
    fn coalescing_key(&self) -> Option<(u16, FeedbackKind)>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedbackKind {
    Rumble,
    TriggerRumble,
    MotionAccel,
    MotionGyro,
    RgbLed,
    AdaptiveTriggers,
}

impl HostFeedbackExt for HostFeedback {
    fn controller(&self) -> u16 {
        match self {
            Self::Rumble { controller, .. }
            | Self::TriggerRumble { controller, .. }
            | Self::Motion { controller, .. }
            | Self::RgbLed { controller, .. }
            | Self::AdaptiveTriggers { controller, .. }
            | Self::HapticsPcm { controller, .. } => *controller,
        }
    }

    fn is_best_effort_pcm(&self) -> bool {
        matches!(self, Self::HapticsPcm { .. })
    }

    fn coalescing_key(&self) -> Option<(u16, FeedbackKind)> {
        match self {
            Self::Rumble { controller, .. } => Some((*controller, FeedbackKind::Rumble)),
            Self::TriggerRumble { controller, .. } => {
                Some((*controller, FeedbackKind::TriggerRumble))
            }
            Self::Motion {
                controller,
                motion_type,
                ..
            } => Some((
                *controller,
                match motion_type {
                    amaterasu_gamestream::control::ControllerMotionType::Accel => {
                        FeedbackKind::MotionAccel
                    }
                    amaterasu_gamestream::control::ControllerMotionType::Gyro => {
                        FeedbackKind::MotionGyro
                    }
                },
            )),
            Self::RgbLed { controller, .. } => Some((*controller, FeedbackKind::RgbLed)),
            Self::AdaptiveTriggers { controller, .. } => {
                Some((*controller, FeedbackKind::AdaptiveTriggers))
            }
            Self::HapticsPcm { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_gamestream::control::ControllerMotionType;

    fn rumble(controller: u16, low_frequency: u16, high_frequency: u16) -> HostFeedback {
        HostFeedback::Rumble {
            controller,
            low_frequency,
            high_frequency,
        }
    }

    fn pcm(controller: u16, sequence: u32) -> HostFeedback {
        HostFeedback::HapticsPcm {
            controller,
            sequence,
            frames: 1,
            samples_s16le: vec![0; 4],
        }
    }

    #[test]
    fn feedback_is_owned_by_generation_and_retained_until_acknowledged() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 3, HAPTICS_PCM_CAPABILITY);
        assert!(producer.push(rumble(3, 1, 2)));

        let first = queue.peek(8);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].sequence, 1);
        assert_eq!(first[0].feedback, rumble(3, 1, 2));
        assert_eq!(queue.peek(8), first);
        assert!(!queue.acknowledge(2));
        assert_eq!(queue.peek(8), first);
        assert!(queue.acknowledge(first[0].sequence));
        assert!(queue.peek(8).is_empty());
        assert!(queue.acknowledge(first[0].sequence));
    }

    #[test]
    fn stale_callbacks_and_wrong_slot_feedback_cannot_cross_lease_or_slot() {
        let queue = FeedbackQueue::default();
        let old_generation = queue.begin_lease();
        let stale = queue.producer(old_generation, 1, 0);
        assert!(!stale.push(rumble(2, 1, 2)));

        queue.end_lease();
        let new_generation = queue.begin_lease();
        assert_ne!(old_generation, new_generation);
        assert!(!stale.push(rumble(1, 1, 2)));
        assert!(queue.peek(1).is_empty());
    }

    #[test]
    fn pcm_requires_the_client_capability_and_state_updates_coalesce_by_slot() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let no_pcm = queue.producer(generation, 0, 0);
        assert!(!no_pcm.push(pcm(0, 1)));

        let capable = queue.producer(generation, 0, HAPTICS_PCM_CAPABILITY);
        assert!(capable.push(pcm(0, 1)));
        assert!(capable.push(rumble(0, 1, 2)));
        assert!(capable.push(rumble(0, 0, 0)));
        let events = queue.peek(8);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].feedback, pcm(0, 1));
        assert_eq!(events[1].feedback, rumble(0, 0, 0));
    }

    #[test]
    fn full_queue_drops_new_pcm_and_preserves_critical_stop_state() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 0, HAPTICS_PCM_CAPABILITY);
        for sequence in 0..MAX_FEEDBACK_EVENTS as u32 {
            assert!(producer.push(pcm(0, sequence)));
        }
        assert!(!producer.push(pcm(0, 300)));
        assert!(producer.push(rumble(0, 0, 0)));
        let events = queue.peek(MAX_FEEDBACK_EVENTS);
        assert_eq!(events.len(), MAX_FEEDBACK_EVENTS);
        assert_eq!(events.last().unwrap().feedback, rumble(0, 0, 0));
    }

    #[test]
    fn motion_activation_keeps_accel_and_gyro_state_independent() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 4, 0);
        assert!(producer.push(HostFeedback::Motion {
            controller: 4,
            report_rate_hz: 100,
            motion_type: ControllerMotionType::Accel,
        }));
        assert!(producer.push(HostFeedback::Motion {
            controller: 4,
            report_rate_hz: 100,
            motion_type: ControllerMotionType::Gyro,
        }));
        assert_eq!(queue.peek(8).len(), 2);
    }
}
