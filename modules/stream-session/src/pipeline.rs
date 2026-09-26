mod actions;
mod cleanup;
mod history;
mod input;
mod lifecycle;
mod limiter;
mod record;
mod responses;
mod rpc;
#[cfg(test)]
mod tests;
mod workflow;

use crate::{
    SERVICE,
    model::{Operation, Settings},
    plane::{Handles, Plane, Progress as PlaneProgress, TickFailure as PlaneFailure},
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use record::{Record, Resource, State, error};
use rpc::Send;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};
use workflow::{Job, Step};

trait Drive {
    fn tick(&mut self, media_active: bool) -> Result<PlaneProgress, PlaneFailure>;
    fn stop(&mut self);
    fn is_drained(&self) -> bool;
    fn recover_audio(&mut self);
}

impl Drive for Plane {
    fn tick(&mut self, media_active: bool) -> Result<PlaneProgress, PlaneFailure> {
        Plane::tick(self, media_active)
    }
    fn stop(&mut self) {
        Plane::stop(self);
    }
    fn is_drained(&self) -> bool {
        Plane::is_drained(self)
    }
    fn recover_audio(&mut self) {
        Plane::recover_audio(self)
    }
}

type Factory<'a> = dyn FnMut(Handles) -> Result<Box<dyn Drive>, &'static str> + 'a;

pub struct Pipeline {
    settings: Settings,
    records: BTreeMap<u64, Record>,
    job: Option<Job>,
    sequence: u64,
    started: Instant,
    background_turn: bool,
    control_after: u64,
    history_after: u64,
}

impl Pipeline {
    pub fn new(settings: Settings) -> Result<Self, &'static str> {
        settings.validate()?;
        Ok(Self {
            settings,
            records: BTreeMap::new(),
            job: None,
            sequence: 0,
            started: Instant::now(),
            background_turn: false,
            control_after: 0,
            history_after: 0,
        })
    }

    /// Unload cannot drive asynchronous provider cleanup. Release native bindings
    /// and settle broker work; active provider resources require their own unload.
    pub fn shutdown(&mut self, runtime: &RuntimeClient) {
        self.shutdown_with(&mut |command| runtime.send_ephemeral(command));
    }

    fn shutdown_with(&mut self, send: &mut Send<'_>) -> bool {
        let mut settled = true;
        if let Some(job) = self.job.as_mut()
            && job.step == Step::ApplyLimiter
            && let Some(mut pending) = job.pending.take()
            && let Some(record) = self.records.get_mut(&job.session)
        {
            if !pending.was_sent() {
                record.limiter_attempted = false;
            } else {
                let mut resolved = false;
                for turn in 0..8 {
                    match pending.poll(send, turn) {
                        rpc::Progress::Result(response) => {
                            limiter::accept_apply(record, &response);
                            resolved = pending.release_result(send);
                            break;
                        }
                        rpc::Progress::Waiting => {}
                        rpc::Progress::Failed | rpc::Progress::Unresolved => break,
                    }
                }
                if !resolved {
                    settled &= pending.shutdown(send);
                }
            }
        }
        for record in self.records.values_mut() {
            record.plane = None;
            settled &= record.history.shutdown(send);
            if let Some(pending) = record.uncertain.as_mut() {
                settled &= pending.shutdown(send);
            }
            settled &= limiter::shutdown(record, send);
        }
        if let Some(pending) = self.job.as_mut().and_then(|job| job.pending.as_mut()) {
            settled &= pending.shutdown(send);
        }
        settled
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.tick_with(
            now,
            &mut |command| runtime.send_ephemeral(command),
            &mut |handles| {
                Plane::new(runtime, handles).map(|plane| Box::new(plane) as Box<dyn Drive>)
            },
        )
    }

    fn tick_with(
        &mut self,
        now: u64,
        send: &mut Send<'_>,
        factory: &mut Factory<'_>,
    ) -> ModuleResult<()> {
        self.pump(now);
        if let Some(job) = self.job.as_mut()
            && job.step == Step::WaitDrain
            && self.records[&job.session].audio_recovery_needed
        {
            job.step = Step::RecoverAudio;
        }
        self.progress_history(now, send);
        if let Some(mut job) = self.job.take() {
            let mut record = self
                .records
                .remove(&job.session)
                .expect("owned workflow record");
            let retain = self.advance(&mut record, &mut job, now, send, factory);
            self.records.insert(record.id, record);
            if retain {
                self.job = Some(job);
            }
            return Ok(());
        }
        if let Some(record) = self
            .records
            .values()
            .find(|record| record.state == State::Stopping && !record.audio_recovery_needed)
        {
            let mut job = Job::new(record.id, None, Step::Open(Resource::Session), now + 15000);
            job.cleanup = true;
            if let Some(step) = job.next_cleanup(record) {
                job.step = step;
            }
            self.job = Some(job);
            return Ok(());
        }
        if self.background_turn && self.schedule_control(now) {
            return Ok(());
        }
        match send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                service,
                payload,
                remaining_ms,
                ..
            } => {
                self.background_turn = true;
                if service != SERVICE {
                    rpc::complete(send, id, error("invalid_service"));
                    return Ok(());
                }
                match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) => self.receive(id, caller, operation, now, remaining_ms, send),
                    Err(_) => {
                        rpc::complete(send, id, error("invalid_request"));
                    }
                }
                Ok(())
            }
            Response::Pending => {
                self.schedule_control(now);
                Ok(())
            }
            _ => Err("stream session poll failed".into()),
        }
    }

    fn progress_history(&mut self, now: u64, send: &mut Send<'_>) {
        for record in self.records.values_mut() {
            if record.state == State::Active {
                record.history.sample_if_due(now);
            }
        }
        let eligible = |record: &&Record| record.history.has_work();
        let selected = self
            .records
            .values()
            .filter(eligible)
            .find(|record| record.id > self.history_after)
            .or_else(|| self.records.values().find(eligible))
            .map(|record| record.id);
        let Some(id) = selected else {
            return;
        };
        self.history_after = id;
        let queued = {
            let record = self.records.get(&id).expect("selected history record");
            record.history.queued_request(
                &record.owner,
                record
                    .start
                    .as_ref()
                    .expect("history begins after activation"),
            )
        };
        self.records
            .get_mut(&id)
            .expect("selected history record")
            .history
            .progress(queued, now, send);
    }

    fn schedule_control(&mut self, now: u64) -> bool {
        let eligible = |record: &&Record| {
            (matches!(record.state, State::Active | State::Stopping)
                && record.audio_recovery_needed)
                || (record.state == State::Active
                    && (record.force_idr
                        || (record.input_lease.is_some()
                            && (now >= record.input_renew_at || !record.input_events.is_empty()))))
                || (record.state == State::Armed && now >= record.next_auth_poll)
        };
        let next = self
            .records
            .values()
            .filter(eligible)
            .find(|record| record.id > self.control_after)
            .or_else(|| self.records.values().find(eligible));
        let Some(record) = next else {
            return false;
        };
        self.control_after = record.id;
        self.background_turn = false;
        let (step, deadline) = if record.state == State::Armed {
            (
                if record.control_authenticated {
                    Step::MediaReady
                } else {
                    Step::Authenticate
                },
                record.prepared_until,
            )
        } else if record.audio_recovery_needed {
            (Step::RecoverAudio, now.saturating_add(5000))
        } else if record.input_lease.is_some() && now >= record.input_renew_at {
            (
                Step::RenewInput,
                record.input_expires_at.min(now.saturating_add(5000)),
            )
        } else if !record.input_events.is_empty() {
            (
                Step::InputBatch,
                record.input_expires_at.min(now.saturating_add(5000)),
            )
        } else {
            (Step::ForceIdr, now + 5000)
        };
        self.job = Some(Job::new(record.id, None, step, deadline));
        true
    }

    fn new_id(&self) -> Result<u64, &'static str> {
        for _ in 0..8 {
            let mut bytes = [0; 8];
            getrandom::fill(&mut bytes).map_err(|_| "random_unavailable")?;
            let id = u64::from_ne_bytes(bytes);
            if id != 0 && !self.records.contains_key(&id) {
                return Ok(id);
            }
        }
        Err("identifier_exhausted")
    }

    fn pump(&mut self, now: u64) {
        use amaterasu_gamestream::{
            control::{self, Message, Profile},
            crypto::Direction,
        };
        for record in self.records.values_mut() {
            if matches!(record.state, State::Prepared | State::Armed)
                && now >= record.prepared_until
            {
                record.state = State::Stopping;
                record.outcome = State::Failed;
                record.error = Some("prepare_expired");
            }
            if record.state == State::Active
                && record.input_lease.is_some()
                && now >= record.input_expires_at
            {
                input::stop_for_input_failure(record, "input_lease_expired");
            }
            let Some(plane) = record.plane.as_mut() else {
                continue;
            };
            let media_active = matches!(record.state, State::Active | State::Stopping);
            let result = plane.tick(media_active);
            match result {
                Ok(progress) => {
                    record.history.observe_media(
                        progress.accepted_video_access_units,
                        progress.accepted_media_payload_bytes,
                        progress.observed_video_sequence_gaps,
                    );
                    for event in progress.controls {
                        record.control_messages = record.control_messages.saturating_add(1);
                        record.last_control_sequence = Some(event.sequence);
                        let message = match control::decode(
                            Profile::SunshineModern,
                            Direction::ClientToHost,
                            event.message_type,
                            &event.payload,
                        ) {
                            Ok(message) => message,
                            Err(_) => {
                                record.rejected_controls =
                                    record.rejected_controls.saturating_add(1);
                                continue;
                            }
                        };
                        match message {
                            Message::RequestIdr | Message::InvalidateReferences { .. } => {
                                record.force_idr = true
                            }
                            Message::Input(input) => {
                                if let Err(code) = input::queue_control_input(
                                    record,
                                    input,
                                    self.settings.enable_input,
                                ) {
                                    input::stop_for_input_failure(record, code);
                                }
                            }
                            Message::Ping { .. } | Message::Start => {}
                            _ => {
                                record.unsupported_controls =
                                    record.unsupported_controls.saturating_add(1)
                            }
                        }
                    }
                }
                Err(failure) => {
                    record.history.observe_media(
                        failure.progress.accepted_video_access_units,
                        failure.progress.accepted_media_payload_bytes,
                        failure.progress.observed_video_sequence_gaps,
                    );
                    if failure.code == "audio_encoder_failed" && record.config.stream_audio {
                        record.audio_recovery_needed = true;
                        continue;
                    }
                    record.error = Some(failure.code);
                    record.outcome = State::Failed;
                    record.history.stopping(failure.code);
                    record.state = State::Stopping;
                    record.plane = None;
                }
            }
        }
    }
}
