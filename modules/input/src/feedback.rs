//! Bounded feedback mailbox owned by the active input lease.
use amaterasu_gamestream::control::{HostFeedback, encode_host_feedback};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_FEEDBACK_EVENTS: usize = 256;
const HAPTICS_PCM_CAPABILITY: u32 = 0x8000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedFeedback {
    pub sequence: u64,
    pub device_generation: u64,
    pub device_capabilities: u32,
    pub feedback: HostFeedback,
}

struct FeedbackState {
    generation: u64,
    active: bool,
    next_sequence: u64,
    last_peeked_sequence: u64,
    device_generation: [u64; 16],
    device_active: [bool; 16],
    device_capabilities: [u32; 16],
    events: VecDeque<QueuedFeedback>,
}

impl Default for FeedbackState {
    fn default() -> Self {
        Self {
            generation: 0,
            active: false,
            next_sequence: 0,
            last_peeked_sequence: 0,
            device_generation: [1; 16],
            device_active: [false; 16],
            device_capabilities: [0; 16],
            events: VecDeque::new(),
        }
    }
}

impl FeedbackState {
    fn retire_active_devices(&mut self) {
        for slot in 0..self.device_active.len() {
            if self.device_active[slot] {
                self.device_active[slot] = false;
                self.device_generation[slot] =
                    self.device_generation[slot].checked_add(1).unwrap_or(0);
            }
        }
    }
}

/// Shared between the input controller and native callback threads. Every
/// producer carries the generation and client-relative slot captured when its
/// device was created, so a stale callback cannot enter a later lease.
#[derive(Clone, Default)]
pub(crate) struct FeedbackQueue(Arc<Mutex<FeedbackState>>);

impl FeedbackQueue {
    pub(crate) fn begin_lease(&self) -> u64 {
        let mut state = self.lock();
        state.retire_active_devices();
        state.generation = state.generation.wrapping_add(1).max(1);
        state.active = true;
        state.next_sequence = 0;
        state.last_peeked_sequence = 0;
        state.events.clear();
        state.generation
    }

    pub(crate) fn end_lease(&self) {
        let mut state = self.lock();
        state.retire_active_devices();
        state.generation = state.generation.wrapping_add(1).max(1);
        state.active = false;
        state.next_sequence = 0;
        state.last_peeked_sequence = 0;
        state.events.clear();
    }

    /// Fence callbacks before detaching or reusing a slot. Previously queued
    /// feedback remains tagged with its old incarnation until acknowledged.
    /// A zero incarnation is permanently exhausted for this provider process.
    pub(crate) fn retire_controller(&self, controller: u16) {
        let mut state = self.lock();
        let slot = usize::from(controller);
        if slot >= state.device_active.len() || !state.device_active[slot] {
            return;
        }
        state.device_active[slot] = false;
        state.device_generation[slot] = state.device_generation[slot].checked_add(1).unwrap_or(0);
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
        let mut state = self.lock();
        let slot = usize::from(controller);
        let device_generation = if state.active
            && state.generation == generation
            && slot < state.device_active.len()
            && state.device_generation[slot] != 0
        {
            state.device_active[slot] = true;
            state.device_capabilities[slot] = capabilities;
            state.device_generation[slot]
        } else {
            0
        };
        FeedbackProducer {
            queue: self.clone(),
            generation,
            device_generation,
            controller,
            capabilities,
        }
    }

    /// A process-local observation only; caller must separately verify the
    /// owned native slot and lifecycle binding before relying on it.
    pub(crate) fn device_observation(&self, controller: u16) -> Option<(u64, u32)> {
        let state = self.lock();
        let slot = usize::from(controller);
        (state.active
            && slot < state.device_active.len()
            && state.device_active[slot]
            && state.device_generation[slot] != 0)
            .then(|| {
                (
                    state.device_generation[slot],
                    state.device_capabilities[slot],
                )
            })
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

    fn push(
        &self,
        generation: u64,
        device_generation: u64,
        capabilities: u32,
        feedback: HostFeedback,
    ) -> bool {
        if encode_host_feedback(feedback.clone()).is_err() {
            return false;
        }
        let mut state = self.lock();
        let slot = usize::from(feedback.controller());
        if !state.active
            || state.generation != generation
            || device_generation == 0
            || slot >= state.device_active.len()
            || !state.device_active[slot]
            || state.device_generation[slot] != device_generation
        {
            return false;
        }

        if let Some(key) = feedback.coalescing_key()
            && let Some(index) = state.events.iter().position(|event| {
                event.device_generation == device_generation
                    && event.feedback.coalescing_key() == Some(key)
            })
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
        state.events.push_back(QueuedFeedback {
            sequence,
            device_generation,
            device_capabilities: capabilities,
            feedback,
        });
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
    device_generation: u64,
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
        self.queue.push(
            self.generation,
            self.device_generation,
            self.capabilities,
            feedback,
        )
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
        let current = queue.producer(new_generation, 1, 0);
        assert!(current.push(rumble(1, 3, 4)));
        assert_eq!(queue.peek(1)[0].device_generation, 2);
    }

    #[test]
    fn controller_reuse_fences_old_callbacks_and_preserves_tagged_pending_feedback() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let old = queue.producer(generation, 1, HAPTICS_PCM_CAPABILITY);
        let other = queue.producer(generation, 2, 0);
        assert!(old.push(rumble(1, 0, 0)));
        assert!(other.push(rumble(2, 3, 4)));
        let original = queue.peek(8);
        assert_eq!(original[0].device_generation, 1);
        assert_eq!(original[0].device_capabilities, HAPTICS_PCM_CAPABILITY);
        queue.retire_controller(1);
        assert!(!old.push(rumble(1, 5, 6)));
        let replacement = queue.producer(generation, 1, 0);
        assert!(replacement.push(rumble(1, 7, 8)));
        let events = queue.peek(8);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].feedback, rumble(1, 0, 0));
        assert_eq!(events[0].device_generation, 1);
        assert_eq!(events[1].feedback, rumble(2, 3, 4));
        assert_eq!(events[2].feedback, rumble(1, 7, 8));
        assert_eq!(events[2].device_generation, 2);
        assert_eq!(events[2].device_capabilities, 0);
        assert!(!old.push(pcm(1, 1)));
    }

    #[test]
    fn delayed_callback_cannot_enter_reused_controller_slot() {
        use std::sync::mpsc;
        use std::time::Duration;
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let old = queue.producer(generation, 3, 0);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            old.push(rumble(3, 1, 2))
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        queue.retire_controller(3);
        let current = queue.producer(generation, 3, 0);
        assert!(current.push(rumble(3, 3, 4)));
        resume_tx.send(()).unwrap();
        assert!(!worker.join().unwrap());
        let events = queue.peek(8);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].device_generation, 2);
        assert_eq!(events[0].feedback, rumble(3, 3, 4));
    }

    #[test]
    fn exhausted_controller_generation_fails_closed_until_provider_restart() {
        let queue = FeedbackQueue::default();
        let generation = queue.begin_lease();
        let producer = queue.producer(generation, 0, 0);
        queue.lock().device_generation[0] = u64::MAX;
        queue.retire_controller(0);
        assert!(!producer.push(rumble(0, 1, 2)));
        assert!(!queue.producer(generation, 0, 0).push(rumble(0, 1, 2)));
        let next = queue.begin_lease();
        assert!(!queue.producer(next, 0, 0).push(rumble(0, 1, 2)));
        let restarted = FeedbackQueue::default();
        let new_epoch = restarted.begin_lease();
        assert!(restarted.producer(new_epoch, 0, 0).push(rumble(0, 1, 2)));
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
