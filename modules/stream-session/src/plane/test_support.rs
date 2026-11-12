//! In-process transport for integration tests using the real feedback pump.
use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
pub(crate) struct TransportState {
    pub controls: VecDeque<ControlRecord>,
    pub busy: bool,
    pub sent: Vec<HostFeedbackMessage>,
}
struct Transport(Rc<RefCell<TransportState>>);
impl Backend for Transport {
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>> {
        Ok(self.0.borrow_mut().controls.pop_front())
    }
    fn send_feedback(&mut self, message: &HostFeedbackMessage) -> PlaneResult<bool> {
        let mut state = self.0.borrow_mut();
        if state.busy {
            return Ok(false);
        }
        state.sent.push(message.clone());
        Ok(true)
    }
    fn send_rumble(&mut self, _: RumbleCommand) -> PlaneResult<bool> {
        Ok(true)
    }
    fn video(&mut self) -> PlaneResult<Option<VideoPacket>> {
        Ok(None)
    }
    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>> {
        Ok(None)
    }
    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>> {
        Ok(None)
    }
    fn submit_video(&mut self, _: &EncodedPacket, _: u64) -> PlaneResult<bool> {
        Ok(true)
    }
    fn submit_pcm(&mut self, _: &AudioFrame) -> PlaneResult<bool> {
        Ok(true)
    }
    fn submit_audio(&mut self, _: &AudioUnit) -> PlaneResult<bool> {
        Ok(true)
    }
    fn drain_encoder(&mut self) -> PlaneResult<bool> {
        Ok(false)
    }
    fn drain_transport(&mut self) -> PlaneResult<bool> {
        Ok(false)
    }
}
pub(crate) struct TestPlane {
    pump: Pump<Transport>,
    stopped_tick: bool,
}
impl TestPlane {
    pub fn new(state: Rc<RefCell<TransportState>>) -> Self {
        Self {
            pump: Pump::with_media(Transport(state), false, 2, CODEC_H264),
            stopped_tick: false,
        }
    }
    pub fn tick(&mut self) -> Result<Progress, TickFailure> {
        if self.pump.stopping {
            self.stopped_tick = true;
        }
        self.pump.tick(false)
    }
    pub fn enqueue_feedback(
        &mut self,
        feedback: HostFeedback,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError> {
        self.pump.enqueue_feedback(feedback)
    }
    pub fn enqueue_contribution(
        &mut self,
        message: HostFeedbackMessage,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError> {
        self.pump.enqueue_message(PendingFeedback {
            message,
            contribution: true,
        })
    }
    pub fn pump_contributions(&mut self, allowed: bool) -> PlaneResult<()> {
        self.pump.pump_feedback(allowed)
    }
    pub fn discard_contributions(&mut self) {
        self.pump.discard_contributions();
    }
    pub fn stop(&mut self) {
        self.pump.stopping = true;
        self.pump.cancel_feedback();
    }
    pub fn is_drained(&self) -> bool {
        self.pump.stopping && self.stopped_tick
    }
}
