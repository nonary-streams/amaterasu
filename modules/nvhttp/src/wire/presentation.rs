//! Authenticated protocol adapter for the startup-selected presentation API.
//!
//! Product identities and remote-session policy stay in the selected provider.
//! This adapter owns bounded broker work, protocol conversion, live permission
//! checks and the distinction between accepting cleanup and completing cleanup.
mod applications;
mod recovery;

use super::{Permission, Purpose, Reply, RequestContext, Router, RouterConfig, xml};
use crate::launch::{Controller, LaunchRequest, PeerIdentity};
use amaterasu_module_sdk::requests::RequestOptions;
use amaterasu_module_sdk::session_presentation::{
    self as session, Artwork, Client, Context, ControlCompletion, Decision, DispatchIntent,
    Observation, Request, SessionEvent, SharedCapture,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use applications::{CatalogProjection, ProjectionCall};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const MAX_JOBS: usize = 16;
const MAX_KNOWN_CLIENTS: usize = 64;
const WORK_TIMEOUT: Duration = Duration::from_secs(40);
const CLEANUP_RETRY: Duration = Duration::from_millis(100);
const LIBRARY_SERVICE: &str = "amaterasu.library/1";

pub(super) struct PresentationRouter {
    jobs: BTreeMap<u64, Work>,
    next_job: u64,
    next_call: u64,
    operation_prefix: String,
    quiescing: bool,
    pending_observations: VecDeque<Observation>,
    known_certificates: BTreeMap<String, Vec<u8>>,
    pending_revocations: BTreeSet<String>,
    revocations_inflight: BTreeSet<String>,
    activation_pending: bool,
    recovery: recovery::RecoveryPump,
}

struct Work {
    connection: Option<u64>,
    certificate: Vec<u8>,
    context: Context,
    deadline: Instant,
    not_before: Instant,
    call: Option<BrokerCall>,
    stage: Stage,
    cancelled: bool,
    dispatch_operation: Option<String>,
    intent: Option<DispatchIntent>,
    completed: bool,
    list_required: bool,
    cancellation_reply: Option<Reply>,
}

pub(super) struct DispatchSubmission {
    pub connection: u64,
    pub certificate: Vec<u8>,
    pub context: Context,
    pub peer: PeerIdentity,
    pub request: Option<LaunchRequest>,
    pub intent: DispatchIntent,
}

enum Stage {
    Catalog {
        projection: Box<CatalogProjection>,
        then: Continuation,
    },
    Info {
        host_id: String,
    },
    Dispatch {
        peer: Box<PeerIdentity>,
        request: Option<LaunchRequest>,
        operation_id: String,
    },
    Artwork {
        selection: session::Selection,
    },
    LibraryArtwork {
        application_id: String,
    },
    AwaitStop {
        target: StopTarget,
        completion: Completion,
    },
    Observe {
        observation: Observation,
        completion: Option<Reply>,
    },
    Release {
        request: Box<Request>,
        completion: Option<Reply>,
    },
}

enum Continuation {
    Catalog,
    Dispatch {
        peer: Box<PeerIdentity>,
        request: Option<LaunchRequest>,
        operation_id: String,
    },
    Artwork {
        selection: session::Selection,
    },
}

enum StopTarget {
    Application {
        application_id: String,
        generation: u64,
    },
    Auxiliary {
        session_id: String,
        generation: u64,
    },
}

enum Completion {
    Control(ControlCompletion),
    Replace {
        peer: Box<PeerIdentity>,
        request: LaunchRequest,
        source: Option<SharedCapture>,
    },
}

enum CallSpec {
    Provider(Box<Request>),
    Library(Value),
}

impl CallSpec {
    fn provider(request: Request) -> Self {
        Self::Provider(Box::new(request))
    }
}

struct BrokerCall {
    command: Command,
    request: Option<u64>,
    cancelling: bool,
}

enum CallOutcome {
    Waiting,
    Result(Value),
    Rejected,
    Retired,
}

impl PresentationRouter {
    pub(super) fn bind(runtime: &RuntimeClient, selected: &str) -> ModuleResult<Self> {
        if selected != session::SERVICE {
            return Err("incompatible_session_presentation_service".into());
        }
        for expected in session::methods() {
            let Response::Result { payload } = runtime.send(Command::QueryApi {
                service: selected.into(),
                method: expected.method.clone(),
                interface_major: expected.interface_major,
                min_minor: expected.interface_minor,
            })?
            else {
                return Err("session_presentation_api_unavailable".into());
            };
            if payload != serde_json::to_value(expected).map_err(|error| error.to_string())? {
                return Err("session_presentation_api_identity_mismatch".into());
            }
        }
        Self::new()
    }

    fn new() -> ModuleResult<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "session_presentation_identity_unavailable")?;
        Ok(Self {
            jobs: BTreeMap::new(),
            next_job: 0,
            next_call: 0,
            operation_prefix: nonce.iter().map(|byte| format!("{byte:02x}")).collect(),
            quiescing: false,
            pending_observations: VecDeque::new(),
            known_certificates: BTreeMap::new(),
            pending_revocations: BTreeSet::new(),
            revocations_inflight: BTreeSet::new(),
            activation_pending: false,
            recovery: recovery::RecoveryPump::new()?,
        })
    }

    pub(super) fn client(peer: &RequestContext) -> Result<Client, Reply> {
        if !peer.trusted() {
            return Err(xml::error(401, "authenticated paired TLS is required"));
        }
        let certificate = peer
            .peer_certificate
            .as_deref()
            .expect("trusted certificate");
        Ok(Client {
            id: fingerprint(certificate),
            paired: true,
            may_list: peer.list_allowed,
            may_view: peer.view_allowed,
            may_launch: peer.launch_allowed,
            may_terminate: peer.terminate_allowed,
        })
    }

    fn queue(
        &mut self,
        connection: Option<u64>,
        certificate: Vec<u8>,
        context: Context,
        stage: Stage,
        intent: Option<DispatchIntent>,
        now: Instant,
    ) -> Result<u64, Reply> {
        if self.quiescing
            || self.activation_pending
            || self.jobs.len() >= MAX_JOBS
            || self.pending_observations.len() >= MAX_JOBS
        {
            return Err(xml::error(503, "session presentation capacity unavailable"));
        }
        if !certificate.is_empty() {
            self.remember_client(&certificate)?;
        }
        self.next_job = self
            .next_job
            .checked_add(1)
            .ok_or_else(|| xml::error(503, "session presentation identifiers exhausted"))?;
        self.jobs.insert(
            self.next_job,
            Work {
                connection,
                certificate,
                context,
                stage,
                intent,
                deadline: now + WORK_TIMEOUT,
                not_before: now,
                call: None,
                cancelled: false,
                dispatch_operation: None,
                completed: false,
                list_required: false,
                cancellation_reply: None,
            },
        );
        Ok(self.next_job)
    }

    pub(super) fn info(
        &mut self,
        connection: u64,
        certificate: Vec<u8>,
        context: Context,
        host_id: String,
        now: Instant,
    ) -> Option<Reply> {
        self.queue(
            Some(connection),
            certificate,
            context,
            Stage::Info { host_id },
            None,
            now,
        )
        .err()
    }

    pub(super) fn applications(
        &mut self,
        connection: u64,
        certificate: Vec<u8>,
        context: Context,
        launches: &Controller,
        now: Instant,
    ) -> Option<Reply> {
        let projection = projection(context.clone(), launches);
        match self.queue(
            Some(connection),
            certificate,
            context,
            Stage::Catalog {
                projection,
                then: Continuation::Catalog,
            },
            None,
            now,
        ) {
            Ok(id) => {
                self.jobs
                    .get_mut(&id)
                    .expect("queued catalog")
                    .list_required = true;
                None
            }
            Err(reply) => Some(reply),
        }
    }

    pub(super) fn dispatch(
        &mut self,
        submission: DispatchSubmission,
        launches: &Controller,
        now: Instant,
    ) -> Option<Reply> {
        let DispatchSubmission {
            connection,
            certificate,
            context,
            peer,
            request,
            intent,
        } = submission;
        let operation_id = format!("session-{}-{}", self.operation_prefix, self.next_job + 1);
        let projection = inventory(context.clone(), launches);
        self.queue(
            Some(connection),
            certificate,
            context,
            Stage::Catalog {
                projection,
                then: Continuation::Dispatch {
                    peer: Box::new(peer),
                    request,
                    operation_id,
                },
            },
            Some(intent),
            now,
        )
        .err()
    }

    pub(super) fn artwork(
        &mut self,
        connection: u64,
        certificate: Vec<u8>,
        context: Context,
        selection: session::Selection,
        launches: &Controller,
        now: Instant,
    ) -> Option<Reply> {
        let projection = inventory(context.clone(), launches);
        match self.queue(
            Some(connection),
            certificate,
            context,
            Stage::Catalog {
                projection,
                then: Continuation::Artwork { selection },
            },
            None,
            now,
        ) {
            Ok(id) => {
                self.jobs
                    .get_mut(&id)
                    .expect("queued artwork")
                    .list_required = true;
                None
            }
            Err(reply) => Some(reply),
        }
    }

    pub(super) fn poll(
        &mut self,
        launches: &mut Controller,
        config: &RouterConfig,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
        allowed: &mut impl FnMut(&[u8], Permission) -> bool,
        receive: &mut impl FnMut(Value) -> ModuleResult<amaterasu_module_sdk::artwork_transfer::Image>,
    ) -> Vec<(u64, Reply)> {
        let mut replies = Vec::new();
        self.recovery.poll(now, send, |binding| match binding {
            session::RecoveryBinding::Transport {
                operation_id,
                epoch,
            } => !launches.owns_transport_operation(operation_id, epoch),
            session::RecoveryBinding::Application {
                operation_id,
                epoch,
                ..
            } => !launches.owns_application_operation(operation_id, epoch),
        });
        self.pending_observations
            .extend(launches.take_auxiliary_events());
        self.admit_observations(now);
        self.admit_revocations(now);
        let keys: Vec<_> = self.jobs.keys().copied().collect();
        for key in keys {
            let Some(mut work) = self.jobs.remove(&key) else {
                continue;
            };
            if now < work.not_before {
                self.jobs.insert(key, work);
                continue;
            }
            let cleanup = matches!(
                work.stage,
                Stage::Observe { .. } | Stage::Release { .. } | Stage::AwaitStop { .. }
            );
            let cancelled = if now >= work.deadline {
                Some(xml::error(504, "session request expired"))
            } else if self.quiescing || self.activation_pending {
                Some(xml::error(503, "host is stopping"))
            } else if !permissions_unchanged(&work, allowed) {
                Some(xml::error(403, "session authorization changed"))
            } else {
                None
            };
            if let Some(reply) = cancelled {
                if matches!(work.stage, Stage::AwaitStop { .. }) {
                    work.cancellation_reply = Some(reply);
                } else if !cleanup {
                    work.cancelled = true;
                    work.cancellation_reply = Some(reply);
                }
            }
            if work.cancelled {
                if let Some(call) = &mut work.call {
                    call.cancelling = true;
                    if matches!(call.poll(send), CallOutcome::Waiting) {
                        self.jobs.insert(key, work);
                        continue;
                    }
                    work.call = None;
                }
                let release = match &work.stage {
                    Stage::Catalog { projection, .. } => projection.release_request(),
                    _ => work.dispatch_operation.as_ref().map(|operation_id| {
                        Request::CancelDispatch {
                            client_id: work.context.client.id.clone(),
                            operation_id: operation_id.clone(),
                        }
                    }),
                };
                if let Some(request) = release {
                    work.stage = Stage::Release {
                        request: Box::new(request),
                        completion: work.cancellation_reply.take(),
                    };
                    work.cancelled = false;
                } else {
                    if let Some(connection) = work.connection {
                        replies.push((
                            connection,
                            work.cancellation_reply
                                .take()
                                .unwrap_or_else(|| xml::error(503, "session request ended")),
                        ));
                    }
                    continue;
                }
            }
            if let Stage::AwaitStop { target, .. } = &work.stage {
                let stopped = match target {
                    StopTarget::Application {
                        application_id,
                        generation,
                    } => launches.presented_game_stopped(application_id, *generation),
                    StopTarget::Auxiliary {
                        session_id,
                        generation,
                    } => launches.auxiliary_stopped_for_client(
                        session_id,
                        *generation,
                        &work.context.client.id,
                    ),
                };
                match stopped {
                    Ok(false) => {
                        work.not_before = now + CLEANUP_RETRY;
                        self.jobs.insert(key, work);
                        continue;
                    }
                    Err(_) => {
                        finish(
                            &work,
                            xml::error(503, "session cleanup could not be verified"),
                            &mut replies,
                        );
                        work.connection = None;
                        work.not_before = now + CLEANUP_RETRY;
                        self.jobs.insert(key, work);
                        continue;
                    }
                    Ok(true) => {
                        if let Some(reply) = self.complete_stop(&mut work, launches, now) {
                            finish(&work, reply, &mut replies);
                            continue;
                        }
                        if !matches!(work.stage, Stage::Observe { .. } | Stage::Release { .. })
                            || work.completed
                        {
                            continue;
                        }
                    }
                }
            }
            if work.call.is_none() {
                match self.command(&work, now) {
                    Ok(command) => {
                        work.call = Some(BrokerCall {
                            command,
                            request: None,
                            cancelling: false,
                        })
                    }
                    Err(reply) => {
                        finish(&work, reply, &mut replies);
                        continue;
                    }
                }
            }
            let outcome = work.call.as_mut().expect("admitted call").poll(send);
            let result = match outcome {
                CallOutcome::Waiting => {
                    self.jobs.insert(key, work);
                    continue;
                }
                CallOutcome::Result(payload) => Ok(payload),
                CallOutcome::Rejected | CallOutcome::Retired => Err(()),
            };
            work.call = None;
            if !cleanup && !permissions_unchanged(&work, allowed) {
                work.cancelled = true;
                work.cancellation_reply = Some(xml::error(403, "session authorization changed"));
                self.jobs.insert(key, work);
                continue;
            }
            if let Some(reply) = self.accept(&mut work, result, launches, config, now, receive) {
                finish(&work, reply, &mut replies);
            } else if !work.completed
                && (work.connection.is_some()
                    || matches!(work.stage, Stage::Observe { .. } | Stage::Release { .. }))
            {
                self.jobs.insert(key, work);
            }
        }
        replies
    }

    fn command(&mut self, work: &Work, now: Instant) -> Result<Command, Reply> {
        self.next_call = self
            .next_call
            .checked_add(1)
            .ok_or_else(|| xml::error(503, "session request identifiers exhausted"))?;
        let operation_id = format!("session-api-{}-{}", self.operation_prefix, self.next_call);
        let spec = match &work.stage {
            Stage::Catalog { projection, .. } => match projection.next_call() {
                ProjectionCall::Provider(request) => CallSpec::Provider(request),
                ProjectionCall::Library(payload) => CallSpec::Library(payload),
            },
            Stage::Info { .. } => CallSpec::provider(Request::Info {
                context: work.context.clone(),
            }),
            Stage::Dispatch {
                request,
                operation_id,
                ..
            } => CallSpec::provider(Request::Dispatch {
                context: work.context.clone(),
                operation_id: operation_id.clone(),
                intent: work.intent.expect("dispatch intent"),
                selection: request.as_ref().map(|request| session::Selection {
                    id: request.app_id.to_string(),
                    uuid: None,
                }),
                mode: request.as_ref().map(|request| session::Mode {
                    width: request.width,
                    height: request.height,
                    refresh_millihz: request.fps.saturating_mul(1000),
                    hdr: request.hdr,
                }),
            }),
            Stage::Artwork { selection } => CallSpec::provider(Request::Artwork {
                context: work.context.clone(),
                selection: selection.clone(),
            }),
            Stage::LibraryArtwork { application_id } => CallSpec::Library(json!({
                "action":"artwork_open", "id":format!("app-{:016x}", numeric_id(application_id)?),
            })),
            Stage::Observe { observation, .. } => CallSpec::provider(Request::Observe {
                client_id: observation.client_id.clone(),
                session_id: observation.session_id.clone(),
                generation: observation.generation,
                transport_id: observation.transport_id.clone(),
                event: observation.event,
                reservation_id: observation.reservation_id.clone(),
                transport_epoch: observation.transport_epoch.clone(),
            }),
            Stage::Release { request, .. } => CallSpec::Provider(request.clone()),
            Stage::AwaitStop { .. } => return Err(xml::error(500, "invalid session work phase")),
        };
        let remaining = if matches!(work.stage, Stage::Observe { .. } | Stage::Release { .. }) {
            WORK_TIMEOUT
        } else {
            work.deadline.saturating_duration_since(now)
        };
        let timeout_ms = remaining.min(WORK_TIMEOUT).as_millis().clamp(1, 40_000) as u64;
        match spec {
            CallSpec::Provider(request) => session::command(
                RequestOptions {
                    service: session::SERVICE,
                    timeout_ms,
                    operation_id: &operation_id,
                    parent: None,
                },
                &request,
            )
            .map_err(|_| xml::error(502, "invalid presentation contract request")),
            CallSpec::Library(payload) => Ok(Command::Request {
                service: LIBRARY_SERVICE.into(),
                payload,
                timeout_ms,
                operation_id,
                parent: None,
            }),
        }
    }

    fn accept(
        &mut self,
        work: &mut Work,
        result: Result<Value, ()>,
        launches: &mut Controller,
        config: &RouterConfig,
        now: Instant,
        receive: &mut impl FnMut(Value) -> ModuleResult<amaterasu_module_sdk::artwork_transfer::Image>,
    ) -> Option<Reply> {
        if let Stage::Catalog { projection, then } = &mut work.stage {
            let payload = match result {
                Ok(payload) => payload,
                Err(()) => {
                    return self.fail_with_release(
                        work,
                        xml::error(503, "catalog presentation unavailable"),
                    );
                }
            };
            match projection.accept(payload) {
                Ok(None) => return None,
                Err(reply) => return self.fail_with_release(work, reply),
                Ok(Some(reply)) => {
                    work.context = projection.context().clone();
                    if !context_current(&work.context, launches) {
                        return Some(xml::error(409, "active application changed; request again"));
                    }
                    let continuation = std::mem::replace(then, Continuation::Catalog);
                    match continuation {
                        Continuation::Catalog => return Some(reply),
                        Continuation::Dispatch {
                            peer,
                            request,
                            operation_id,
                        } => {
                            work.dispatch_operation = Some(operation_id.clone());
                            work.stage = Stage::Dispatch {
                                peer,
                                request,
                                operation_id,
                            };
                            return None;
                        }
                        Continuation::Artwork { selection } => {
                            work.stage = Stage::Artwork { selection };
                            return None;
                        }
                    }
                }
            }
        }
        if let Stage::LibraryArtwork { .. } = work.stage {
            let image = result
                .map_err(|()| "artwork unavailable".into())
                .and_then(receive);
            return Some(match image {
                Ok(image) => {
                    let content_type = match image.mime_type.as_str() {
                        "image/png" => "image/png",
                        "image/jpeg" => "image/jpeg",
                        "image/webp" => "image/webp",
                        _ => return Some(xml::error(404, "artwork unavailable")),
                    };
                    private_reply(
                        Reply {
                            status: 200,
                            content_type,
                            body: image.bytes,
                            requires_paired_tls: true,
                            permission: Some(Permission::List),
                        },
                        Permission::List,
                    )
                }
                Err(_) => xml::error(404, "artwork unavailable"),
            });
        }
        let response: session::Reply = match result
            .ok()
            .and_then(|payload| session::decode_reply(payload).ok())
        {
            Some(response) => response,
            None => {
                if matches!(work.stage, Stage::Observe { .. } | Stage::Release { .. }) {
                    work.not_before = now + CLEANUP_RETRY;
                    return None;
                }
                return self
                    .fail_with_release(work, xml::error(503, "presentation provider unavailable"));
            }
        };
        if let session::Reply::Pending { retry_after_ms } = response {
            if !(1..=1000).contains(&retry_after_ms) {
                return self.fail_with_release(
                    work,
                    xml::error(502, "invalid presentation retry interval"),
                );
            }
            work.not_before = now + Duration::from_millis(retry_after_ms);
            return None;
        }
        let revoked_client = match &work.stage {
            Stage::Release { request, .. } => match request.as_ref() {
                Request::RevokeClient { client_id } => Some(client_id.clone()),
                _ => None,
            },
            _ => None,
        };
        match (&mut work.stage, response) {
            (Stage::Info { host_id }, session::Reply::Info { presentation }) => {
                if !context_current(&work.context, launches) {
                    return Some(xml::error(409, "active application changed; request again"));
                }
                let current_game = match presentation
                    .current_game
                    .as_deref()
                    .map(numeric_id)
                    .transpose()
                {
                    Ok(current) => current.unwrap_or(0),
                    Err(reply) => return Some(reply),
                };
                let mut reply = xml::server_info(
                    config,
                    host_id,
                    true,
                    current_game,
                    presentation.busy,
                    launches.supported(),
                    launches.codec_advertisement(),
                );
                if let Some(uuid) = presentation.current_game_uuid {
                    if uuid.len() > 128 || uuid.chars().any(char::is_control) {
                        return Some(xml::error(502, "invalid projected application UUID"));
                    }
                    let body = String::from_utf8(reply.body).expect("XML is UTF-8");
                    reply.body = body
                        .replace(
                            "</root>",
                            &format!(
                                "<currentgameuuid>{}</currentgameuuid></root>",
                                xml::escape(&uuid)
                            ),
                        )
                        .into_bytes();
                }
                reply.requires_paired_tls = true;
                Some(reply)
            }
            (Stage::Dispatch { peer, request, .. }, session::Reply::Dispatched { decision }) => {
                if !context_current(&work.context, launches) {
                    return self.fail_with_release(
                        work,
                        xml::error(409, "active application changed; request again"),
                    );
                }
                let peer = peer.as_ref().clone();
                let request = request.clone();
                self.apply_decision(work, launches, peer, request, decision, now)
            }
            (Stage::Artwork { .. }, session::Reply::Artwork { artwork }) => match artwork {
                Artwork::Application { application_id } => {
                    if let Err(reply) = numeric_id(&application_id) {
                        return Some(reply);
                    }
                    work.stage = Stage::LibraryArtwork { application_id };
                    None
                }
                Artwork::InlinePng { bytes } => {
                    if bytes.len() > session::MAX_ARTWORK_BYTES
                        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                    {
                        return Some(xml::error(502, "invalid presentation artwork"));
                    }
                    Some(Reply {
                        status: 200,
                        content_type: "image/png",
                        body: bytes,
                        requires_paired_tls: true,
                        permission: Some(Permission::List),
                    })
                }
                Artwork::Unavailable => Some(xml::error(404, "artwork unavailable")),
            },
            (
                Stage::Observe { completion, .. } | Stage::Release { completion, .. },
                session::Reply::Acknowledged {},
            ) => {
                if let Some(client_id) = revoked_client {
                    self.known_certificates.remove(&client_id);
                    self.revocations_inflight.remove(&client_id);
                }
                work.completed = true;
                completion.take()
            }
            (Stage::Observe { .. } | Stage::Release { .. }, session::Reply::Error { .. }) => {
                work.not_before = now + CLEANUP_RETRY;
                None
            }
            (_, session::Reply::Error { message, .. }) => {
                self.fail_with_release(work, xml::error(503, &bounded_message(&message)))
            }
            _ => self.fail_with_release(
                work,
                xml::error(
                    502,
                    "presentation provider returned an incompatible response",
                ),
            ),
        }
    }

    fn apply_decision(
        &mut self,
        work: &mut Work,
        launches: &mut Controller,
        peer: PeerIdentity,
        request: Option<LaunchRequest>,
        decision: Decision,
        now: Instant,
    ) -> Option<Reply> {
        let connection = work.connection?;
        let auxiliary_permission = if matches!(&decision, Decision::ResumeAuxiliary { .. }) {
            Permission::View
        } else {
            Permission::Launch
        };
        match decision {
            Decision::LaunchApplication {
                application_id,
                source,
            } => {
                if !work.context.client.may_launch {
                    return self
                        .fail_with_release(work, xml::error(403, "launch permission denied"));
                }
                let Some(mut request) = request else {
                    return self.fail_with_release(
                        work,
                        xml::error(502, "missing application stream request"),
                    );
                };
                request.app_id = match numeric_id(&application_id) {
                    Ok(id) => id,
                    Err(reply) => return self.fail_with_release(work, reply),
                };
                let reply = match source {
                    Some(source) => launches
                        .launch_application_with_source(connection, request, peer, source, now),
                    None => launches.launch(connection, request, peer, now),
                };
                self.handoff_reply(work, reply)
            }
            Decision::ResumeApplication {
                application_id,
                source,
            } => {
                if !work.context.client.may_view {
                    return self.fail_with_release(work, xml::error(403, "view permission denied"));
                }
                let Some(mut request) = request else {
                    return self.fail_with_release(
                        work,
                        xml::error(502, "missing application stream request"),
                    );
                };
                request.app_id = match numeric_id(&application_id) {
                    Ok(id) => id,
                    Err(reply) => return self.fail_with_release(work, reply),
                };
                let reply = match source {
                    Some(source) => {
                        launches.resume_with_source(connection, request, &peer, source, now)
                    }
                    None => launches.resume(connection, request, &peer, now),
                };
                launches.set_presentation_delivery_permission(connection, Permission::View);
                self.handoff_reply(
                    work,
                    reply.map(|reply| private_reply(reply, Permission::View)),
                )
            }
            Decision::Auxiliary { plan } | Decision::ResumeAuxiliary { plan } => {
                let permitted = if auxiliary_permission == Permission::View {
                    work.context.client.may_view
                } else {
                    work.context.client.may_launch
                };
                if !permitted {
                    return self
                        .fail_with_release(work, xml::error(403, "session permission denied"));
                }
                if plan.validate().is_err() {
                    return self
                        .fail_with_release(work, xml::error(502, "invalid auxiliary stream plan"));
                }
                let Some(request) = request else {
                    return self.fail_with_release(
                        work,
                        xml::error(502, "missing auxiliary stream request"),
                    );
                };
                let permission = auxiliary_permission;
                let reply = launches.launch_auxiliary(connection, request, peer, plan, now);
                launches.set_presentation_delivery_permission(connection, permission);
                self.handoff_reply(work, reply.map(|reply| private_reply(reply, permission)))
            }
            Decision::StopApplication {
                application_id,
                generation,
                completion,
            } => {
                if !work.context.client.may_terminate {
                    return self
                        .fail_with_release(work, xml::error(403, "termination permission denied"));
                }
                if launches
                    .stop_presented_game(&peer, &application_id, generation, now)
                    .is_err()
                {
                    return self
                        .fail_with_release(work, xml::error(409, "active application changed"));
                }
                work.stage = Stage::AwaitStop {
                    target: StopTarget::Application {
                        application_id,
                        generation,
                    },
                    completion: Completion::Control(completion),
                };
                None
            }
            Decision::ReplaceApplication {
                application_id,
                expected_current_generation,
                source,
            } => {
                if !work.context.client.may_launch || !work.context.client.may_terminate {
                    return self.fail_with_release(
                        work,
                        xml::error(403, "application replacement permission denied"),
                    );
                }
                let Some(mut request) = request else {
                    return self.fail_with_release(
                        work,
                        xml::error(502, "missing replacement stream request"),
                    );
                };
                request.app_id = match numeric_id(&application_id) {
                    Ok(id) => id,
                    Err(reply) => return self.fail_with_release(work, reply),
                };
                let Some(game) = &work.context.game else {
                    return self
                        .fail_with_release(work, xml::error(409, "active application changed"));
                };
                if game.generation != expected_current_generation
                    || launches
                        .stop_presented_game(&peer, &game.application.id, game.generation, now)
                        .is_err()
                {
                    return self
                        .fail_with_release(work, xml::error(409, "active application changed"));
                }
                work.stage = Stage::AwaitStop {
                    target: StopTarget::Application {
                        application_id: game.application.id.clone(),
                        generation: game.generation,
                    },
                    completion: Completion::Replace {
                        peer: Box::new(peer),
                        request,
                        source,
                    },
                };
                None
            }
            Decision::StopAuxiliary {
                session_id,
                generation,
                completion,
            } => {
                if !work.context.client.may_terminate {
                    return self
                        .fail_with_release(work, xml::error(403, "termination permission denied"));
                }
                if launches
                    .stop_auxiliary_for_client(
                        &session_id,
                        generation,
                        &work.context.client.id,
                        now,
                    )
                    .is_err()
                {
                    return self
                        .fail_with_release(work, xml::error(409, "retained session changed"));
                }
                work.stage = Stage::AwaitStop {
                    target: StopTarget::Auxiliary {
                        session_id,
                        generation,
                    },
                    completion: Completion::Control(completion),
                };
                None
            }
            Decision::Complete {
                status_code,
                message,
            } => Some(control_reply(
                status_code,
                &message,
                work.intent,
                Permission::Paired,
            )),
            Decision::Reject {
                status_code,
                message,
            } => {
                let mut reply = xml::error(valid_status(status_code), &bounded_message(&message));
                reply.status = 200;
                Some(private_reply(reply, Permission::Paired))
            }
        }
    }

    fn complete_stop(
        &mut self,
        work: &mut Work,
        launches: &mut Controller,
        now: Instant,
    ) -> Option<Reply> {
        let stage = std::mem::replace(
            &mut work.stage,
            Stage::Release {
                request: Box::new(Request::Describe {}),
                completion: None,
            },
        );
        let Stage::AwaitStop { target, completion } = stage else {
            unreachable!()
        };
        match completion {
            Completion::Replace {
                peer,
                request,
                source,
            } => {
                if work.connection.is_none() || work.cancellation_reply.is_some() {
                    let reply = work
                        .cancellation_reply
                        .take()
                        .unwrap_or_else(|| xml::error(503, "replacement request ended"));
                    return self.fail_with_release(work, reply);
                }
                let connection = work.connection?;
                let reply = match source {
                    Some(source) => launches
                        .launch_application_with_source(connection, request, *peer, source, now),
                    None => launches.launch(connection, request, *peer, now),
                };
                self.handoff_reply(work, reply)
            }
            Completion::Control(completion) => {
                let reply = work.cancellation_reply.take().unwrap_or_else(|| {
                    control_reply(
                        completion.status_code,
                        &completion.message,
                        work.intent,
                        Permission::Terminate,
                    )
                });
                match target {
                    StopTarget::Auxiliary {
                        session_id,
                        generation,
                    } => {
                        work.stage = Stage::Observe {
                            observation: Observation {
                                client_id: work.context.client.id.clone(),
                                session_id,
                                generation,
                                transport_id: format!(
                                    "control-{}",
                                    work.dispatch_operation.as_deref().unwrap_or("disconnect")
                                ),
                                event: SessionEvent::ExplicitDisconnect,
                                reservation_id: work
                                    .dispatch_operation
                                    .clone()
                                    .unwrap_or_else(|| "disconnect".into()),
                                transport_epoch: None,
                            },
                            completion: Some(reply),
                        };
                        None
                    }
                    StopTarget::Application { .. } => Some(reply),
                }
            }
        }
    }

    fn fail_with_release(&mut self, work: &mut Work, reply: Reply) -> Option<Reply> {
        let release = match &work.stage {
            Stage::Catalog { projection, .. } => projection.release_request(),
            _ => work
                .dispatch_operation
                .as_ref()
                .map(|operation_id| Request::CancelDispatch {
                    client_id: work.context.client.id.clone(),
                    operation_id: operation_id.clone(),
                }),
        };
        if let Some(request) = release {
            work.stage = Stage::Release {
                request: Box::new(request),
                completion: Some(reply),
            };
            None
        } else {
            Some(reply)
        }
    }

    fn handoff_reply(&mut self, work: &mut Work, reply: Option<Reply>) -> Option<Reply> {
        if let Some(reply) = reply {
            self.fail_with_release(work, reply)
        } else {
            work.connection = None;
            work.completed = true;
            None
        }
    }

    pub(super) fn remember_client(&mut self, certificate: &[u8]) -> Result<(), Reply> {
        let id = fingerprint(certificate);
        if !self.known_certificates.contains_key(&id)
            && self.known_certificates.len() >= MAX_KNOWN_CLIENTS
        {
            return Err(xml::error(
                503,
                "retained client tracking capacity exhausted",
            ));
        }
        self.known_certificates.insert(id, certificate.to_vec());
        Ok(())
    }

    pub(super) fn revoke_client(&mut self, client_id: String) {
        if !self.revocations_inflight.contains(&client_id) {
            self.pending_revocations.insert(client_id);
        }
    }

    pub(super) fn revoke_untrusted_clients(
        &mut self,
        allowed: &mut impl FnMut(&[u8], Permission) -> bool,
    ) {
        let revoked: Vec<_> = self
            .known_certificates
            .iter()
            .filter(|(_, certificate)| !allowed(certificate, Permission::Paired))
            .map(|(id, _)| id.clone())
            .collect();
        for id in revoked {
            self.revoke_client(id);
        }
    }

    fn admit_revocations(&mut self, now: Instant) {
        while self.jobs.len() < MAX_JOBS {
            let Some(client_id) = self.pending_revocations.pop_first() else {
                break;
            };
            let Some(id) = self.next_job.checked_add(1) else {
                self.pending_revocations.insert(client_id);
                break;
            };
            self.next_job = id;
            self.revocations_inflight.insert(client_id.clone());
            let context = Context {
                client: Client {
                    id: client_id.clone(),
                    paired: true,
                    may_list: false,
                    may_view: false,
                    may_launch: false,
                    may_terminate: false,
                },
                game: None,
            };
            self.jobs.insert(
                id,
                Work {
                    connection: None,
                    certificate: Vec::new(),
                    context,
                    deadline: now + WORK_TIMEOUT,
                    not_before: now,
                    call: None,
                    stage: Stage::Release {
                        request: Box::new(Request::RevokeClient { client_id }),
                        completion: None,
                    },
                    cancelled: false,
                    dispatch_operation: None,
                    intent: None,
                    completed: false,
                    list_required: false,
                    cancellation_reply: None,
                },
            );
        }
    }

    fn admit_observations(&mut self, now: Instant) {
        while self.jobs.len() < MAX_JOBS {
            let Some(observation) = self.pending_observations.pop_front() else {
                break;
            };
            let context = Context {
                client: Client {
                    id: observation.client_id.clone(),
                    paired: true,
                    may_list: false,
                    may_view: false,
                    may_launch: false,
                    may_terminate: false,
                },
                game: None,
            };
            let Some(id) = self.next_job.checked_add(1) else {
                self.pending_observations.push_front(observation);
                break;
            };
            self.next_job = id;
            self.jobs.insert(
                id,
                Work {
                    connection: None,
                    certificate: Vec::new(),
                    context,
                    deadline: now + WORK_TIMEOUT,
                    not_before: now,
                    call: None,
                    stage: Stage::Observe {
                        observation,
                        completion: None,
                    },
                    cancelled: false,
                    dispatch_operation: None,
                    intent: None,
                    completed: false,
                    list_required: false,
                    cancellation_reply: None,
                },
            );
        }
    }

    pub(super) fn cancel(&mut self, connection: u64) {
        for work in self
            .jobs
            .values_mut()
            .filter(|work| work.connection == Some(connection))
        {
            work.connection = None;
            if !matches!(
                work.stage,
                Stage::AwaitStop { .. } | Stage::Observe { .. } | Stage::Release { .. }
            ) {
                work.cancelled = true;
            }
        }
    }

    pub(super) fn quiesce(&mut self) -> bool {
        self.quiescing = true;
        self.jobs.is_empty()
            && self.pending_observations.is_empty()
            && self.pending_revocations.is_empty()
            && self.recovery.is_idle()
    }

    pub(super) fn package_activation_barrier(&mut self, pending: bool) -> bool {
        self.activation_pending = pending;
        self.jobs.is_empty()
            && self.pending_observations.is_empty()
            && self.pending_revocations.is_empty()
            && self.recovery.is_idle()
    }
}

impl BrokerCall {
    fn poll(&mut self, send: &mut impl FnMut(Command) -> ModuleResult<Response>) -> CallOutcome {
        let Some(request) = self.request else {
            return match send(self.command.clone()) {
                Ok(Response::Accepted { id }) => {
                    self.request = Some(id);
                    CallOutcome::Waiting
                }
                Ok(Response::Error { .. }) => CallOutcome::Rejected,
                _ => CallOutcome::Waiting,
            };
        };
        if self.cancelling {
            let _ = send(Command::Cancel { request });
            let result = send(Command::TakeResult { request });
            return if super::cleanup::released(&result) {
                CallOutcome::Retired
            } else {
                CallOutcome::Waiting
            };
        }
        match send(Command::TakeResult { request }) {
            Ok(Response::Pending) => CallOutcome::Waiting,
            Ok(Response::Result { payload }) => CallOutcome::Result(payload),
            Ok(Response::Error { code, .. })
                if matches!(
                    code.as_str(),
                    "cancelled" | "deadline_exceeded" | "not_found"
                ) =>
            {
                CallOutcome::Rejected
            }
            _ => {
                self.cancelling = true;
                CallOutcome::Waiting
            }
        }
    }
}

fn projection(context: Context, launches: &Controller) -> Box<CatalogProjection> {
    Box::new(CatalogProjection::new(
        context,
        launches.desktop_enabled(),
        launches.codec_advertisement().codec_mode_support & 0x20200 != 0,
    ))
}

fn inventory(context: Context, launches: &Controller) -> Box<CatalogProjection> {
    Box::new(CatalogProjection::inventory(
        context,
        launches.desktop_enabled(),
        launches.codec_advertisement().codec_mode_support & 0x20200 != 0,
    ))
}

fn context_current(context: &Context, launches: &Controller) -> bool {
    let current = launches.session_presentation_context(context.client.clone());
    match (&context.game, &current.game) {
        (None, None) => true,
        (Some(expected), Some(actual)) => {
            expected.application.id == actual.application.id
                && expected.owner_client_id == actual.owner_client_id
                && expected.generation == actual.generation
                && expected.source == actual.source
        }
        _ => false,
    }
}

fn permissions_unchanged(work: &Work, allowed: &mut impl FnMut(&[u8], Permission) -> bool) -> bool {
    if work.certificate.is_empty() {
        return true;
    }
    let client = &work.context.client;
    allowed(&work.certificate, Permission::Paired)
        && (!work.list_required || allowed(&work.certificate, Permission::List))
        && (!client.may_list || allowed(&work.certificate, Permission::List))
        && (!client.may_view || allowed(&work.certificate, Permission::View))
        && (!client.may_launch || allowed(&work.certificate, Permission::Launch))
        && (!client.may_terminate || allowed(&work.certificate, Permission::Terminate))
}

pub(super) fn fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn numeric_id(id: &str) -> Result<u32, Reply> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(xml::error(502, "invalid projected application ID"));
    }
    let value = id
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0 && *value <= i32::MAX as u32)
        .ok_or_else(|| xml::error(502, "invalid projected application ID"))?;
    if value.to_string() != id {
        return Err(xml::error(502, "noncanonical projected application ID"));
    }
    Ok(value)
}

fn finish(work: &Work, reply: Reply, replies: &mut Vec<(u64, Reply)>) {
    if let Some(connection) = work.connection {
        replies.push((connection, reply));
    }
}

fn private_reply(mut reply: Reply, permission: Permission) -> Reply {
    reply.requires_paired_tls = true;
    reply.permission = Some(permission);
    reply
}

fn control_reply(
    status: u16,
    message: &str,
    intent: Option<DispatchIntent>,
    permission: Permission,
) -> Reply {
    let fields = if intent == Some(DispatchIntent::Cancel) {
        vec![("cancel", "1")]
    } else {
        vec![("resume", "0"), ("gamesession", "0")]
    };
    let mut reply = xml::response(valid_status(status), &bounded_message(message), &fields);
    // Moonlight reads the semantic status from XML. HTTP errors instead take
    // its transport-failure path and hide the action's completion message.
    reply.status = 200;
    private_reply(reply, permission)
}

fn valid_status(status: u16) -> u16 {
    if (200..=599).contains(&status) {
        status
    } else {
        502
    }
}
fn bounded_message(message: &str) -> String {
    message.chars().take(512).collect()
}

impl Router {
    pub(super) fn dispatch_presented(
        &mut self,
        connection: u64,
        query: &super::Query,
        peer: &RequestContext,
        now: Instant,
    ) -> Option<Reply> {
        let client = match PresentationRouter::client(peer) {
            Ok(client) => client,
            Err(reply) => return Some(reply),
        };
        let certificate = peer
            .peer_certificate
            .clone()
            .expect("trusted presentation certificate");
        let context = self.launches.session_presentation_context(client.clone());
        match query.path.as_str() {
            "/applist" => {
                if !peer.list_allowed {
                    return Some(xml::error(403, "application list permission denied"));
                }
                self.presentation
                    .as_mut()
                    .expect("selected provider")
                    .applications(connection, certificate, context, &self.launches, now)
            }
            "/appasset" => {
                if !peer.list_allowed {
                    return Some(xml::error(403, "application list permission denied"));
                }
                let id = match query
                    .required("appid")
                    .ok()
                    .and_then(|id| numeric_id(id).ok())
                {
                    Some(id) => id,
                    None => return Some(xml::error(400, "invalid application ID")),
                };
                self.presentation
                    .as_mut()
                    .expect("selected provider")
                    .artwork(
                        connection,
                        certificate,
                        context,
                        session::Selection {
                            id: id.to_string(),
                            uuid: None,
                        },
                        &self.launches,
                        now,
                    )
            }
            "/launch" | "/resume" | "/cancel" => {
                let intent = match query.path.as_str() {
                    "/resume" => DispatchIntent::Resume,
                    "/cancel" => DispatchIntent::Cancel,
                    _ => DispatchIntent::Launch,
                };
                let permitted = match intent {
                    DispatchIntent::Resume => client.may_view,
                    DispatchIntent::Cancel => client.may_terminate,
                    DispatchIntent::Launch => {
                        client.may_launch || client.may_view || client.may_terminate
                    }
                };
                if !permitted {
                    return Some(xml::error(403, "session operation permission denied"));
                }
                let request = if intent == DispatchIntent::Cancel {
                    if let Err(error) = query.require_only(&["uniqueid", "uuid"]) {
                        return Some(xml::error(400, error));
                    }
                    None
                } else {
                    match super::streaming::launch_request(query) {
                        Ok(request) => Some(request),
                        Err(error) => return Some(xml::error(400, error)),
                    }
                };
                let identity = match peer.identity() {
                    Ok(identity) => identity,
                    Err(error) => return Some(xml::error(401, error)),
                };
                let certificate_pem = match pem_rfc7468::encode_string(
                    "CERTIFICATE",
                    pem_rfc7468::LineEnding::LF,
                    &certificate,
                ) {
                    Ok(pem) => pem,
                    Err(_) => return Some(xml::error(401, "client certificate is unavailable")),
                };
                self.queue(
                    Some(connection),
                    Purpose::PresentationIdentity {
                        request,
                        peer: identity,
                        client,
                        intent,
                    },
                    json!({"action":"check_client_certificate","certificate_pem":certificate_pem}),
                    now + super::SERVICE_TIMEOUT,
                    now,
                )
                .err()
                .map(|message| xml::error(503, message))
            }
            _ => Some(xml::error(404, "unknown presentation route")),
        }
    }
}

#[cfg(test)]
mod tests;
