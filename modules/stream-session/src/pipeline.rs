mod actions;
mod cleanup;
mod display;
mod history;
mod history_activity;
mod host_metrics_activity;
mod input;
mod journal;
mod lifecycle;
mod record;
mod responses;
mod rpc;
mod session_effects;
mod shared_capture;
#[cfg(test)]
mod tests;
mod virtual_display;
mod workflow;

use crate::{
    SERVICE,
    model::{Operation, Settings},
    plane::{
        FeedbackEnqueueError, FeedbackEnqueueOutcome, Handles, Plane, Progress as PlaneProgress,
        TickFailure as PlaneFailure,
    },
};
use amaterasu_gamestream::control::HostFeedback;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use record::{Record, Resource, State, error};
use rpc::Send;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};
use workflow::{Job, Step};

trait Drive {
    fn tick(&mut self, media_active: bool) -> Result<PlaneProgress, PlaneFailure>;
    fn send_rumble(&mut self, command: crate::plane::RumbleCommand) -> Result<bool, &'static str>;
    fn stop(&mut self);
    fn is_drained(&self) -> bool;
    fn recover_audio(&mut self);
    fn enqueue_feedback(
        &mut self,
        feedback: HostFeedback,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError>;
}

impl Drive for Plane {
    fn tick(&mut self, media_active: bool) -> Result<PlaneProgress, PlaneFailure> {
        Plane::tick(self, media_active)
    }
    fn send_rumble(&mut self, command: crate::plane::RumbleCommand) -> Result<bool, &'static str> {
        Plane::send_rumble(self, command)
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
    fn enqueue_feedback(
        &mut self,
        feedback: HostFeedback,
    ) -> Result<FeedbackEnqueueOutcome, FeedbackEnqueueError> {
        Plane::enqueue_feedback(self, feedback)
    }
}

type Factory<'a> = dyn FnMut(Handles, &crate::providers::ProviderServices) -> Result<Box<dyn Drive>, &'static str>
    + 'a;

pub struct Pipeline {
    settings: Settings,
    records: BTreeMap<u64, Record>,
    job: Option<Job>,
    sequence: u64,
    started: Instant,
    live_status_epoch: Option<String>,
    background_turn: bool,
    control_after: u64,
    history_after: u64,
    history_activities: history_activity::HistoryActivities,
    tray_activity: session_effects::TrayActivityEffects,
    journal: journal::LifecycleJournal,
    recovery_initialized: bool,
    recovery_queue: VecDeque<u64>,
    recovery_ids: BTreeSet<u64>,
    recovery_ready: bool,
    recovery_blocked: bool,
    package_barrier_registered: bool,
    package_barrier_pending: bool,
    package_barrier_next_poll: u64,
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
            live_status_epoch: None,
            background_turn: false,
            control_after: 0,
            history_after: 0,
            history_activities: history_activity::HistoryActivities::default(),
            tray_activity: session_effects::TrayActivityEffects::default(),
            journal: journal::LifecycleJournal::new()?,
            recovery_initialized: false,
            recovery_queue: VecDeque::new(),
            recovery_ids: BTreeSet::new(),
            recovery_ready: false,
            recovery_blocked: false,
            package_barrier_registered: false,
            package_barrier_pending: false,
            package_barrier_next_poll: 0,
        })
    }

    #[cfg(test)]
    pub(super) fn new_for_test(settings: Settings) -> Result<Self, &'static str> {
        let mut pipeline = Self::new(settings)?;
        pipeline.journal = journal::LifecycleJournal::empty_for_test();
        pipeline.recovery_initialized = true;
        pipeline.recovery_ready = true;
        Ok(pipeline)
    }

    #[cfg(test)]
    pub(super) fn new_for_restart_test(settings: Settings) -> Result<Self, &'static str> {
        Self::new(settings)
    }

    /// Unload cannot drive asynchronous provider cleanup. Release native bindings
    /// and settle broker work; active provider resources require their own unload.
    pub fn shutdown(&mut self, runtime: &RuntimeClient) {
        self.shutdown_with(&mut |command| runtime.send_ephemeral(command));
    }

    fn shutdown_with(&mut self, send: &mut Send<'_>) -> bool {
        let mut settled = true;

        if let Some(job) = self.job.as_mut()
            && job.step == Step::ApplyDisplay
            && let Some(mut pending) = job.pending.take()
            && let Some(record) = self.records.get_mut(&job.session)
        {
            if !pending.was_sent() {
                record.display_attempted = false;
            } else {
                let mut resolved = false;
                for turn in 0..8 {
                    match pending.poll(send, turn) {
                        rpc::Progress::Result(response) => {
                            display::accept_apply(record, &response);
                            resolved = pending.release_result(send);
                            break;
                        }
                        rpc::Progress::Waiting => {}
                        rpc::Progress::Failed | rpc::Progress::Unresolved => break,
                    }
                }
                if !resolved {
                    let cleanup_settled = pending.shutdown(send);
                    if !cleanup_settled {
                        record.uncertain = Some(pending);
                        record.uncertain_step = Some(Step::ApplyDisplay);
                    }
                    settled &= cleanup_settled;
                }
            }
        }
        if let Some(job) = self.job.as_mut()
            && matches!(
                job.step,
                Step::CreateVirtualDisplay
                    | Step::StatusVirtualDisplay
                    | Step::SnapshotDisplay
                    | Step::RemoveVirtualDisplay
            )
            && let Some(mut pending) = job.pending.take()
            && let Some(record) = self.records.get_mut(&job.session)
        {
            if !pending.was_sent() {
                if job.step == Step::CreateVirtualDisplay {
                    record.virtual_display_attempted = false;
                }
                if job.step == Step::RemoveVirtualDisplay {
                    record.virtual_display_remove_attempted = false;
                }
            } else if !pending.shutdown(send) {
                record.uncertain = Some(pending);
                record.uncertain_step = Some(job.step);
                settled = false;
            }
        }
        for record in self.records.values_mut() {
            record.plane = None;
            settled &= record.history.shutdown(send);
            let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            settled &= record.host_metrics.shutdown(record.id, now, send);
            if let Some(pending) = record.uncertain.as_mut() {
                settled &= pending.shutdown(send);
            }
            let display_request_uncertain = record.uncertain.is_some()
                && matches!(
                    record.uncertain_step,
                    Some(
                        Step::CheckDisplay
                            | Step::ApplyDisplay
                            | Step::FindDisplay
                            | Step::RestoreDisplay
                            | Step::RecoverDisplay
                            | Step::CreateVirtualDisplay
                            | Step::StatusVirtualDisplay
                            | Step::SnapshotDisplay
                            | Step::RemoveVirtualDisplay
                    )
                );
            if display_request_uncertain {
                settled = false;
            } else {
                settled &= display::shutdown(record, send);
                settled &= if record.config.virtual_display_intent.is_some() {
                    virtual_display::shutdown(record, send)
                } else {
                    virtual_display::owned_shutdown(record, send)
                };
            }
        }
        settled &= self.history_activities.shutdown(send);
        if let Some(pending) = self.job.as_mut().and_then(|job| job.pending.as_mut()) {
            settled &= pending.shutdown(send);
        }
        settled
    }

    pub fn enable_package_barrier(&mut self) {
        self.package_barrier_registered = true;
    }

    fn package_replacement_ready(&self) -> bool {
        self.journal.loaded
            && !self.journal.failed
            && self.recovery_ready
            && !self.recovery_blocked
            && self.journal.document.entries.is_empty()
            && self.recovery_queue.is_empty()
            && self.job.is_none()
            && !self.history_activities.has_active_reservations()
            && self.records.values().all(|record| {
                record.state.terminal()
                    && !record.history.has_work()
                    && record.uncertain.is_none()
                    && record.plane.is_none()
                    && !record.audio_recovery_needed
            })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let poll_barrier = self.package_barrier_registered && now >= self.package_barrier_next_poll;
        if poll_barrier {
            match runtime.send(Command::HostPackageBarrierPending {})? {
                Response::HostPackageActivationState { pending, .. } => {
                    self.package_barrier_pending = pending
                }
                _ => return Err("package activation barrier status unavailable".into()),
            }
            self.package_barrier_next_poll = now.saturating_add(100);
        }
        let result = self.tick_with(
            now,
            &mut |command| runtime.send_ephemeral(command),
            &mut |handles, providers| {
                Plane::new(runtime, handles, providers)
                    .map(|plane| Box::new(plane) as Box<dyn Drive>)
            },
        );
        if poll_barrier {
            let ready =
                result.is_ok() && self.package_barrier_pending && self.package_replacement_ready();
            match runtime.send(Command::HostPackageBarrierReady { ready })? {
                Response::Ok => {}
                _ => return Err("package activation barrier update failed".into()),
            }
        }
        result
    }

    fn tick_with(
        &mut self,
        now: u64,
        send: &mut Send<'_>,
        factory: &mut Factory<'_>,
    ) -> ModuleResult<()> {
        if !self.journal.loaded {
            match self.journal.progress_load(send, now) {
                journal::LoadProgress::Waiting => return Ok(()),
                journal::LoadProgress::Failed => {
                    self.recovery_blocked = true;
                }
                journal::LoadProgress::Ready => {}
            }
        }
        if self.journal.loaded && !self.recovery_initialized {
            self.initialize_recovery(now)?;
        }
        if self.job.is_none() && !self.recovery_queue.is_empty() {
            self.start_next_recovery(now);
        }
        self.pump(now);
        self.history_activities.progress(now, &self.records, send);
        if let Some(job) = self.job.as_mut()
            && job.step == Step::WaitDrain
            && self.records[&job.session].audio_recovery_needed
        {
            job.step = Step::RecoverAudio;
        }
        self.progress_history(now, send);
        self.progress_host_metrics(now, send);
        self.progress_session_effects(now, send);
        self.tray_activity.progress(now, send);
        if let Some(mut job) = self.job.take() {
            let session = job.session;
            let mut record = self
                .records
                .remove(&job.session)
                .expect("owned workflow record");
            let retain = self.advance(&mut record, &mut job, now, send, factory);
            self.records.insert(record.id, record);
            if retain {
                self.job = Some(job);
            } else if self.recovery_ids.remove(&session) {
                if self.journal.document.entries.contains_key(&session) {
                    self.recovery_blocked = true;
                }
                self.recovery_ready = self.recovery_queue.is_empty()
                    && self.journal.document.entries.is_empty()
                    && !self.recovery_blocked;
            }
            return Ok(());
        }
        if !self.recovery_ready || self.recovery_blocked || self.journal.failed {
            // Keep the module responsive, but never admit new launch state while
            // durable recovery is unreadable or an owned resource is uncertain.
            match send(Command::Poll {})? {
                Response::Request {
                    id,
                    caller,
                    service,
                    payload,
                    ..
                } => {
                    if service == SERVICE
                        && let Ok(operation) = serde_json::from_value::<Operation>(payload)
                    {
                        self.receive(id, caller, operation, now, 0, send);
                    } else {
                        rpc::complete(send, id, error("restart_recovery_unconfirmed"));
                    }
                }
                Response::Pending => {}
                _ => return Err("stream session poll failed".into()),
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

    fn initialize_recovery(&mut self, now: u64) -> ModuleResult<()> {
        self.recovery_initialized = true;
        let entries: Vec<_> = self.journal.entries().cloned().collect();
        for entry in entries {
            if !self
                .settings
                .allowed_principals
                .iter()
                .any(|principal| principal == &entry.owner)
            {
                self.recovery_blocked = true;
                self.journal.failed = true;
                self.recovery_ready = false;
                return Ok(());
            }
            let record = match entry.recovery_record() {
                Ok(record) => record,
                Err(_) => {
                    self.recovery_blocked = true;
                    self.journal.failed = true;
                    self.recovery_ready = false;
                    return Ok(());
                }
            };
            let id = record.id;
            if self.records.insert(id, record).is_some() {
                self.recovery_blocked = true;
                self.journal.failed = true;
                self.recovery_ready = false;
                return Ok(());
            }
            self.recovery_queue.push_back(id);
            self.recovery_ids.insert(id);
        }
        self.recovery_ready = self.recovery_queue.is_empty();
        if !self.recovery_ready {
            self.start_next_recovery(now);
        }
        Ok(())
    }

    fn start_next_recovery(&mut self, now: u64) {
        let Some(id) = self.recovery_queue.pop_front() else {
            return;
        };
        let mut job = Job::new(id, None, Step::WaitDrain, now.saturating_add(30_000));
        job.cleanup = true;
        self.job = Some(job);
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

    fn progress_host_metrics(&mut self, now: u64, send: &mut Send<'_>) {
        for record in self.records.values_mut() {
            record.host_metrics.progress(record.id, now, send);
        }
    }

    fn progress_session_effects(&mut self, now: u64, send: &mut Send<'_>) {
        for record in self.records.values_mut() {
            record.session_effects.progress(now, send);
        }
    }

    fn activate_tray_session(&mut self, record_id: u64) {
        self.tray_activity.activated(record_id);
    }

    fn stop_tray_session(&mut self, record_id: u64) {
        self.tray_activity.teardown_confirmed(record_id);
    }

    fn schedule_control(&mut self, now: u64) -> bool {
        let has_control_work = |record: &Record| {
            record.force_idr
                || (record.input_lease.is_some()
                    && (now >= record.input_renew_at
                        || (record.feedback_ack_through.is_some()
                            && now >= record.feedback_poll_at)
                        || !record.input_events.is_empty()
                        || now >= record.feedback_poll_at
                        || (record.input_controller_supported
                            && record.client_haptics_requested
                            && now >= record.rumble_poll_at)))
        };
        let eligible = |record: &&Record| {
            (matches!(record.state, State::Active | State::Stopping)
                && record.audio_recovery_needed)
                || (record.state == State::Active
                    && (record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.manages_lifecycle_capture())
                        && !record.lifecycle_delivery_permit
                        || record.force_idr
                        || (record.input_lease.is_some()
                            && (now >= record.input_renew_at
                                || (record.feedback_ack_through.is_some()
                                    && now >= record.feedback_poll_at)
                                || !record.input_events.is_empty()
                                || now >= record.feedback_poll_at
                                || (record.input_controller_supported
                                    && record.client_haptics_requested
                                    && now >= record.rumble_poll_at)))))
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
        } else if record.state == State::Active
            && record
                .start
                .as_ref()
                .is_some_and(|start| start.manages_lifecycle_capture())
            && !record.lifecycle_delivery_permit
            && !(record.lifecycle_control_permit && has_control_work(record))
        {
            (Step::ObserveActiveBinding, now.saturating_add(5000))
        } else if record.audio_recovery_needed {
            (Step::RecoverAudio, now.saturating_add(5000))
        } else if record.input_lease.is_some()
            && record.input_controller_supported
            && record.client_haptics_requested
            && now >= record.rumble_poll_at
        {
            (Step::PollGamepadFeedback, now.saturating_add(5000))
        } else if record.input_lease.is_some() && now >= record.input_renew_at {
            (
                Step::RenewInput,
                record.input_expires_at.min(now.saturating_add(5000)),
            )
        } else if record.input_lease.is_some()
            && record.feedback_ack_through.is_some()
            && now >= record.feedback_poll_at
        {
            (
                Step::FeedbackAck,
                record.input_expires_at.min(now.saturating_add(5000)),
            )
        } else if !record.input_events.is_empty() {
            (
                Step::InputBatch,
                record.input_expires_at.min(now.saturating_add(5000)),
            )
        } else if record.input_lease.is_some() && now >= record.feedback_poll_at {
            (Step::FeedbackPeek, now.saturating_add(5000))
        } else {
            (Step::ForceIdr, now + 5000)
        };
        let id = record.id;
        if step == Step::ObserveActiveBinding {
            let record = self.records.get_mut(&id).expect("selected record");
            let Some(next) = record.lifecycle_validation_sequence.checked_add(1) else {
                record.state = State::Stopping;
                record.outcome = State::Failed;
                record.error = Some("lifecycle_sequence_exhausted");
                return true;
            };
            record.lifecycle_validation_sequence = next;
        } else if let Some(record) = self.records.get_mut(&id)
            && record
                .start
                .as_ref()
                .is_some_and(|start| start.manages_lifecycle_capture())
        {
            record.lifecycle_control_permit = false;
        }
        self.job = Some(Job::new(id, None, step, deadline));
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
            for slot in record
                .rumble_active_until
                .iter()
                .filter_map(|(slot, expires)| (*expires <= now).then_some(*slot))
                .collect::<Vec<_>>()
            {
                record.rumble_active_until.remove(&slot);
                record.rumble_pending.insert(
                    slot,
                    (
                        crate::plane::RumbleCommand {
                            slot,
                            low_frequency: 0,
                            high_frequency: 0,
                        },
                        None,
                    ),
                );
                record
                    .rumble_shutdown_deadline
                    .get_or_insert(now.saturating_add(200));
            }
            if record.state == State::Stopping && record.rumble_shutdown_deadline.is_none() {
                input::queue_rumble_zeros(record, now);
            }
            if matches!(record.state, State::Prepared | State::Armed)
                && now >= record.prepared_until
            {
                record.state = State::Stopping;
                record.outcome = State::Failed;
                record.error = Some("prepare_expired");
            }
            if record.state == State::Stopping
                && record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.lifecycle_target.is_some())
            {
                record.plane = None;
                record.input_events.clear();
                continue;
            }
            if record.state == State::Active
                && record.input_lease.is_some()
                && now >= record.input_expires_at
            {
                input::stop_for_input_failure(record, "input_lease_expired");
            }
            if record.state == State::Active
                && record
                    .rumble_shutdown_deadline
                    .is_some_and(|deadline| now >= deadline)
                && record
                    .rumble_pending
                    .values()
                    .any(|(command, _)| command.low_frequency == 0 && command.high_frequency == 0)
            {
                record.rumble_stop_unconfirmed = true;
                record.error.get_or_insert("rumble_zero_delivery_timeout");
                record.outcome = State::Failed;
                record.state = State::Stopping;
                record.rumble_pending.clear();
                record.rumble_active_until.clear();
            }
            if record.state == State::Active
                && record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.manages_lifecycle_capture())
            {
                if !record.lifecycle_delivery_permit {
                    continue;
                }
                record.lifecycle_delivery_permit = false;
            }
            let Some(plane) = record.plane.as_mut() else {
                continue;
            };
            if record.error == Some("rumble_zero_delivery_timeout") {
                plane.stop();
            }
            if let Some((slot, (command, expires_at))) = record
                .rumble_pending
                .first_key_value()
                .map(|(slot, value)| (*slot, *value))
            {
                match plane.send_rumble(command) {
                    Ok(true) => {
                        record.rumble_pending.remove(&slot);
                        if command.low_frequency == 0 && command.high_frequency == 0 {
                            record.rumble_active_until.remove(&slot);
                            record.rumble_stop_unconfirmed = true;
                            if !record.rumble_pending.values().any(|(pending, _)| {
                                pending.low_frequency == 0 && pending.high_frequency == 0
                            }) {
                                record.rumble_shutdown_deadline = None;
                            }
                        } else if let Some(expires_at) = expires_at {
                            record.rumble_active_until.insert(slot, expires_at);
                        }
                    }
                    Ok(false) => {}
                    Err(_) => {
                        record.rumble_stop_unconfirmed = true;
                        record.rumble_pending.clear();
                        record.rumble_active_until.clear();
                        record.error.get_or_insert("rumble_delivery_failed");
                        record.outcome = State::Failed;
                        record.state = State::Stopping;
                        record.rumble_shutdown_deadline = Some(now);
                        plane.stop();
                    }
                }
            }
            let media_active = matches!(record.state, State::Active | State::Stopping);
            let result = plane.tick(media_active);
            match result {
                Ok(progress) => {
                    if virtual_display::requested(record)
                        && record.capture_geometry != progress.desktop_geometry
                    {
                        record.capture_geometry = progress.desktop_geometry;
                        if let Err(code) =
                            input::configure_translator(record, record.input_absolute_pointer)
                        {
                            record.error = Some(code);
                            record.state = State::Stopping;
                            record.outcome = State::Failed;
                        }
                    }
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
                                    now,
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
                    eprintln!("amaterasu session media failed code={}", failure.code);
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
