//! Executes provider-selected stream plans without assigning product roles to
//! the protocol consumer. Source ownership stays with the selected provider.
use super::{
    CaptureProvider, Controller, LaunchRequest, PeerIdentity, Reply, StreamProfile, fingerprint,
    private_error,
    record::{Record, State},
    workflow::{ApplicationConfigOverrides, project_application_policy},
};
use amaterasu_module_sdk::session_presentation::{
    Application, AuxiliaryStreamPlan, CaptureSource, Client, Context, Game, Observation,
    SessionEvent, SharedCapture,
};
use std::time::Instant;

pub(super) struct AuxiliaryRecord {
    pub plan: AuxiliaryStreamPlan,
    started_reported: bool,
    ended_reported: bool,
}

pub(super) struct ApplicationSourceHold {
    pub source: SharedCapture,
    pub client_id: String,
    pub transport_id: String,
    pub transport_epoch: Option<String>,
    pub started_reported: bool,
    pub ended_reported: bool,
}

impl ApplicationSourceHold {
    pub(super) fn new(source: SharedCapture, record: &Record) -> Self {
        Self {
            source,
            client_id: fingerprint(record.peer.certificate()),
            transport_id: record.rtsp_operation.clone(),
            transport_epoch: None,
            started_reported: false,
            ended_reported: false,
        }
    }
}

pub(super) struct PendingAuxiliary {
    pub connection: u64,
    request: LaunchRequest,
    pub(super) peer: PeerIdentity,
    plan: AuxiliaryStreamPlan,
    pub(super) delivery_permission: crate::wire::Permission,
}

impl AuxiliaryRecord {
    pub(super) fn new(plan: AuxiliaryStreamPlan) -> Self {
        Self {
            plan,
            started_reported: false,
            ended_reported: false,
        }
    }
}

impl Record {
    pub(super) fn source_observations_emitted(&self) -> bool {
        self.auxiliary
            .as_ref()
            .is_none_or(|auxiliary| auxiliary.ended_reported)
            && (self.shared_capture.is_none() || self.source_ended_reported)
            && self
                .application_source
                .as_ref()
                .is_none_or(|hold| hold.ended_reported)
    }
}

impl Controller {
    /// Recovery workers may fence only operations no longer represented by
    /// this controller, including uncertain cleanup and pending admissions.
    pub fn owns_transport_operation(&self, operation_id: &str, runtime_epoch: &str) -> bool {
        self.records.values().any(|record| {
            !record.state.terminal()
                && record.rtsp_operation == operation_id
                && record.rtsp_runtime_epoch.as_deref() == Some(runtime_epoch)
        })
    }

    pub fn owns_application_operation(&self, operation_id: &str, runtime_epoch: &str) -> bool {
        self.records.values().any(|record| {
            !record.state.terminal()
                && record.application_operation == operation_id
                && record
                    .application_operation
                    .strip_prefix("ale1:")
                    .and_then(|value| value.split_once(':').map(|(epoch, _)| epoch))
                    == Some(runtime_epoch)
        })
    }
    /// Capture aliases are selected by trusted startup composition. A plan
    /// identifies a source kind and never chooses a provider or broker service.
    pub fn set_auxiliary_capture_routes(
        &mut self,
        black: Option<CaptureProvider>,
        exact: Option<CaptureProvider>,
    ) {
        self.auxiliary_black_capture_provider = black;
        self.auxiliary_exact_capture_provider = exact;
    }

    pub fn set_auxiliary_exact_capture_routes(
        &mut self,
        routes: std::collections::BTreeMap<String, CaptureProvider>,
    ) {
        self.auxiliary_exact_capture_routes = routes;
    }

    pub fn set_borrowed_capture_verification(&mut self, selected: bool) {
        self.borrowed_capture_verification = selected;
    }

    pub fn session_presentation_context(&self, client: Client) -> Context {
        let game = self
            .records
            .values()
            .find(|record| {
                record.auxiliary.is_none()
                    && record.application_session.is_none()
                    && (!record.revoked || record.state == State::Paused)
                    && matches!(record.state, State::Ready | State::Paused)
            })
            .map(|record| Game {
                application: Application {
                    id: record.request.app_id.to_string(),
                    uuid: String::new(),
                    title: if record.desktop {
                        "Desktop"
                    } else {
                        "Application"
                    }
                    .into(),
                    aliases: Vec::new(),
                    visible: true,
                    monitor_profile: false,
                    properties: serde_json::json!({}),
                },
                owner_client_id: record.application_owner_client_id.clone(),
                generation: record.id,
                source: record
                    .application_source
                    .as_ref()
                    .map(|hold| hold.source.clone()),
            });
        Context { client, game }
    }

    pub fn launch_auxiliary(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        plan: AuxiliaryStreamPlan,
        now: Instant,
    ) -> Option<Reply> {
        if !self.borrowed_capture_verification {
            return Some(private_error(
                503,
                "session presentation provider is not selected",
            ));
        }
        if plan.validate().is_err()
            || plan.mode.width != request.width
            || plan.mode.height != request.height
            || plan.mode.refresh_millihz != request.fps.saturating_mul(1000)
            || plan.mode.hdr != request.hdr
        {
            return Some(private_error(400, "invalid auxiliary stream plan"));
        }
        let client = fingerprint(peer.certificate());
        if self.pending_auxiliary.contains_key(&client) {
            return Some(private_error(
                409,
                "auxiliary reconnect is already in progress",
            ));
        }
        if self.records.values().any(|record| {
            record.auxiliary.is_some()
                && !record.state.terminal()
                && record.peer.certificate() == peer.certificate()
                && record.request.remote_input_key != request.remote_input_key
        }) {
            if let Err(code) =
                self.stop_auxiliary_for_client(&plan.session_id, plan.generation, &client, now)
            {
                return Some(private_error(409, code));
            }
            self.pending_auxiliary.insert(
                client,
                PendingAuxiliary {
                    connection,
                    request,
                    peer,
                    plan,
                    delivery_permission: crate::wire::Permission::Launch,
                },
            );
            return None;
        }
        self.launch_with_source(connection, request, peer, Some(plan), None, now)
    }

    /// Launch a normal application on an exact source already retained by a
    /// presentation provider. This is an explicit decision, never a fallback.
    pub fn launch_application_with_source(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        source: SharedCapture,
        now: Instant,
    ) -> Option<Reply> {
        if source.validate().is_err() {
            return Some(private_error(400, "invalid shared capture source"));
        }
        if !self.borrowed_capture_verification {
            return Some(private_error(
                503,
                "borrowed capture verification is not selected",
            ));
        }
        if !self.session_lifecycle_required || !self.session_lifecycle_capture_mode.is_external() {
            return Some(private_error(
                503,
                "borrowed capture requires external lifecycle",
            ));
        }
        self.launch_with_source(connection, request, peer, None, Some(source), now)
    }

    pub fn resume_with_source(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: &PeerIdentity,
        source: SharedCapture,
        now: Instant,
    ) -> Option<Reply> {
        if source.validate().is_err() || !self.borrowed_capture_verification {
            return Some(private_error(
                503,
                "borrowed capture verification is not selected",
            ));
        }
        let Some(root) = self.shared_application(request.app_id) else {
            return Some(private_error(404, "running application was not found"));
        };
        let Some(previous) = self
            .records
            .get(&root)
            .and_then(|record| record.shared_capture.as_ref())
        else {
            return Some(private_error(
                409,
                "running application uses another capture source",
            ));
        };
        if previous.session_id != source.session_id
            || previous.generation != source.generation
            || previous.capture != source.capture
            || previous.mode != source.mode
        {
            return Some(private_error(
                409,
                "borrowed capture source generation changed",
            ));
        }
        if self.pending_presentation_observations.len() >= super::MAX_RECORDS {
            return Some(private_error(503, "source observation capacity exhausted"));
        }
        // Retire the previous transport's exact source hold before replacing
        // its in-memory reservation on a paused application root.
        let previous_retirement = self.records.get_mut(&root).and_then(|record| {
            if record.rtsp_requested || record.source_ended_reported {
                return None;
            }
            let previous = record.shared_capture.as_ref()?;
            if record
                .application_source
                .as_ref()
                .is_some_and(|hold| hold.source.reservation_id == previous.reservation_id)
            {
                return None;
            }
            record.source_ended_reported = true;
            Some(Observation {
                client_id: fingerprint(record.peer.certificate()),
                session_id: previous.session_id.clone(),
                generation: previous.generation,
                reservation_id: previous.reservation_id.clone(),
                transport_id: record.rtsp_operation.clone(),
                transport_epoch: record.rtsp_runtime_epoch.clone(),
                event: if record.source_started_reported {
                    SessionEvent::StreamEnded
                } else {
                    SessionEvent::FailedStart
                },
            })
        });
        if let Some(observation) = previous_retirement {
            self.pending_presentation_observations.push(observation);
        }
        // The fresh reservation is frozen on the candidate created by the
        // ordinary resume admission. Native stream effects follow verification.
        let reply = self.resume(connection, request, peer, now);
        if reply.is_none()
            && let Some(record) = self
                .records
                .values_mut()
                .find(|record| record.launch_connection == Some(connection))
        {
            record.shared_capture = Some(source);
            record.presentation_client_id = Some(fingerprint(peer.certificate()));
            record.presentation_continue_stage = record.stage;
            record.stage = super::record::Stage::ReadRtspRuntimeEpoch;
            record.next_monitor = now;
            record.borrowed_capture_verified = false;
            record.presentation_transport_bound = false;
            record.rtsp_runtime_epoch = None;
            record.source_started_reported = false;
            record.source_ended_reported = false;
        }
        reply
    }

    pub(super) fn presentation_capture_route(
        &self,
        source: &CaptureSource,
        request: &LaunchRequest,
    ) -> Result<(CaptureProvider, bool, String), &'static str> {
        match source {
            CaptureSource::SyntheticBlack => {
                let provider = self
                    .auxiliary_black_capture_provider
                    .clone()
                    .ok_or("synthetic capture provider is not selected")?;
                // The selected black source exposes NV12 input. The encoder
                // independently negotiates its output format; no real output
                // is captured if the requested encoding cannot be served.
                Ok((
                    provider,
                    false,
                    format!(
                        "black:{}x{}@{}:nv12",
                        request.width, request.height, request.fps
                    ),
                ))
            }
            CaptureSource::ExactOutput { output, route_key } => {
                if output.is_empty()
                    || output.len() > 512
                    || output.chars().any(char::is_control)
                    || output == super::OWNED_DISPLAY_OUTPUT
                    || output == super::PORTAL_SELECTED_SESSION_SOURCE
                {
                    return Err("invalid exact capture output");
                }
                let provider = if route_key
                    == amaterasu_module_sdk::session_presentation::DEFAULT_CAPTURE_ROUTE
                {
                    self.auxiliary_exact_capture_provider.clone()
                } else {
                    self.auxiliary_exact_capture_routes.get(route_key).cloned()
                }
                .ok_or("exact capture route is not selected")?;
                // Unlike ordinary user-selected capture, this route preserves
                // the provider's exact selector without a portal substitution.
                Ok((provider, false, output.clone()))
            }
        }
    }

    /// Admission does not report cleanup complete. Call `auxiliary_stopped`
    /// until the exact transports have retired before releasing their source.
    pub fn stop_auxiliary(
        &mut self,
        session_id: &str,
        generation: u64,
        now: Instant,
    ) -> Result<(), &'static str> {
        self.check_auxiliary_generation(session_id, generation)?;
        let ids: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                record.auxiliary.as_ref().is_some_and(|auxiliary| {
                    auxiliary.plan.session_id == session_id
                        && auxiliary.plan.generation == generation
                }) && !record.state.terminal()
            })
            .map(|record| record.id)
            .collect();
        for id in ids {
            self.end_application_group(id, now);
        }
        Ok(())
    }

    pub fn auxiliary_stopped(
        &self,
        session_id: &str,
        generation: u64,
    ) -> Result<bool, &'static str> {
        self.check_auxiliary_generation(session_id, generation)?;
        for record in self.records.values().filter(|record| {
            record.auxiliary.as_ref().is_some_and(|auxiliary| {
                auxiliary.plan.session_id == session_id && auxiliary.plan.generation == generation
            })
        }) {
            if record.state == State::CleanupFailed {
                return Err("auxiliary cleanup is unconfirmed");
            }
            if !record.state.terminal() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn stop_auxiliary_for_client(
        &mut self,
        session_id: &str,
        generation: u64,
        client_id: &str,
        now: Instant,
    ) -> Result<(), &'static str> {
        self.check_auxiliary_owner(session_id, generation, client_id)?;
        self.stop_auxiliary(session_id, generation, now)
    }

    pub fn auxiliary_stopped_for_client(
        &self,
        session_id: &str,
        generation: u64,
        client_id: &str,
    ) -> Result<bool, &'static str> {
        self.check_auxiliary_owner(session_id, generation, client_id)?;
        self.auxiliary_stopped(session_id, generation)
    }

    fn check_auxiliary_owner(
        &self,
        session_id: &str,
        generation: u64,
        client_id: &str,
    ) -> Result<(), &'static str> {
        if !super::valid_fingerprint(client_id) {
            return Err("invalid authenticated client reference");
        }
        if self.records.values().any(|record| {
            record.auxiliary.as_ref().is_some_and(|auxiliary| {
                auxiliary.plan.session_id == session_id && auxiliary.plan.generation == generation
            }) && super::fingerprint(record.peer.certificate()) != client_id
        }) {
            return Err("auxiliary stream is owned by another client");
        }
        Ok(())
    }

    fn check_auxiliary_generation(
        &self,
        session_id: &str,
        generation: u64,
    ) -> Result<(), &'static str> {
        if !valid_source_reference(session_id, generation) {
            return Err("invalid auxiliary session reference");
        }
        if self.records.values().any(|record| {
            !record.state.terminal()
                && record.auxiliary.as_ref().is_some_and(|auxiliary| {
                    auxiliary.plan.session_id == session_id
                        && auxiliary.plan.generation != generation
                })
        }) {
            return Err("auxiliary session generation changed");
        }
        Ok(())
    }

    /// The wire consumer calls this only for a reauthorized provider decision.
    /// Application generation validation prevents a delayed decision from
    /// terminating a different application that has since occupied the slot.
    pub fn stop_presented_game(
        &mut self,
        peer: &PeerIdentity,
        application_id: &str,
        generation: u64,
        now: Instant,
    ) -> Result<(), &'static str> {
        if !peer.paired() {
            return Err("authenticated paired TLS is required");
        }
        let Some(record) = self.records.get(&generation).filter(|record| {
            record.auxiliary.is_none()
                && record.application_session.is_none()
                && !record.state.terminal()
                && record.request.app_id.to_string() == application_id
        }) else {
            return Err("active application generation changed");
        };
        self.end_application_group(record.id, now);
        Ok(())
    }

    pub fn presented_game_stopped(
        &self,
        application_id: &str,
        generation: u64,
    ) -> Result<bool, &'static str> {
        let Some(record) = self.records.get(&generation) else {
            return Ok(true);
        };
        if record.auxiliary.is_some()
            || record.application_session.is_some()
            || record.request.app_id.to_string() != application_id
        {
            return Err("active application generation changed");
        }
        if record.state == State::CleanupFailed {
            return Err("application cleanup is unconfirmed");
        }
        Ok(record.state.terminal())
    }

    /// Events are emitted once per actual transport. Failed or uncertain
    /// cleanup never fabricates a StreamEnded acknowledgement.
    pub fn take_auxiliary_events(&mut self) -> Vec<Observation> {
        let mut observations = std::mem::take(&mut self.pending_presentation_observations);
        for record in self.records.values_mut() {
            let (session_id, generation, reservation_id, started_reported, ended_reported) =
                match record.auxiliary.as_mut() {
                    Some(auxiliary) => (
                        &auxiliary.plan.session_id,
                        auxiliary.plan.generation,
                        &auxiliary.plan.reservation_id,
                        &mut auxiliary.started_reported,
                        &mut auxiliary.ended_reported,
                    ),
                    None => match record.shared_capture.as_ref() {
                        Some(source) => (
                            &source.session_id,
                            source.generation,
                            &source.reservation_id,
                            &mut record.source_started_reported,
                            &mut record.source_ended_reported,
                        ),
                        None => continue,
                    },
                };
            let application_root_hold = record
                .application_source
                .as_ref()
                .is_some_and(|hold| hold.source.reservation_id == *reservation_id);
            let stream_retired = record.state.terminal()
                || (!application_root_hold
                    && record.shared_capture.is_some()
                    && record.state == State::Paused
                    && !record.rtsp_requested);
            let event = if stream_retired && !*ended_reported {
                *ended_reported = true;
                Some(if !*started_reported {
                    SessionEvent::FailedStart
                } else if record.authenticated_client_disconnected {
                    SessionEvent::ClientDisconnected
                } else {
                    SessionEvent::StreamEnded
                })
            } else if record.state == State::Ready && record.adopted && !*started_reported {
                *started_reported = true;
                Some(SessionEvent::StreamStarted)
            } else {
                None
            };
            if let Some(event) = event {
                if application_root_hold && let Some(hold) = record.application_source.as_mut() {
                    if event == SessionEvent::StreamStarted {
                        hold.started_reported = true;
                    }
                    if matches!(
                        event,
                        SessionEvent::StreamEnded
                            | SessionEvent::FailedStart
                            | SessionEvent::ClientDisconnected
                    ) {
                        hold.ended_reported = true;
                    }
                }
                observations.push(Observation {
                    client_id: fingerprint(record.peer.certificate()),
                    session_id: session_id.clone(),
                    generation,
                    reservation_id: reservation_id.clone(),
                    transport_id: record.rtsp_operation.clone(),
                    transport_epoch: record.rtsp_runtime_epoch.clone(),
                    event,
                });
            }
            if record.state.terminal()
                && let Some(hold) = record.application_source.as_mut()
                && !hold.ended_reported
            {
                hold.ended_reported = true;
                observations.push(Observation {
                    client_id: hold.client_id.clone(),
                    session_id: hold.source.session_id.clone(),
                    generation: hold.source.generation,
                    reservation_id: hold.source.reservation_id.clone(),
                    transport_id: hold.transport_id.clone(),
                    transport_epoch: hold.transport_epoch.clone(),
                    event: if hold.started_reported {
                        SessionEvent::StreamEnded
                    } else {
                        SessionEvent::FailedStart
                    },
                });
            }
        }
        observations
    }

    pub fn set_presentation_delivery_permission(
        &mut self,
        connection: u64,
        permission: crate::wire::Permission,
    ) {
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.launch_connection == Some(connection))
        {
            record.delivery_permission = permission;
        }
        for pending in self
            .pending_auxiliary
            .values_mut()
            .filter(|pending| pending.connection == connection)
        {
            pending.delivery_permission = permission;
        }
    }

    pub(super) fn advance_auxiliary_reconnects(
        &mut self,
        now: Instant,
        allowed: &mut impl FnMut(&[u8], crate::wire::Permission) -> bool,
        replies: &mut Vec<(u64, Reply)>,
    ) {
        let clients: Vec<_> = self.pending_auxiliary.keys().cloned().collect();
        for client in clients {
            let pending = &self.pending_auxiliary[&client];
            let outcome = self.auxiliary_stopped(&pending.plan.session_id, pending.plan.generation);
            if matches!(outcome, Ok(false)) {
                continue;
            }
            let pending = self
                .pending_auxiliary
                .remove(&client)
                .expect("pending reconnect");
            if let Err(code) = outcome {
                replies.push((pending.connection, private_error(503, code)));
            } else if !allowed(pending.peer.certificate(), pending.delivery_permission) {
                replies.push((
                    pending.connection,
                    private_error(401, "paired client is no longer trusted"),
                ));
            } else {
                if let Some(reply) = self.launch_auxiliary(
                    pending.connection,
                    pending.request,
                    pending.peer,
                    pending.plan,
                    now,
                ) {
                    replies.push((pending.connection, reply));
                }
                self.set_presentation_delivery_permission(
                    pending.connection,
                    pending.delivery_permission,
                );
            }
        }
    }
}

fn valid_source_reference(session_id: &str, generation: u64) -> bool {
    generation != 0
        && !session_id.is_empty()
        && session_id.len() <= 128
        && !session_id.chars().any(char::is_control)
}

pub(super) fn prepare_auxiliary(
    record: &mut Record,
    profile: &StreamProfile,
) -> Result<(), &'static str> {
    let plan = record
        .auxiliary
        .as_ref()
        .expect("auxiliary record")
        .plan
        .clone();
    let mut profile = profile.clone();
    project_application_policy(record, &ApplicationConfigOverrides::default(), &mut profile)?;
    record.effective_stream_audio = plan.stream_audio;
    record.capture_sink_without_routing = true;
    record.resolved_virtual_sink = None;
    if !plan.stream_audio {
        record.audio_endpoint.clear();
    }
    record.application_display_overrides = None;
    record.application_display_settings = None;
    record.display_policy_snapshot = None;
    record.virtual_display_intent = None;
    record.display_request = None;
    record.display_admission_token = None;
    Ok(())
}
