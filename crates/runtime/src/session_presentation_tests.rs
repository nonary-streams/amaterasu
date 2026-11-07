//! The protocol consumer remains identical when trusted composition selects a
//! different implementation of the public session-presentation contract.
use super::Broker;
use amaterasu_contracts::{ApiMethod, ApiMethodApproval, Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::requests::RequestOptions;
use amaterasu_module_sdk::session_presentation::{
    self, Application, Artwork, AuxiliaryStreamPlan, CaptureSource, Client, Context, Decision,
    DispatchIntent, Game, Mode, Observation, Presentation, RecoveryBinding, RecoveryWorkItem,
    Reply, Request, SERVICE, SessionEvent, SharedCapture,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const CONSUMER: &str = "org.example.protocol";
const OUTSIDER: &str = "org.example.outsider";
const UNSELECTED: &str = "org.example.unselected";
const SECOND_CONSUMER: &str = "org.example.secondary-protocol";

fn context() -> Context {
    Context {
        client: Client {
            id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            paired: true,
            may_list: true,
            may_view: true,
            may_launch: true,
            may_terminate: false,
        },
        game: Some(Game {
            application: Application {
                id: "game-opaque-id".into(),
                uuid: "b063b169-a0c7-47de-99c6-bd945008876f".into(),
                title: "Running game".into(),
                aliases: Vec::new(),
                visible: true,
                monitor_profile: false,
                properties: json!({}),
            },
            owner_client_id: "another-authenticated-client".into(),
            generation: 17,
            source: None,
        }),
    }
}

// This consumer only knows the versioned public contract and authenticated
// context. It has no branch for either provider principal or product policy.
fn info_command(operation_id: &str) -> Command {
    session_presentation::command(
        RequestOptions {
            service: SERVICE,
            timeout_ms: 1_000,
            operation_id,
            parent: None,
        },
        &Request::Info { context: context() },
    )
    .unwrap()
}

fn selected_broker(provider: &str) -> Broker {
    let mut policy = RuntimeConfig::default();
    for caller in [CONSUMER, SECOND_CONSUMER, OUTSIDER] {
        policy.principals.insert(
            caller.into(),
            Grants {
                call: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    for principal in [provider, UNSELECTED] {
        policy.principals.insert(
            principal.into(),
            Grants {
                provide: [SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    for method in session_presentation::methods() {
        policy.api_methods.push(ApiMethodApproval {
            provider: provider.into(),
            callers: [CONSUMER.into(), SECOND_CONSUMER.into()].into(),
            method,
        });
    }
    Broker::new(policy).unwrap()
}

fn register(broker: &mut Broker, provider: &str) {
    for method in session_presentation::methods() {
        assert_ok(broker.dispatch(provider, Command::RegisterApi { method }));
    }
}

fn info_method() -> ApiMethod {
    session_presentation::methods()
        .into_iter()
        .find(|method| method.method == "info")
        .unwrap()
}

fn accepted(response: Response) -> u64 {
    match response {
        Response::Accepted { id } => id,
        other => panic!("expected request admission, got {other:?}"),
    }
}

fn assert_ok(response: Response) {
    assert!(matches!(response, Response::Ok), "{response:?}");
}

fn assert_error(response: Response, expected: &str) {
    assert!(
        matches!(&response, Response::Error { code, .. } if code == expected),
        "expected {expected}, got {response:?}"
    );
}

fn deliver_info(broker: &mut Broker, provider: &str, request: u64) {
    let Response::Request {
        id,
        caller,
        initiator,
        service,
        payload,
        ..
    } = broker.dispatch(provider, Command::Poll {})
    else {
        panic!("selected provider did not receive the typed API request");
    };
    assert_eq!(id, request);
    assert_eq!(caller, CONSUMER);
    assert_eq!(initiator, CONSUMER);
    assert_eq!(service, SERVICE);
    assert_eq!(
        session_presentation::decode(&service, payload).unwrap(),
        Request::Info { context: context() }
    );
}

fn complete_info(broker: &mut Broker, provider: &str, request: u64, presentation: Presentation) {
    assert_ok(broker.dispatch(
        provider,
        Command::Complete {
            request,
            payload: serde_json::to_value(Reply::Info { presentation }).unwrap(),
        },
    ));
}

fn consume_info(broker: &mut Broker, request: u64) -> Presentation {
    let Response::Result { payload } = broker.dispatch(CONSUMER, Command::TakeResult { request })
    else {
        panic!("consumer did not receive the provider result");
    };
    let Reply::Info { presentation } = session_presentation::decode_reply(payload).unwrap() else {
        panic!("provider returned an incompatible method result");
    };
    assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
    presentation
}

#[test]
fn session_presentation_consumer_survives_selected_provider_replacement() {
    let unchanged_call = serde_json::to_value(info_command("presentation-info")).unwrap();
    let presentations = [
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
        Presentation {
            busy: true,
            current_game: Some("game-opaque-id".into()),
            current_game_uuid: Some("b063b169-a0c7-47de-99c6-bd945008876f".into()),
        },
    ];
    let mut actual = Vec::new();
    for (provider, presentation) in ["vendor.primary", "vendor.replacement"]
        .into_iter()
        .zip(presentations.iter())
    {
        let mut broker = selected_broker(provider);
        register(&mut broker, provider);
        let Response::Result { payload } = broker.dispatch(
            CONSUMER,
            Command::QueryApiApproval {
                service: SERVICE.into(),
                method: "info".into(),
                interface_major: 1,
                min_minor: 0,
            },
        ) else {
            panic!("consumer could not discover its selected API approval");
        };
        let approval: ApiMethodApproval = serde_json::from_value(payload).unwrap();
        assert_eq!(approval.provider, provider);
        assert_eq!(approval.method, info_method());
        assert_eq!(
            serde_json::to_value(info_command("presentation-info")).unwrap(),
            unchanged_call
        );
        let request = accepted(broker.dispatch(CONSUMER, info_command("presentation-info")));
        deliver_info(&mut broker, provider, request);
        assert!(matches!(
            broker.dispatch(UNSELECTED, Command::Poll {}),
            Response::Pending
        ));
        complete_info(&mut broker, provider, request, presentation.clone());
        actual.push(consume_info(&mut broker, request));
        assert_error(
            broker.dispatch(CONSUMER, Command::TakeResult { request }),
            "not_found",
        );
    }
    assert_eq!(actual, presentations);
}

#[test]
fn session_presentation_schema_identity_and_selected_grants_are_enforced() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    let method = info_method();
    // An unavailable selected provider never causes an implicit raw-service
    // fallback to another component that happens to hold a provide grant.
    assert_error(
        broker.dispatch(CONSUMER, info_command("provider-unavailable")),
        "permission_denied",
    );
    assert_error(
        broker.dispatch(
            UNSELECTED,
            Command::Register {
                service: SERVICE.into(),
            },
        ),
        "permission_denied",
    );
    assert_error(
        broker.dispatch(
            UNSELECTED,
            Command::RegisterApi {
                method: method.clone(),
            },
        ),
        "permission_denied",
    );
    let mut altered = method.clone();
    altered.request.allow_extra = true;
    assert_error(
        broker.dispatch(provider, Command::RegisterApi { method: altered }),
        "permission_denied",
    );
    // The same service name and version do not approve a different response
    // schema either. Composition selects the full published method contract.
    let mut altered = method.clone();
    altered.response.required.clear();
    assert_error(
        broker.dispatch(provider, Command::RegisterApi { method: altered }),
        "permission_denied",
    );
    register(&mut broker, provider);
    assert_error(
        broker.dispatch(OUTSIDER, info_command("unapproved-consumer")),
        "permission_denied",
    );
    let mut wrong_major = info_command("wrong-major");
    let Command::CallApi {
        interface_major, ..
    } = &mut wrong_major
    else {
        panic!("typed helper did not build an API invocation");
    };
    *interface_major = 2;
    // A different major identifies another contract and has no installed
    // approval. A minor mismatch within an approved major is incompatible.
    assert_error(broker.dispatch(CONSUMER, wrong_major), "permission_denied");
    let mut wrong_minor = info_command("wrong-minor");
    let Command::CallApi {
        interface_minor, ..
    } = &mut wrong_minor
    else {
        unreachable!();
    };
    *interface_minor = 1;
    assert_error(broker.dispatch(CONSUMER, wrong_minor), "incompatible_api");
    let mut extra_field = info_command("extra-field");
    let Command::CallApi { payload, .. } = &mut extra_field else {
        unreachable!();
    };
    payload["provider"] = json!(UNSELECTED);
    assert_error(broker.dispatch(CONSUMER, extra_field), "invalid_request");
    assert_error(
        broker.dispatch(
            CONSUMER,
            Command::Request {
                service: SERVICE.into(),
                payload: serde_json::to_value(Request::Info { context: context() }).unwrap(),
                timeout_ms: 1_000,
                operation_id: "untyped-route".into(),
                parent: None,
            },
        ),
        "permission_denied",
    );
    assert!(
        session_presentation::decode(
            SERVICE,
            serde_json::to_value(Request::Info { context: context() }).unwrap()
        )
        .is_err()
    );
}

#[test]
fn session_presentation_results_and_cancellation_retain_exact_caller_authority() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    let request = accepted(broker.dispatch(CONSUMER, info_command("owned-result")));
    assert_eq!(
        accepted(broker.dispatch(CONSUMER, info_command("owned-result"))),
        request
    );
    deliver_info(&mut broker, provider, request);
    for outsider in [provider, OUTSIDER] {
        assert_error(
            broker.dispatch(outsider, Command::TakeResult { request }),
            "permission_denied",
        );
        assert_error(
            broker.dispatch(outsider, Command::ReleaseResult { request }),
            "permission_denied",
        );
        assert_error(
            broker.dispatch(outsider, Command::Cancel { request }),
            "permission_denied",
        );
    }
    assert_error(
        broker.dispatch(CONSUMER, Command::ReleaseResult { request }),
        "request_pending",
    );
    complete_info(
        &mut broker,
        provider,
        request,
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
    );
    for outsider in [provider, OUTSIDER] {
        assert_error(
            broker.dispatch(outsider, Command::ReleaseResult { request }),
            "permission_denied",
        );
    }
    consume_info(&mut broker, request);
    let replacement = accepted(broker.dispatch(CONSUMER, info_command("owned-result")));
    assert!(replacement > request);
    deliver_info(&mut broker, provider, replacement);
    assert_ok(broker.dispatch(
        CONSUMER,
        Command::Cancel {
            request: replacement,
        },
    ));
    assert_error(
        broker.dispatch(
            provider,
            Command::CheckRequest {
                request: replacement,
            },
        ),
        "request_ended",
    );
    assert_error(
        broker.dispatch(
            provider,
            Command::Complete {
                request: replacement,
                payload: serde_json::to_value(Reply::Acknowledged {}).unwrap(),
            },
        ),
        "request_ended",
    );
    assert_ok(broker.dispatch(
        CONSUMER,
        Command::ReleaseResult {
            request: replacement,
        },
    ));
}

#[test]
fn session_presentation_consumed_and_cancelled_calls_do_not_exhaust_broker_capacity() {
    let provider = "vendor.replacement";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    // The broker retains at most 512 requests. More than twice that number
    // exercises the SDK consumer's normal consumption and cancellation paths.
    for sequence in 0..1_100 {
        let operation_id = format!("presentation-{sequence}");
        let request = accepted(broker.dispatch(CONSUMER, info_command(&operation_id)));
        if sequence % 2 == 0 {
            deliver_info(&mut broker, provider, request);
            complete_info(
                &mut broker,
                provider,
                request,
                Presentation {
                    busy: false,
                    current_game: None,
                    current_game_uuid: None,
                },
            );
            consume_info(&mut broker, request);
        } else {
            assert_ok(broker.dispatch(CONSUMER, Command::Cancel { request }));
            assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
        }
    }
    assert!(matches!(
        broker.dispatch(provider, Command::Poll {}),
        Response::Pending
    ));
}

#[test]
fn session_presentation_response_schema_is_checked_before_result_completion() {
    let provider = "vendor.primary";
    let mut broker = selected_broker(provider);
    register(&mut broker, provider);
    let request = accepted(broker.dispatch(CONSUMER, info_command("response-schema")));
    deliver_info(&mut broker, provider, request);
    for invalid in [json!({}), json!({"result":false}), Value::Null] {
        assert_error(
            broker.dispatch(
                provider,
                Command::Complete {
                    request,
                    payload: invalid,
                },
            ),
            "invalid_request",
        );
    }
    complete_info(
        &mut broker,
        provider,
        request,
        Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
    );
    consume_info(&mut broker, request);
}

// These deliberately small providers exercise the *public contract*. They do
// not simulate native resource retirement or qualify the real monitor provider.
// Each has its own state and product strategy: one publishes controls first and
// admits immediately, the other publishes controls last and stages admission.
// Both reuse the same reservation bookkeeping, as external implementations may
// reuse SDK helpers without changing the protocol consumer.
#[derive(Clone, Copy)]
enum FixtureStrategy {
    Immediate,
    Staged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FixtureScope {
    caller: String,
    client: String,
}

impl FixtureScope {
    fn new(caller: &str, client: &str) -> Self {
        Self {
            caller: caller.into(),
            client: client.into(),
        }
    }
}

struct FixtureProjection {
    scope: FixtureScope,
    context: Context,
    catalog_revision: String,
    catalog: Vec<Application>,
    projected: Option<Vec<Application>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FixtureBinding {
    transport: String,
    epoch: String,
}

struct FixtureReservation {
    scope: FixtureScope,
    operation: String,
    auxiliary: bool,
    binding: Option<FixtureBinding>,
    started: bool,
    application: Option<FixtureApplicationBinding>,
    transport_retired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FixtureApplicationBinding {
    operation: String,
    epoch: String,
    generation: u64,
}

struct FixtureSource {
    owner: FixtureScope,
    session: String,
    generation: u64,
    mode: Mode,
    capture: CaptureSource,
    role_retained: bool,
    role_confirmed: bool,
    reservations: BTreeMap<String, FixtureReservation>,
    retired: BTreeMap<String, (FixtureScope, Option<FixtureBinding>, bool)>,
    announced_game: Option<Game>,
}

impl FixtureSource {
    fn shared(&self, reservation_id: &str) -> SharedCapture {
        SharedCapture {
            session_id: self.session.clone(),
            generation: self.generation,
            reservation_id: reservation_id.into(),
            capture: self.capture.clone(),
            mode: self.mode,
        }
    }

    fn matches(&self, source: &SharedCapture) -> bool {
        self.session == source.session_id
            && self.generation == source.generation
            && self.capture == source.capture
            && self.mode == source.mode
    }
}

struct FixtureAdmission {
    request: Request,
    decision: Decision,
    delivered: bool,
    cancelled: bool,
}

struct FixtureRecovery {
    caller: String,
    item: RecoveryWorkItem,
}

struct FixtureRecoveryCursor {
    caller: String,
    revision: u64,
    after: String,
}

type AuthorityEvidence = (String, String, String, String, String, u64);

struct StatefulFixture {
    strategy: FixtureStrategy,
    sequence: u64,
    generation: u64,
    control: Application,
    alias: Application,
    catalog: BTreeMap<String, Vec<Application>>,
    catalog_revision: BTreeMap<String, String>,
    projections: BTreeMap<String, FixtureProjection>,
    sources: BTreeMap<String, FixtureSource>,
    admissions: BTreeMap<(String, String, String), FixtureAdmission>,
    observed_methods: BTreeSet<String>,
    pending_replies: usize,
    recovery: BTreeMap<String, FixtureRecovery>,
    recovery_revision: u64,
    recovery_cursors: BTreeMap<String, FixtureRecoveryCursor>,
    completed_recovery: BTreeMap<String, String>,
    retirement_evidence: BTreeSet<AuthorityEvidence>,
}

fn fixture_application(id: &str, title: &str) -> Application {
    Application {
        id: id.into(),
        uuid: format!("uuid-{id}"),
        title: title.into(),
        aliases: Vec::new(),
        visible: true,
        monitor_profile: false,
        properties: json!({}),
    }
}

fn fixture_error(code: &str) -> Reply {
    Reply::Error {
        code: code.into(),
        message: format!("fixture rejected {code}"),
    }
}

fn fixture_png() -> Vec<u8> {
    // A static 1x1 grayscale/alpha PNG, not a runtime capture artifact.
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x04\0\0\0\xb5\x1c\x0c\x02\0\0\0\x0bIDAT\x78\xda\x63\x64\xf8\x0f\0\x01\x05\x01\x01\x27\x18\xe3\x66\0\0\0\0IEND\xae\x42\x60\x82".to_vec()
}

impl StatefulFixture {
    fn new(strategy: FixtureStrategy) -> Self {
        let (control_id, alias_id, title) = match strategy {
            FixtureStrategy::Immediate => ("display-entry-a", "resume-tile-a", "Attach display"),
            FixtureStrategy::Staged => ("extension-b-opaque", "current-b-opaque", "Extend desktop"),
        };
        let mut control = fixture_application(control_id, title);
        control.monitor_profile = true;
        let mut alias = context().game.unwrap().application;
        alias.id = alias_id.into();
        Self {
            strategy,
            sequence: 0,
            generation: 0,
            control,
            alias,
            catalog: BTreeMap::new(),
            catalog_revision: BTreeMap::new(),
            projections: BTreeMap::new(),
            sources: BTreeMap::new(),
            admissions: BTreeMap::new(),
            observed_methods: BTreeSet::new(),
            pending_replies: 0,
            recovery: BTreeMap::new(),
            recovery_revision: 0,
            recovery_cursors: BTreeMap::new(),
            completed_recovery: BTreeMap::new(),
            retirement_evidence: BTreeSet::new(),
        }
    }

    fn allocate(&mut self, kind: &str) -> String {
        self.sequence += 1;
        match self.strategy {
            FixtureStrategy::Immediate => format!("a-{kind}-{}", self.sequence),
            FixtureStrategy::Staged => format!("opaque-b-{:x}-{kind}", self.sequence * 97),
        }
    }

    fn presentation(&self, context: &Context) -> Presentation {
        let expose = matches!(self.strategy, FixtureStrategy::Staged);
        Presentation {
            busy: expose && context.game.is_some(),
            current_game: context
                .game
                .as_ref()
                .filter(|_| expose)
                .map(|game| game.application.id.clone()),
            current_game_uuid: context
                .game
                .as_ref()
                .filter(|_| expose)
                .map(|game| game.application.uuid.clone()),
        }
    }

    fn collect_retired_sources(&mut self) {
        self.sources
            .retain(|_, source| source.role_retained || !source.reservations.is_empty());
    }

    fn cancel_operation(&mut self, scope: &FixtureScope, operation: &str) -> Reply {
        let key = (scope.caller.clone(), scope.client.clone(), operation.into());
        if self.sources.values().any(|source| {
            source.reservations.values().any(|reservation| {
                reservation.scope == *scope
                    && reservation.operation == operation
                    && (reservation.binding.is_some() || reservation.application.is_some())
            })
        }) {
            return fixture_error("transport_must_retire");
        }
        for source in self.sources.values_mut() {
            let cancelled_auxiliary = source.reservations.values().any(|reservation| {
                reservation.scope == *scope
                    && reservation.operation == operation
                    && reservation.auxiliary
            });
            source.reservations.retain(|_, reservation| {
                reservation.scope != *scope || reservation.operation != operation
            });
            if cancelled_auxiliary && !source.role_confirmed {
                source.role_retained = false;
            }
        }
        if let Some(admission) = self.admissions.get_mut(&key) {
            admission.cancelled = true;
        }
        self.collect_retired_sources();
        Reply::Acknowledged {}
    }

    fn dispatch(&mut self, caller: &str, request: Request) -> Reply {
        let Request::Dispatch {
            context,
            operation_id,
            intent,
            selection,
            mode,
        } = &request
        else {
            unreachable!();
        };
        let scope = FixtureScope::new(caller, &context.client.id);
        let key = (
            caller.into(),
            context.client.id.clone(),
            operation_id.clone(),
        );
        if let Some(admission) = self.admissions.get_mut(&key) {
            if admission.request != request {
                return fixture_error("operation_conflict");
            }
            if admission.cancelled {
                return fixture_error("dispatch_cancelled");
            }
            admission.delivered = true;
            return Reply::Dispatched {
                decision: admission.decision.clone(),
            };
        }
        let resuming = *intent == DispatchIntent::Resume;
        if (resuming && !context.client.may_view) || (!resuming && !context.client.may_launch) {
            return Reply::Dispatched {
                decision: Decision::Reject {
                    status_code: 403,
                    message: "launch permission denied".into(),
                },
            };
        }
        if *intent == DispatchIntent::Cancel {
            return fixture_error("not_stream_admission");
        }
        let Some(selection) = selection else {
            return fixture_error("selection_required");
        };
        let mode = mode.expect("SDK validated non-cancel mode");
        let auxiliary = selection.id == self.control.id;
        if auxiliary
            && resuming
            && !self
                .sources
                .values()
                .any(|source| source.owner == scope && source.role_retained)
        {
            return fixture_error("no_retained_role");
        }
        let target = if selection.id == self.alias.id {
            let Some(game) = &context.game else {
                return fixture_error("stale_game");
            };
            if game.application.uuid != self.alias.uuid {
                return fixture_error("stale_game");
            }
            game.application.id.clone()
        } else {
            selection.id.clone()
        };
        if !auxiliary
            && !self.catalog.get(&scope.client).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.id == target && entry.visible)
            })
        {
            return fixture_error("unknown_application");
        }
        let reservation_id = self.allocate("reservation");
        let session = if auxiliary {
            if let Some(source) = self.sources.values().find(|source| source.owner == scope) {
                source.session.clone()
            } else {
                let session = self.allocate("session");
                self.generation += 1;
                self.sources.insert(
                    session.clone(),
                    FixtureSource {
                        owner: scope.clone(),
                        session: session.clone(),
                        generation: self.generation,
                        mode: match self.strategy {
                            FixtureStrategy::Immediate => mode,
                            FixtureStrategy::Staged => Mode {
                                width: 2560,
                                height: 1440,
                                refresh_millihz: 144_000,
                                hdr: true,
                            },
                        },
                        capture: CaptureSource::ExactOutput {
                            route_key: match self.strategy {
                                FixtureStrategy::Immediate => "startup-capture-a".into(),
                                FixtureStrategy::Staged => "startup-capture-b".into(),
                            },
                            output: format!("logical-output-{session}"),
                        },
                        role_retained: true,
                        role_confirmed: false,
                        reservations: BTreeMap::new(),
                        retired: BTreeMap::new(),
                        announced_game: None,
                    },
                );
                session
            }
        } else if let Some(root) = context.game.as_ref().and_then(|game| game.source.as_ref()) {
            let Some(source) = self.sources.get(&root.session_id) else {
                return fixture_error("source_changed");
            };
            let game = context.game.as_ref().unwrap();
            let original = source.reservations.get(&root.reservation_id);
            if !source.matches(root)
                || original.is_none_or(|reservation| {
                    reservation.scope.client != game.owner_client_id
                        || reservation.binding.is_none()
                        || !reservation.started
                        || reservation
                            .application
                            .as_ref()
                            .is_none_or(|application| application.generation != game.generation)
                })
                || source.announced_game.as_ref() != Some(game)
            {
                return fixture_error("game_source_changed");
            }
            root.session_id.clone()
        } else {
            let Some(source) = self.sources.values().find(|source| source.owner == scope) else {
                return fixture_error("no_retained_source");
            };
            source.session.clone()
        };
        let source = self.sources.get_mut(&session).unwrap();
        if source.reservations.len() >= 8 {
            return fixture_error("reservation_capacity");
        }
        source.reservations.insert(
            reservation_id.clone(),
            FixtureReservation {
                scope,
                operation: operation_id.clone(),
                auxiliary,
                binding: None,
                started: false,
                application: None,
                transport_retired: false,
            },
        );
        let shared = source.shared(&reservation_id);
        let decision = if auxiliary {
            let plan = AuxiliaryStreamPlan {
                session_id: shared.session_id.clone(),
                generation: shared.generation,
                reservation_id,
                capture: shared.capture.clone(),
                mode,
                source_mode: Some(shared.mode),
                stream_audio: true,
                host_audio: true,
            };
            if resuming {
                Decision::ResumeAuxiliary { plan }
            } else {
                Decision::Auxiliary { plan }
            }
        } else if *intent == DispatchIntent::Resume {
            Decision::ResumeApplication {
                application_id: target,
                source: Some(shared),
            }
        } else {
            Decision::LaunchApplication {
                application_id: target,
                source: Some(shared),
            }
        };
        let deferred = matches!(self.strategy, FixtureStrategy::Staged);
        self.admissions.insert(
            key,
            FixtureAdmission {
                request,
                decision: decision.clone(),
                delivered: !deferred,
                cancelled: false,
            },
        );
        if deferred {
            self.pending_replies += 1;
            Reply::Pending { retry_after_ms: 1 }
        } else {
            Reply::Dispatched { decision }
        }
    }

    fn verify(&self, scope: &FixtureScope, requested: &SharedCapture) -> Reply {
        let Some(source) = self.sources.get(&requested.session_id) else {
            return fixture_error("source_retired");
        };
        if !source.matches(requested)
            || source
                .reservations
                .get(&requested.reservation_id)
                .is_none_or(|reservation| reservation.scope != *scope)
        {
            return fixture_error("source_changed");
        }
        Reply::Verified {
            source: requested.clone(),
        }
    }

    fn bind(&mut self, caller: &str, observation: Observation) -> Reply {
        let scope = FixtureScope::new(caller, &observation.client_id);
        let Some(source) = self.sources.get_mut(&observation.session_id) else {
            return fixture_error("source_retired");
        };
        if source.generation != observation.generation {
            return fixture_error("source_changed");
        }
        let Some(reservation) = source.reservations.get_mut(&observation.reservation_id) else {
            return fixture_error("reservation_changed");
        };
        if reservation.scope != scope {
            return fixture_error("reservation_owner");
        }
        let Some(epoch) = observation.transport_epoch else {
            return fixture_error("transport_epoch_required");
        };
        let proposed = FixtureBinding {
            transport: observation.transport_id,
            epoch,
        };
        if reservation
            .binding
            .as_ref()
            .is_some_and(|binding| binding != &proposed)
        {
            return fixture_error("transport_binding_changed");
        }
        reservation.binding = Some(proposed);
        Reply::Acknowledged {}
    }

    fn bind_application(
        &mut self,
        caller: &str,
        source: SharedCapture,
        client: &str,
        proposed: FixtureApplicationBinding,
    ) -> Reply {
        let Some(retained) = self.sources.get_mut(&source.session_id) else {
            return fixture_error("source_retired");
        };
        if retained.generation != source.generation {
            return fixture_error("source_changed");
        }
        let Some(reservation) = retained.reservations.get_mut(&source.reservation_id) else {
            return fixture_error("reservation_changed");
        };
        if reservation.scope != FixtureScope::new(caller, client) || reservation.auxiliary {
            return fixture_error("reservation_owner");
        }
        if reservation
            .application
            .as_ref()
            .is_some_and(|application| application != &proposed)
        {
            return fixture_error("application_binding_changed");
        }
        reservation.application = Some(proposed);
        Reply::Acknowledged {}
    }

    // Private fixture evidence for the selected execution provider's status API.
    // Transport closure alone cannot manufacture this independent proof.
    fn execution_retired(
        &mut self,
        scope: &FixtureScope,
        evidence: &FixtureApplicationBinding,
    ) -> bool {
        let mut retired = false;
        for source in self.sources.values_mut() {
            source.reservations.retain(|_, reservation| {
                let matches = reservation.scope == *scope
                    && reservation.application.as_ref() == Some(evidence)
                    && reservation.transport_retired;
                retired |= matches;
                !matches
            });
        }
        self.collect_retired_sources();
        retired
    }

    fn observe(&mut self, caller: &str, observation: Observation) -> Reply {
        let scope = FixtureScope::new(caller, &observation.client_id);
        let Some(source) = self.sources.get_mut(&observation.session_id) else {
            return fixture_error("source_retired");
        };
        if source.generation != observation.generation {
            return fixture_error("source_changed");
        }
        let ownership = source
            .reservations
            .get(&observation.reservation_id)
            .map(|reservation| {
                (
                    &reservation.scope,
                    reservation.binding.as_ref(),
                    reservation.auxiliary,
                )
            })
            .or_else(|| {
                source
                    .retired
                    .get(&observation.reservation_id)
                    .map(|(scope, binding, auxiliary)| (scope, binding.as_ref(), *auxiliary))
            });
        let Some((owner, binding, auxiliary)) = ownership else {
            return fixture_error("reservation_changed");
        };
        if *owner != scope {
            return fixture_error("reservation_owner");
        }
        if binding.is_some_and(|binding| {
            observation.transport_id != binding.transport
                || observation.transport_epoch.as_deref() != Some(binding.epoch.as_str())
        }) {
            return fixture_error("transport_binding_changed");
        }
        match observation.event {
            SessionEvent::StreamStarted => {
                let Some(reservation) = source.reservations.get_mut(&observation.reservation_id)
                else {
                    return fixture_error("reservation_retired");
                };
                if reservation.binding.is_none() {
                    return fixture_error("transport_not_bound");
                }
                reservation.started = true;
                if auxiliary {
                    source.role_confirmed = true;
                }
            }
            SessionEvent::StreamEnded
            | SessionEvent::ClientDisconnected
            | SessionEvent::FailedStart => {
                if let Some(reservation) = source.reservations.get_mut(&observation.reservation_id)
                    && reservation.application.is_some()
                {
                    reservation.transport_retired = true;
                } else if let Some(reservation) =
                    source.reservations.remove(&observation.reservation_id)
                {
                    if observation.event == SessionEvent::FailedStart
                        && auxiliary
                        && !source.role_confirmed
                    {
                        source.role_retained = false;
                    }
                    source.retired.insert(
                        observation.reservation_id,
                        (
                            reservation.scope,
                            reservation.binding,
                            reservation.auxiliary,
                        ),
                    );
                }
            }
            SessionEvent::ExplicitDisconnect => {
                if !auxiliary {
                    return fixture_error("not_auxiliary_role");
                }
                if source
                    .reservations
                    .values()
                    .any(|reservation| reservation.auxiliary && reservation.scope == scope)
                {
                    return Reply::Pending { retry_after_ms: 1 };
                }
                source.role_retained = false;
            }
            SessionEvent::Unpaired | SessionEvent::Shutdown => {
                if source.owner != scope {
                    return fixture_error("source_owner");
                }
                source.role_retained = false;
                if !source.reservations.is_empty() {
                    return Reply::Pending { retry_after_ms: 1 };
                }
            }
        }
        self.collect_retired_sources();
        Reply::Acknowledged {}
    }

    fn recovery_intents(&mut self, scope: &FixtureScope) {
        let mut candidates = Vec::new();
        for source in self.sources.values() {
            for (reservation_id, reservation) in &source.reservations {
                if reservation.scope != *scope {
                    continue;
                }
                let mut bindings = Vec::new();
                if !reservation.transport_retired
                    && let Some(binding) = &reservation.binding
                {
                    bindings.push(RecoveryBinding::Transport {
                        operation_id: binding.transport.clone(),
                        epoch: binding.epoch.clone(),
                    });
                }
                if let Some(binding) = &reservation.application {
                    bindings.push(RecoveryBinding::Application {
                        operation_id: binding.operation.clone(),
                        epoch: binding.epoch.clone(),
                        application_generation: binding.generation,
                    });
                }
                for binding in bindings {
                    if !self.recovery.values().any(|existing| {
                        existing.caller == scope.caller
                            && existing.item.client_id == scope.client
                            && existing.item.session_id == source.session
                            && existing.item.generation == source.generation
                            && existing.item.reservation_id == *reservation_id
                            && existing.item.binding == binding
                    }) {
                        candidates.push(RecoveryWorkItem {
                            id: String::new(),
                            client_id: scope.client.clone(),
                            session_id: source.session.clone(),
                            generation: source.generation,
                            reservation_id: reservation_id.clone(),
                            binding,
                            budget_ms: 1_000,
                        });
                    }
                }
            }
        }
        for mut item in candidates {
            item.id = self.allocate("retirement-work");
            item.validate().unwrap();
            self.recovery.insert(
                item.id.clone(),
                FixtureRecovery {
                    caller: scope.caller.clone(),
                    item,
                },
            );
            self.recovery_revision += 1;
        }
    }

    fn list_recovery(&mut self, caller: &str, cursor: Option<String>, limit: usize) -> Reply {
        let after = match cursor {
            Some(cursor) => {
                let Some(cursor) = self.recovery_cursors.get(&cursor) else {
                    return fixture_error("invalid_recovery_cursor");
                };
                if cursor.caller != caller {
                    return fixture_error("recovery_owner");
                }
                if cursor.revision != self.recovery_revision {
                    return fixture_error("stale_recovery_cursor");
                }
                Some(cursor.after.clone())
            }
            None => None,
        };
        if !self.recovery.is_empty() && !self.recovery.values().any(|entry| entry.caller == caller)
        {
            return fixture_error("recovery_owner");
        }
        let eligible: Vec<_> = self
            .recovery
            .values()
            .filter(|entry| {
                entry.caller == caller && after.as_ref().is_none_or(|after| entry.item.id > *after)
            })
            .map(|entry| entry.item.clone())
            .collect();
        let items: Vec<_> = eligible.iter().take(limit).cloned().collect();
        let next_cursor = if eligible.len() > limit {
            let token = self.allocate("recovery-cursor");
            self.recovery_cursors.insert(
                token.clone(),
                FixtureRecoveryCursor {
                    caller: caller.into(),
                    revision: self.recovery_revision,
                    after: items.last().unwrap().id.clone(),
                },
            );
            Some(token)
        } else {
            None
        };
        Reply::RecoveryWork { items, next_cursor }
    }

    fn evidence_key(scope: &FixtureScope, binding: &RecoveryBinding) -> AuthorityEvidence {
        let (kind, operation, epoch, generation) = match binding {
            RecoveryBinding::Transport {
                operation_id,
                epoch,
            } => ("transport", operation_id.clone(), epoch.clone(), 0),
            RecoveryBinding::Application {
                operation_id,
                epoch,
                application_generation,
            } => (
                "application",
                operation_id.clone(),
                epoch.clone(),
                *application_generation,
            ),
        };
        (
            scope.caller.clone(),
            scope.client.clone(),
            kind.into(),
            operation,
            epoch,
            generation,
        )
    }

    // This is private selected-owner status evidence, not an SDK method or a
    // caller's boolean assertion that cleanup succeeded. The provider will
    // independently query this exact operation/epoch/generation when reconciling.
    fn selected_owner_retired(&mut self, scope: &FixtureScope, binding: &RecoveryBinding) -> bool {
        let exact = self.sources.values().any(|source| {
            source.reservations.values().any(|reservation| {
                reservation.scope == *scope
                    && match binding {
                        RecoveryBinding::Transport {
                            operation_id,
                            epoch,
                        } => reservation.binding.as_ref().is_some_and(|binding| {
                            binding.transport == *operation_id && binding.epoch == *epoch
                        }),
                        RecoveryBinding::Application {
                            operation_id,
                            epoch,
                            application_generation,
                        } => reservation.application.as_ref().is_some_and(|binding| {
                            binding.operation == *operation_id
                                && binding.epoch == *epoch
                                && binding.generation == *application_generation
                        }),
                    }
            })
        });
        if exact {
            self.retirement_evidence
                .insert(Self::evidence_key(scope, binding));
        }
        exact
    }

    fn reconcile_recovery(&mut self, caller: &str, work_id: &str) -> Reply {
        let Some(work) = self.recovery.get(work_id) else {
            return if self
                .completed_recovery
                .get(work_id)
                .is_some_and(|owner| owner == caller)
            {
                Reply::Acknowledged {}
            } else {
                fixture_error("invalid_recovery_work")
            };
        };
        if work.caller != caller {
            return fixture_error("recovery_owner");
        }
        let scope = FixtureScope::new(caller, &work.item.client_id);
        if !self
            .retirement_evidence
            .contains(&Self::evidence_key(&scope, &work.item.binding))
        {
            return Reply::Pending { retry_after_ms: 1 };
        }
        let item = work.item.clone();
        let Some(source) = self.sources.get_mut(&item.session_id) else {
            return fixture_error("recovery_target_changed");
        };
        if source.generation != item.generation {
            return fixture_error("recovery_target_changed");
        }
        let Some(reservation) = source.reservations.get_mut(&item.reservation_id) else {
            return fixture_error("recovery_target_changed");
        };
        if reservation.scope != scope {
            return fixture_error("recovery_owner");
        }
        match &item.binding {
            RecoveryBinding::Transport {
                operation_id,
                epoch,
            } => {
                if reservation.binding.as_ref().is_none_or(|binding| {
                    binding.transport != *operation_id || binding.epoch != *epoch
                }) {
                    return fixture_error("recovery_target_changed");
                }
                reservation.transport_retired = true;
            }
            RecoveryBinding::Application {
                operation_id,
                epoch,
                application_generation,
            } => {
                if reservation.application.as_ref().is_none_or(|binding| {
                    binding.operation != *operation_id
                        || binding.epoch != *epoch
                        || binding.generation != *application_generation
                }) {
                    return fixture_error("recovery_target_changed");
                }
                reservation.application = None;
            }
        }
        if reservation.transport_retired && reservation.application.is_none() {
            source.reservations.remove(&item.reservation_id);
        }
        self.recovery.remove(work_id);
        self.completed_recovery
            .insert(work_id.into(), caller.into());
        self.recovery_revision += 1;
        self.collect_retired_sources();
        Reply::Acknowledged {}
    }

    fn revoke_client(&mut self, caller: &str, client_id: &str) -> Reply {
        let scope = FixtureScope::new(caller, client_id);
        self.recovery_intents(&scope);
        for source in self.sources.values_mut() {
            if source.owner == scope {
                source.role_retained = false;
            }
            source.reservations.retain(|_, reservation| {
                reservation.scope != scope
                    || reservation.binding.is_some()
                    || reservation.application.is_some()
            });
        }
        self.collect_retired_sources();
        if self.sources.values().any(|source| {
            source
                .reservations
                .values()
                .any(|reservation| reservation.scope == scope)
        }) {
            Reply::Pending { retry_after_ms: 1 }
        } else {
            Reply::Acknowledged {}
        }
    }

    fn handle(&mut self, caller: &str, request: Request) -> Reply {
        let value = serde_json::to_value(&request).unwrap();
        self.observed_methods
            .insert(value["action"].as_str().unwrap().into());
        match request {
            Request::Describe {} => Reply::Described {
                interface_major: 1,
                interface_minor: 0,
            },
            Request::BeginProject {
                context,
                catalog_revision,
            } => {
                if self.projections.len() >= session_presentation::MAX_PROJECTIONS {
                    return fixture_error("projection_capacity");
                }
                let projection_id = self.allocate("projection");
                self.projections.insert(
                    projection_id.clone(),
                    FixtureProjection {
                        scope: FixtureScope::new(caller, &context.client.id),
                        context,
                        catalog_revision,
                        catalog: Vec::new(),
                        projected: None,
                    },
                );
                Reply::Started { projection_id }
            }
            Request::AppendProject {
                client_id,
                projection_id,
                entries,
                final_page,
            } => {
                let Some(projection) = self.projections.get_mut(&projection_id) else {
                    return fixture_error("projection_missing");
                };
                if projection.scope != FixtureScope::new(caller, &client_id) {
                    return fixture_error("projection_owner");
                }
                if projection.projected.is_some()
                    || projection.catalog.len() + entries.len()
                        > session_presentation::MAX_CATALOG_ENTRIES
                {
                    return fixture_error("projection_closed");
                }
                projection.catalog.extend(entries);
                if final_page {
                    self.catalog
                        .insert(client_id.clone(), projection.catalog.clone());
                    self.catalog_revision
                        .insert(client_id, projection.catalog_revision.clone());
                    let visible = projection
                        .catalog
                        .iter()
                        .filter(|entry| entry.visible)
                        .cloned();
                    let controls = [self.control.clone(), self.alias.clone()];
                    projection.projected = Some(match self.strategy {
                        FixtureStrategy::Immediate => controls.into_iter().chain(visible).collect(),
                        FixtureStrategy::Staged => visible.chain(controls).collect(),
                    });
                }
                Reply::Appended {
                    complete: final_page,
                }
            }
            Request::ReadProject {
                client_id,
                projection_id,
                offset,
                limit,
            } => {
                let Some(projection) = self.projections.get(&projection_id) else {
                    return fixture_error("projection_missing");
                };
                if projection.scope != FixtureScope::new(caller, &client_id) {
                    return fixture_error("projection_owner");
                }
                let Some(entries) = &projection.projected else {
                    return fixture_error("projection_incomplete");
                };
                if offset > entries.len() {
                    return fixture_error("projection_offset");
                }
                let end = (offset + limit).min(entries.len());
                Reply::Projected {
                    presentation: self.presentation(&projection.context),
                    entries: entries[offset..end].to_vec(),
                    next_offset: (end < entries.len()).then_some(end),
                }
            }
            Request::ReleaseProject {
                client_id,
                projection_id,
            } => {
                if self
                    .projections
                    .get(&projection_id)
                    .is_some_and(|projection| {
                        projection.scope != FixtureScope::new(caller, &client_id)
                    })
                {
                    return fixture_error("projection_owner");
                }
                self.projections.remove(&projection_id);
                Reply::Acknowledged {}
            }
            Request::Info { context } => {
                if let Some(game) = &context.game
                    && game.owner_client_id == context.client.id
                    && let Some(root) = &game.source
                    && let Some(source) = self.sources.get_mut(&root.session_id)
                    && source.matches(root)
                    && source
                        .reservations
                        .get(&root.reservation_id)
                        .is_some_and(|reservation| {
                            reservation.scope == FixtureScope::new(caller, &context.client.id)
                                && reservation.started
                                && reservation.binding.is_some()
                        })
                {
                    source.announced_game = Some(game.clone());
                }
                Reply::Info {
                    presentation: self.presentation(&context),
                }
            }
            request @ Request::Dispatch { .. } => self.dispatch(caller, request),
            Request::CancelDispatch {
                client_id,
                operation_id,
            } => self.cancel_operation(&FixtureScope::new(caller, &client_id), &operation_id),
            Request::VerifySource { client_id, source } => {
                self.verify(&FixtureScope::new(caller, &client_id), &source)
            }
            Request::BindTransport {
                client_id,
                session_id,
                generation,
                reservation_id,
                transport_id,
                transport_epoch,
            } => self.bind(
                caller,
                Observation {
                    client_id,
                    session_id,
                    generation,
                    reservation_id,
                    transport_id,
                    transport_epoch: Some(transport_epoch),
                    event: SessionEvent::StreamStarted,
                },
            ),
            Request::BindApplication {
                client_id,
                session_id,
                generation,
                reservation_id,
                application_operation_id,
                application_epoch,
                application_generation,
            } => {
                let Some(source) = self.sources.get(&session_id) else {
                    return fixture_error("source_retired");
                };
                if source.generation != generation {
                    return fixture_error("source_changed");
                }
                let shared = source.shared(&reservation_id);
                self.bind_application(
                    caller,
                    shared,
                    &client_id,
                    FixtureApplicationBinding {
                        operation: application_operation_id,
                        epoch: application_epoch,
                        generation: application_generation,
                    },
                )
            }
            Request::Observe {
                client_id,
                session_id,
                generation,
                reservation_id,
                transport_id,
                transport_epoch,
                event,
            } => self.observe(
                caller,
                Observation {
                    client_id,
                    session_id,
                    generation,
                    reservation_id,
                    transport_id,
                    transport_epoch,
                    event,
                },
            ),
            Request::RevokeClient { client_id } => self.revoke_client(caller, &client_id),
            Request::RecoveryWork { cursor, limit } => self.list_recovery(caller, cursor, limit),
            Request::ReconcileRecovery { work_id } => self.reconcile_recovery(caller, &work_id),
            Request::Artwork { context, selection } => {
                let artwork = if selection.id == self.control.id {
                    Artwork::InlinePng {
                        bytes: fixture_png(),
                    }
                } else if selection.id == self.alias.id {
                    let Some(game) = &context.game else {
                        return fixture_error("stale_game");
                    };
                    if game.application.uuid != self.alias.uuid {
                        return fixture_error("stale_game");
                    }
                    Artwork::Application {
                        application_id: game.application.id.clone(),
                    }
                } else {
                    Artwork::Unavailable
                };
                Reply::Artwork { artwork }
            }
        }
    }

    fn completion_lost(&mut self, caller: &str, request: &Request, reply: &Reply) {
        match (request, reply) {
            (Request::BeginProject { context, .. }, Reply::Started { projection_id }) => {
                if self
                    .projections
                    .get(projection_id)
                    .is_some_and(|projection| {
                        projection.scope == FixtureScope::new(caller, &context.client.id)
                    })
                {
                    self.projections.remove(projection_id);
                }
            }
            (
                Request::Dispatch {
                    context,
                    operation_id,
                    ..
                },
                _,
            ) => {
                assert!(matches!(
                    self.cancel_operation(
                        &FixtureScope::new(caller, &context.client.id),
                        operation_id
                    ),
                    Reply::Acknowledged {}
                ));
            }
            _ => {}
        }
    }
}

struct ContractWorkflow {
    broker: Broker,
    provider: &'static str,
    implementation: StatefulFixture,
    call_sequence: u64,
}

impl ContractWorkflow {
    fn new(provider: &'static str, strategy: FixtureStrategy) -> Self {
        let mut broker = selected_broker(provider);
        register(&mut broker, provider);
        Self {
            broker,
            provider,
            implementation: StatefulFixture::new(strategy),
            call_sequence: 0,
        }
    }

    fn admit(&mut self, caller: &str, request: &Request) -> u64 {
        self.call_sequence += 1;
        let operation = format!("contract-call-{}", self.call_sequence);
        let command = session_presentation::command(
            RequestOptions {
                service: SERVICE,
                timeout_ms: 1_000,
                operation_id: &operation,
                parent: None,
            },
            request,
        )
        .unwrap();
        accepted(self.broker.dispatch(caller, command))
    }

    fn deliver(&mut self, caller: &str, id: u64, expected: &Request) -> Reply {
        let Response::Request {
            id: delivered,
            caller: actual,
            service,
            payload,
            ..
        } = self.broker.dispatch(self.provider, Command::Poll {})
        else {
            panic!("selected provider did not receive contract request");
        };
        assert_eq!(delivered, id);
        assert_eq!(actual, caller);
        let request = session_presentation::decode(&service, payload).unwrap();
        assert_eq!(&request, expected);
        let reply = self.implementation.handle(&actual, request);
        reply.validate().unwrap();
        reply
    }

    fn call_as(&mut self, caller: &str, request: Request) -> Reply {
        let id = self.admit(caller, &request);
        let reply = self.deliver(caller, id, &request);
        assert_ok(self.broker.dispatch(
            self.provider,
            Command::Complete {
                request: id,
                payload: serde_json::to_value(&reply).unwrap(),
            },
        ));
        let Response::Result { payload } = self
            .broker
            .dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("consumer did not receive contract result");
        };
        let consumed = session_presentation::decode_reply(payload).unwrap();
        assert_eq!(consumed, reply);
        assert_ok(
            self.broker
                .dispatch(caller, Command::ReleaseResult { request: id }),
        );
        consumed
    }

    fn call(&mut self, request: Request) -> Reply {
        self.call_as(CONSUMER, request)
    }

    fn dispatch(&mut self, request: Request) -> Decision {
        for _ in 0..3 {
            match self.call(request.clone()) {
                Reply::Pending { retry_after_ms } => assert_eq!(retry_after_ms, 1),
                Reply::Dispatched { decision } => return decision,
                reply => panic!("dispatch failed: {reply:?}"),
            }
        }
        panic!("provider did not settle its bounded admission");
    }

    fn cancel_after_effect(&mut self, request: Request) {
        let id = self.admit(CONSUMER, &request);
        let reply = self.deliver(CONSUMER, id, &request);
        assert_ok(
            self.broker
                .dispatch(CONSUMER, Command::Cancel { request: id }),
        );
        assert_error(
            self.broker.dispatch(
                self.provider,
                Command::Complete {
                    request: id,
                    payload: serde_json::to_value(&reply).unwrap(),
                },
            ),
            "request_ended",
        );
        self.implementation
            .completion_lost(CONSUMER, &request, &reply);
        assert_error(
            self.broker
                .dispatch(CONSUMER, Command::TakeResult { request: id }),
            "cancelled",
        );
        assert_ok(
            self.broker
                .dispatch(CONSUMER, Command::ReleaseResult { request: id }),
        );
    }
}

fn expect_contract_error(reply: Reply, code: &str) {
    assert!(
        matches!(&reply, Reply::Error { code: actual, .. } if actual == code),
        "expected {code}, received {reply:?}"
    );
}

fn expect_ack(reply: Reply) {
    assert!(matches!(reply, Reply::Acknowledged {}), "{reply:?}");
}

fn selection(application: &Application) -> session_presentation::Selection {
    session_presentation::Selection {
        id: application.id.clone(),
        uuid: Some(application.uuid.clone()),
    }
}

fn requested_mode() -> Mode {
    Mode {
        width: 1280,
        height: 720,
        refresh_millihz: 60_000,
        hdr: true,
    }
}

fn dispatch_request(
    context: &Context,
    operation: &str,
    entry: &Application,
    intent: DispatchIntent,
) -> Request {
    Request::Dispatch {
        context: context.clone(),
        operation_id: operation.into(),
        intent,
        selection: Some(selection(entry)),
        mode: Some(requested_mode()),
    }
}

fn as_shared(plan: &AuxiliaryStreamPlan) -> SharedCapture {
    SharedCapture {
        session_id: plan.session_id.clone(),
        generation: plan.generation,
        reservation_id: plan.reservation_id.clone(),
        capture: plan.capture.clone(),
        mode: plan
            .source_mode
            .expect("exact fixture source has an observed mode"),
    }
}

fn observation(
    client: &str,
    source: &SharedCapture,
    transport: &str,
    epoch: &str,
    event: SessionEvent,
) -> Observation {
    Observation {
        client_id: client.into(),
        session_id: source.session_id.clone(),
        generation: source.generation,
        reservation_id: source.reservation_id.clone(),
        transport_id: transport.into(),
        transport_epoch: Some(epoch.into()),
        event,
    }
}

fn bind_transport(client: &str, source: &SharedCapture, transport: &str, epoch: &str) -> Request {
    Request::BindTransport {
        client_id: client.into(),
        session_id: source.session_id.clone(),
        generation: source.generation,
        reservation_id: source.reservation_id.clone(),
        transport_id: transport.into(),
        transport_epoch: epoch.into(),
    }
}

fn bind_application(
    client: &str,
    source: &SharedCapture,
    evidence: &FixtureApplicationBinding,
) -> Request {
    Request::BindApplication {
        client_id: client.into(),
        session_id: source.session_id.clone(),
        generation: source.generation,
        reservation_id: source.reservation_id.clone(),
        application_operation_id: evidence.operation.clone(),
        application_epoch: evidence.epoch.clone(),
        application_generation: evidence.generation,
    }
}

fn verify_source(client: &str, source: &SharedCapture) -> Request {
    Request::VerifySource {
        client_id: client.into(),
        source: source.clone(),
    }
}

fn expect_verified(workflow: &mut ContractWorkflow, client: &str, source: &SharedCapture) {
    assert_eq!(
        workflow.call(verify_source(client, source)),
        Reply::Verified {
            source: source.clone()
        }
    );
}

fn catalog_entries() -> Vec<Application> {
    let mut entries = vec![context().game.unwrap().application];
    entries.extend((1..=37).map(|number| {
        let mut entry =
            fixture_application(&format!("catalog-{number}"), &format!("Game {number}"));
        entry.visible = number % 7 != 0;
        entry.aliases = vec![format!("old-id-{number}")];
        entry
    }));
    entries
}

fn project_catalog(
    workflow: &mut ContractWorkflow,
    context: &Context,
    catalog: &[Application],
) -> Vec<Application> {
    let Reply::Started { projection_id } = workflow.call(Request::BeginProject {
        context: context.clone(),
        catalog_revision: "catalog-revision-1".into(),
    }) else {
        panic!("projection admission");
    };
    let read = |offset, limit| Request::ReadProject {
        client_id: context.client.id.clone(),
        projection_id: projection_id.clone(),
        offset,
        limit,
    };
    expect_contract_error(workflow.call(read(0, 4)), "projection_incomplete");
    for (index, chunk) in catalog
        .chunks(session_presentation::MAX_PAGE_ENTRIES)
        .enumerate()
    {
        let final_page = (index + 1) * session_presentation::MAX_PAGE_ENTRIES >= catalog.len();
        assert_eq!(
            workflow.call(Request::AppendProject {
                client_id: context.client.id.clone(),
                projection_id: projection_id.clone(),
                entries: chunk.to_vec(),
                final_page
            }),
            Reply::Appended {
                complete: final_page
            }
        );
        if !final_page {
            expect_contract_error(workflow.call(read(0, 4)), "projection_incomplete");
        }
    }
    let mut entries = Vec::new();
    let mut offset = 0;
    loop {
        let Reply::Projected {
            presentation,
            entries: page,
            next_offset,
        } = workflow.call(read(offset, 4))
        else {
            panic!("projected read");
        };
        presentation.validate().unwrap();
        assert!(!page.is_empty() && page.len() <= 4);
        entries.extend(page);
        let Some(next) = next_offset else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(
        entries.len(),
        catalog.iter().filter(|entry| entry.visible).count() + 2
    );
    assert!(
        catalog
            .iter()
            .filter(|entry| !entry.visible)
            .all(|hidden| entries.iter().all(|entry| entry.id != hidden.id))
    );
    expect_ack(workflow.call(Request::ReleaseProject {
        client_id: context.client.id.clone(),
        projection_id,
    }));
    entries
}

#[derive(Debug)]
struct ContractOutcome {
    control_id: String,
    projected_ids: Vec<String>,
    source: SharedCapture,
    presentation: Presentation,
    pending_replies: usize,
}

// The complete consumer workflow is reused verbatim. Selections, reservations,
// sources and synthetic aliases all come from provider replies; it never tests
// which provider principal was selected or recognizes a provider-owned ID.
fn unchanged_session_workflow(workflow: &mut ContractWorkflow) -> ContractOutcome {
    assert_eq!(
        workflow.call(Request::Describe {}),
        Reply::Described {
            interface_major: 1,
            interface_minor: 0
        }
    );
    let original_context = context();
    let client = original_context.client.id.clone();
    let catalog = catalog_entries();
    let projected = project_catalog(workflow, &original_context, &catalog);
    let control = projected
        .iter()
        .find(|entry| entry.monitor_profile)
        .unwrap()
        .clone();
    let application = projected
        .iter()
        .find(|entry| entry.id == catalog[0].id)
        .unwrap()
        .clone();
    let alias = projected
        .iter()
        .find(|entry| entry.uuid == application.uuid && entry.id != application.id)
        .unwrap()
        .clone();
    let Reply::Info { presentation } = workflow.call(Request::Info {
        context: original_context.clone(),
    }) else {
        panic!("info reply");
    };
    assert_eq!(
        workflow.call(Request::Artwork {
            context: original_context.clone(),
            selection: selection(&control)
        }),
        Reply::Artwork {
            artwork: Artwork::InlinePng {
                bytes: fixture_png()
            }
        }
    );
    assert_eq!(
        workflow.call(Request::Artwork {
            context: original_context.clone(),
            selection: selection(&alias)
        }),
        Reply::Artwork {
            artwork: Artwork::Application {
                application_id: application.id.clone()
            }
        }
    );
    assert_eq!(
        workflow.call(Request::Artwork {
            context: original_context.clone(),
            selection: selection(&fixture_application("missing", "Missing"))
        }),
        Reply::Artwork {
            artwork: Artwork::Unavailable
        }
    );

    let auxiliary_request = dispatch_request(
        &original_context,
        "attach-display",
        &control,
        DispatchIntent::Launch,
    );
    let Decision::Auxiliary { plan } = workflow.dispatch(auxiliary_request.clone()) else {
        panic!("auxiliary plan");
    };
    assert_eq!(
        workflow.dispatch(auxiliary_request),
        Decision::Auxiliary { plan: plan.clone() }
    );
    let auxiliary = as_shared(&plan);
    expect_verified(workflow, &client, &auxiliary);
    let mut stale_generation = auxiliary.clone();
    stale_generation.generation += 1;
    expect_contract_error(
        workflow.call(verify_source(&client, &stale_generation)),
        "source_changed",
    );
    let mut wrong_reservation = auxiliary.clone();
    wrong_reservation.reservation_id = "another-reservation".into();
    expect_contract_error(
        workflow.call(verify_source(&client, &wrong_reservation)),
        "source_changed",
    );
    let mut wrong_mode = auxiliary.clone();
    wrong_mode.mode.width += 16;
    expect_contract_error(
        workflow.call(verify_source(&client, &wrong_mode)),
        "source_changed",
    );
    let mut wrong_route = auxiliary.clone();
    if let CaptureSource::ExactOutput { route_key, .. } = &mut wrong_route.capture {
        *route_key = "unselected-capture-route".into();
    }
    expect_contract_error(
        workflow.call(verify_source(&client, &wrong_route)),
        "source_changed",
    );
    expect_contract_error(
        workflow.call(verify_source("different-client", &auxiliary)),
        "source_changed",
    );
    expect_contract_error(
        workflow.call_as(SECOND_CONSUMER, verify_source(&client, &auxiliary)),
        "source_changed",
    );
    expect_contract_error(
        workflow.call(Request::from(observation(
            &client,
            &auxiliary,
            "aux-transport",
            "transport-epoch-1",
            SessionEvent::StreamStarted,
        ))),
        "transport_not_bound",
    );
    expect_contract_error(
        workflow.call(bind_transport(
            "different-client",
            &auxiliary,
            "aux-transport",
            "transport-epoch-1",
        )),
        "reservation_owner",
    );
    expect_ack(workflow.call(bind_transport(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
    )));
    expect_ack(workflow.call(bind_transport(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
    )));
    expect_contract_error(
        workflow.call(bind_transport(
            &client,
            &auxiliary,
            "aux-transport",
            "replacement-epoch",
        )),
        "transport_binding_changed",
    );
    expect_contract_error(
        workflow.call(Request::from(observation(
            &client,
            &auxiliary,
            "aux-transport",
            "replacement-epoch",
            SessionEvent::StreamStarted,
        ))),
        "transport_binding_changed",
    );
    let mut missing_epoch = observation(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
        SessionEvent::StreamStarted,
    );
    missing_epoch.transport_epoch = None;
    expect_contract_error(
        workflow.call(Request::from(missing_epoch)),
        "transport_binding_changed",
    );
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
        SessionEvent::StreamStarted,
    ))));

    let mut view_only = original_context.clone();
    view_only.client.may_launch = false;
    assert!(matches!(
        workflow.dispatch(dispatch_request(
            &view_only,
            "view-cannot-create",
            &control,
            DispatchIntent::Launch
        )),
        Decision::Reject {
            status_code: 403,
            ..
        }
    ));
    let Decision::ResumeAuxiliary { plan: resumed } = workflow.dispatch(dispatch_request(
        &view_only,
        "view-can-resume",
        &control,
        DispatchIntent::Resume,
    )) else {
        panic!("retained role did not resume under view permission");
    };
    assert_eq!(resumed.session_id, plan.session_id);
    assert_ne!(resumed.reservation_id, plan.reservation_id);
    expect_ack(workflow.call(Request::CancelDispatch {
        client_id: client.clone(),
        operation_id: "view-can-resume".into(),
    }));
    let mut fresh_view_only = view_only;
    fresh_view_only.client.id = "view-only-new-client".into();
    expect_contract_error(
        workflow.call(dispatch_request(
            &fresh_view_only,
            "view-cannot-resume-missing",
            &control,
            DispatchIntent::Resume,
        )),
        "no_retained_role",
    );

    let mut launching = original_context.clone();
    launching.game = None;
    let Decision::LaunchApplication {
        application_id,
        source: Some(root),
    } = workflow.dispatch(dispatch_request(
        &launching,
        "game-live",
        &application,
        DispatchIntent::Launch,
    ))
    else {
        panic!("normal application did not borrow retained source");
    };
    assert_eq!(application_id, application.id);
    assert_ne!(root.reservation_id, auxiliary.reservation_id);
    assert_eq!(root.session_id, auxiliary.session_id);
    let Decision::LaunchApplication {
        source: Some(pending),
        ..
    } = workflow.dispatch(dispatch_request(
        &launching,
        "game-pending",
        &application,
        DispatchIntent::Launch,
    ))
    else {
        panic!("pending application did not reserve source");
    };
    let app_binding = FixtureApplicationBinding {
        operation: "execution-op-root".into(),
        epoch: "execution-epoch-1".into(),
        generation: 91,
    };
    expect_ack(workflow.call(bind_application(&client, &root, &app_binding)));
    expect_ack(workflow.call(bind_application(&client, &root, &app_binding)));
    let mut stale_app = app_binding.clone();
    stale_app.epoch = "unrelated-execution-epoch".into();
    expect_contract_error(
        workflow.call(bind_application(&client, &root, &stale_app)),
        "application_binding_changed",
    );
    expect_ack(workflow.call(bind_transport(
        &client,
        &root,
        "game-transport",
        "transport-epoch-1",
    )));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &root,
        "game-transport",
        "transport-epoch-1",
        SessionEvent::StreamStarted,
    ))));

    let mut owner_game = launching.clone();
    owner_game.game = Some(Game {
        application: application.clone(),
        owner_client_id: client.clone(),
        generation: app_binding.generation,
        source: Some(root.clone()),
    });
    assert!(matches!(
        workflow.call(Request::Info {
            context: owner_game.clone()
        }),
        Reply::Info { .. }
    ));
    let mut joining = owner_game.clone();
    joining.client.id = "paired-secondary-client".into();
    let secondary_catalog = project_catalog(workflow, &joining, &catalog);
    let secondary_alias = secondary_catalog
        .iter()
        .find(|entry| entry.uuid == application.uuid && entry.id != application.id)
        .unwrap()
        .clone();
    let Decision::ResumeApplication {
        application_id: resumed_app,
        source: Some(secondary),
    } = workflow.dispatch(dispatch_request(
        &joining,
        "join-current-game",
        &secondary_alias,
        DispatchIntent::Resume,
    ))
    else {
        panic!("secondary game attach");
    };
    assert_eq!(resumed_app, application.id);
    assert_eq!(secondary.session_id, root.session_id);
    assert_ne!(secondary.reservation_id, root.reservation_id);
    expect_contract_error(
        workflow.call(verify_source(&joining.client.id, &root)),
        "source_changed",
    );
    expect_verified(workflow, &joining.client.id, &secondary);
    expect_ack(workflow.call(bind_transport(
        &joining.client.id,
        &secondary,
        "secondary-transport",
        "transport-epoch-2",
    )));
    expect_ack(workflow.call(Request::from(observation(
        &joining.client.id,
        &secondary,
        "secondary-transport",
        "transport-epoch-2",
        SessionEvent::StreamStarted,
    ))));
    let mut stale_game = joining.clone();
    stale_game.game.as_mut().unwrap().generation += 1;
    expect_contract_error(
        workflow.call(dispatch_request(
            &stale_game,
            "join-stale-generation",
            &secondary_alias,
            DispatchIntent::Resume,
        )),
        "game_source_changed",
    );

    // The auxiliary transport ends and its retained role disconnects. Pending
    // admission, the running application and a secondary transport still each
    // independently own their reservations of this exact output.
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
        SessionEvent::StreamEnded,
    ))));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &auxiliary,
        "aux-transport",
        "transport-epoch-1",
        SessionEvent::ExplicitDisconnect,
    ))));
    expect_verified(workflow, &client, &pending);
    expect_verified(workflow, &client, &root);
    expect_verified(workflow, &joining.client.id, &secondary);
    expect_ack(workflow.call(Request::CancelDispatch {
        client_id: client.clone(),
        operation_id: "game-pending".into(),
    }));
    expect_contract_error(
        workflow.call(verify_source(&client, &pending)),
        "source_changed",
    );
    expect_ack(workflow.call(Request::from(observation(
        &joining.client.id,
        &secondary,
        "secondary-transport",
        "transport-epoch-2",
        SessionEvent::StreamEnded,
    ))));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &root,
        "game-transport",
        "transport-epoch-1",
        SessionEvent::StreamEnded,
    ))));
    // Closing the root's RTSP transport can pause its game. Its independent
    // execution binding is still held and cannot be replaced by a new epoch.
    expect_verified(workflow, &client, &root);
    assert!(
        !workflow
            .implementation
            .execution_retired(&FixtureScope::new(CONSUMER, &client), &stale_app)
    );
    expect_verified(workflow, &client, &root);
    assert!(
        workflow
            .implementation
            .execution_retired(&FixtureScope::new(CONSUMER, &client), &app_binding)
    );
    expect_contract_error(
        workflow.call(verify_source(&client, &root)),
        "source_retired",
    );
    assert!(workflow.implementation.sources.is_empty());

    let failed = dispatch_request(&launching, "failed-start", &control, DispatchIntent::Launch);
    let Decision::Auxiliary { plan: failed_plan } = workflow.dispatch(failed.clone()) else {
        panic!("failure fixture admission");
    };
    let failed_source = as_shared(&failed_plan);
    assert!(failed_source.generation > auxiliary.generation);
    expect_ack(workflow.call(bind_transport(
        &client,
        &failed_source,
        "failed-transport",
        "transport-epoch-3",
    )));
    expect_contract_error(
        workflow.call(Request::CancelDispatch {
            client_id: client.clone(),
            operation_id: "failed-start".into(),
        }),
        "transport_must_retire",
    );
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &failed_source,
        "failed-transport",
        "transport-epoch-3",
        SessionEvent::FailedStart,
    ))));
    expect_ack(workflow.call(Request::CancelDispatch {
        client_id: client.clone(),
        operation_id: "failed-start".into(),
    }));
    expect_contract_error(workflow.call(failed), "dispatch_cancelled");
    assert!(workflow.implementation.sources.is_empty());

    let cancelled = dispatch_request(
        &launching,
        "cancel-before-bind",
        &control,
        DispatchIntent::Launch,
    );
    assert!(matches!(
        workflow.call(cancelled.clone()),
        Reply::Pending { .. }
            | Reply::Dispatched {
                decision: Decision::Auxiliary { .. }
            }
    ));
    expect_ack(workflow.call(Request::CancelDispatch {
        client_id: client.clone(),
        operation_id: "cancel-before-bind".into(),
    }));
    expect_contract_error(workflow.call(cancelled), "dispatch_cancelled");
    workflow.cancel_after_effect(dispatch_request(
        &launching,
        "lost-dispatch-completion",
        &control,
        DispatchIntent::Launch,
    ));
    assert!(workflow.implementation.sources.is_empty());

    // An idle retained display has no live transport tuple. RevokeClient must
    // retire it without fabricating session IDs, generations or cleanup events.
    let Decision::Auxiliary { plan: idle } = workflow.dispatch(dispatch_request(
        &launching,
        "retain-idle",
        &control,
        DispatchIntent::Launch,
    )) else {
        panic!("idle fixture admission");
    };
    let idle = as_shared(&idle);
    expect_ack(workflow.call(bind_transport(
        &client,
        &idle,
        "idle-transport",
        "transport-epoch-4",
    )));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &idle,
        "idle-transport",
        "transport-epoch-4",
        SessionEvent::StreamStarted,
    ))));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &idle,
        "idle-transport",
        "transport-epoch-4",
        SessionEvent::ClientDisconnected,
    ))));
    assert_eq!(workflow.implementation.sources.len(), 1);
    expect_ack(workflow.call(Request::RevokeClient { client_id: client }));
    assert!(workflow.implementation.sources.is_empty());
    assert!(workflow.implementation.projections.is_empty());
    unchanged_recovery_workflow(workflow, &launching, &control, &application);
    assert_eq!(
        workflow.implementation.observed_methods,
        session_presentation::methods()
            .into_iter()
            .map(|method| method.method)
            .collect()
    );
    ContractOutcome {
        control_id: control.id,
        projected_ids: projected.into_iter().map(|entry| entry.id).collect(),
        source: auxiliary,
        presentation,
        pending_replies: workflow.implementation.pending_replies,
    }
}

fn unchanged_recovery_workflow(
    workflow: &mut ContractWorkflow,
    context: &Context,
    control: &Application,
    application: &Application,
) {
    let client = context.client.id.clone();
    let Decision::Auxiliary { plan } = workflow.dispatch(dispatch_request(
        context,
        "recovery-auxiliary",
        control,
        DispatchIntent::Launch,
    )) else {
        panic!("orphan fixture source admission");
    };
    let auxiliary = as_shared(&plan);
    expect_ack(workflow.call(bind_transport(
        &client,
        &auxiliary,
        "orphan-aux-transport",
        "orphan-transport-epoch",
    )));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &auxiliary,
        "orphan-aux-transport",
        "orphan-transport-epoch",
        SessionEvent::StreamStarted,
    ))));
    let Decision::LaunchApplication {
        source: Some(root), ..
    } = workflow.dispatch(dispatch_request(
        context,
        "recovery-application",
        application,
        DispatchIntent::Launch,
    ))
    else {
        panic!("orphan application admission");
    };
    let execution = FixtureApplicationBinding {
        operation: "orphan-application-operation".into(),
        epoch: "orphan-execution-epoch".into(),
        generation: 112,
    };
    expect_ack(workflow.call(bind_application(&client, &root, &execution)));
    expect_ack(workflow.call(bind_transport(
        &client,
        &root,
        "orphan-game-transport",
        "orphan-transport-epoch",
    )));
    expect_ack(workflow.call(Request::from(observation(
        &client,
        &root,
        "orphan-game-transport",
        "orphan-transport-epoch",
        SessionEvent::StreamStarted,
    ))));
    // Revocation persists bounded exact retirement intents, and cannot turn a
    // still-running transport or application into a completed cleanup receipt.
    assert!(matches!(
        workflow.call(Request::RevokeClient {
            client_id: client.clone()
        }),
        Reply::Pending { .. }
    ));
    let Reply::RecoveryWork {
        items: first,
        next_cursor: Some(first_cursor),
    } = workflow.call(Request::RecoveryWork {
        cursor: None,
        limit: 1,
    })
    else {
        panic!("bounded recovery page");
    };
    assert_eq!(first.len(), 1);
    expect_contract_error(
        workflow.call_as(
            SECOND_CONSUMER,
            Request::RecoveryWork {
                cursor: None,
                limit: 1,
            },
        ),
        "recovery_owner",
    );
    expect_contract_error(
        workflow.call_as(
            SECOND_CONSUMER,
            Request::RecoveryWork {
                cursor: Some(first_cursor.clone()),
                limit: 1,
            },
        ),
        "recovery_owner",
    );
    expect_contract_error(
        workflow.call_as(
            SECOND_CONSUMER,
            Request::ReconcileRecovery {
                work_id: first[0].id.clone(),
            },
        ),
        "recovery_owner",
    );
    expect_contract_error(
        workflow.call(Request::ReconcileRecovery {
            work_id: "not-a-persisted-work-id".into(),
        }),
        "invalid_recovery_work",
    );
    let mut injected = session_presentation::command(
        RequestOptions {
            service: SERVICE,
            timeout_ms: 1_000,
            operation_id: "recovery-cannot-route-arbitrary-service",
            parent: None,
        },
        &Request::ReconcileRecovery {
            work_id: first[0].id.clone(),
        },
    )
    .unwrap();
    let Command::CallApi { payload, .. } = &mut injected else {
        unreachable!();
    };
    payload["service"] = json!("arbitrary.native-owner/1");
    payload["payload"] = json!({"action":"pretend_cleanup_succeeded"});
    assert_error(
        workflow.broker.dispatch(CONSUMER, injected),
        "invalid_request",
    );
    assert!(matches!(
        workflow
            .broker
            .dispatch(workflow.provider, Command::Poll {}),
        Response::Pending
    ));
    for _ in 0..2 {
        assert!(matches!(
            workflow.call(Request::ReconcileRecovery {
                work_id: first[0].id.clone()
            }),
            Reply::Pending { .. }
        ));
        let Reply::RecoveryWork { items, .. } = workflow.call(Request::RecoveryWork {
            cursor: None,
            limit: 1,
        }) else {
            panic!("unfinished retirement disappeared");
        };
        assert_eq!(items, first);
        expect_verified(workflow, &client, &root);
    }
    let mut work = first;
    let mut continuation = Some(first_cursor.clone());
    while let Some(cursor) = continuation {
        let Reply::RecoveryWork { items, next_cursor } = workflow.call(Request::RecoveryWork {
            cursor: Some(cursor),
            limit: 1,
        }) else {
            panic!("caller-bound recovery continuation");
        };
        assert_eq!(items.len(), 1);
        work.extend(items);
        continuation = next_cursor;
    }
    assert_eq!(work.len(), 3);
    assert_eq!(
        work.iter()
            .filter(|item| matches!(item.binding, RecoveryBinding::Transport { .. }))
            .count(),
        2
    );
    assert_eq!(
        work.iter()
            .filter(|item| matches!(item.binding, RecoveryBinding::Application { .. }))
            .count(),
        1
    );
    let scope = FixtureScope::new(CONSUMER, &client);
    for (index, item) in work.iter().enumerate() {
        item.validate().unwrap();
        assert_eq!(item.session_id, root.session_id);
        assert_eq!(item.generation, root.generation);
        let mut wrong_epoch = item.binding.clone();
        match &mut wrong_epoch {
            RecoveryBinding::Transport { epoch, .. }
            | RecoveryBinding::Application { epoch, .. } => *epoch = "unrelated-owner-epoch".into(),
        }
        assert!(
            !workflow
                .implementation
                .selected_owner_retired(&scope, &wrong_epoch)
        );
        if let RecoveryBinding::Application {
            application_generation,
            ..
        } = &item.binding
        {
            let mut wrong_generation = item.binding.clone();
            if let RecoveryBinding::Application {
                application_generation,
                ..
            } = &mut wrong_generation
            {
                *application_generation += 1;
            }
            assert!(
                !workflow
                    .implementation
                    .selected_owner_retired(&scope, &wrong_generation)
            );
            assert_eq!(*application_generation, execution.generation);
        }
        assert!(matches!(
            workflow.call(Request::ReconcileRecovery {
                work_id: item.id.clone()
            }),
            Reply::Pending { .. }
        ));
        // The consumer chooses its own fixed authority adapter from the typed
        // binding, carrying only the returned exact operation reference. It
        // never invokes a service name or arbitrary payload from the provider.
        assert!(
            workflow
                .implementation
                .selected_owner_retired(&scope, &item.binding)
        );
        assert_eq!(workflow.implementation.sources.len(), 1);
        expect_ack(workflow.call(Request::ReconcileRecovery {
            work_id: item.id.clone(),
        }));
        expect_ack(workflow.call(Request::ReconcileRecovery {
            work_id: item.id.clone(),
        }));
        if index == 0 {
            expect_contract_error(
                workflow.call(Request::RecoveryWork {
                    cursor: Some(first_cursor.clone()),
                    limit: 1,
                }),
                "stale_recovery_cursor",
            );
        }
        let Reply::RecoveryWork { items, next_cursor } = workflow.call(Request::RecoveryWork {
            cursor: None,
            limit: session_presentation::MAX_RECOVERY_ITEMS,
        }) else {
            panic!("reconciled recovery list");
        };
        assert!(next_cursor.is_none());
        assert_eq!(items.len(), work.len() - index - 1);
        if index + 1 < work.len() {
            assert_eq!(workflow.implementation.sources.len(), 1);
        }
    }
    assert!(workflow.implementation.sources.is_empty());
    assert!(workflow.implementation.recovery.is_empty());
    expect_ack(workflow.call(Request::RevokeClient { client_id: client }));
}

#[test]
fn session_presentation_all_operations_survive_stateful_provider_replacement() {
    let mut immediate = ContractWorkflow::new("vendor.eager", FixtureStrategy::Immediate);
    let mut staged = ContractWorkflow::new("vendor.staged", FixtureStrategy::Staged);
    let first = unchanged_session_workflow(&mut immediate);
    let replacement = unchanged_session_workflow(&mut staged);
    assert_ne!(first.control_id, replacement.control_id);
    assert_ne!(first.projected_ids, replacement.projected_ids);
    assert_ne!(first.source.session_id, replacement.source.session_id);
    assert_ne!(first.source.capture, replacement.source.capture);
    assert_ne!(first.presentation, replacement.presentation);
    assert_eq!(first.pending_replies, 0);
    assert!(replacement.pending_replies > 0);
}

fn projection_ownership_and_cleanup(workflow: &mut ContractWorkflow) {
    let original = context();
    let client = original.client.id.clone();
    let begin = || Request::BeginProject {
        context: original.clone(),
        catalog_revision: "bounded-snapshot".into(),
    };
    let Reply::Started { projection_id } = workflow.call(begin()) else {
        panic!("begin fixture");
    };
    let read = |client_id: &str| Request::ReadProject {
        client_id: client_id.into(),
        projection_id: projection_id.clone(),
        offset: 0,
        limit: 2,
    };
    expect_contract_error(
        workflow.call(read("not-the-authenticated-client")),
        "projection_owner",
    );
    expect_contract_error(
        workflow.call_as(SECOND_CONSUMER, read(&client)),
        "projection_owner",
    );
    expect_contract_error(
        workflow.call(Request::AppendProject {
            client_id: "not-the-authenticated-client".into(),
            projection_id: projection_id.clone(),
            entries: Vec::new(),
            final_page: true,
        }),
        "projection_owner",
    );
    expect_contract_error(
        workflow.call_as(
            SECOND_CONSUMER,
            Request::ReleaseProject {
                client_id: client.clone(),
                projection_id: projection_id.clone(),
            },
        ),
        "projection_owner",
    );
    expect_contract_error(workflow.call(read(&client)), "projection_incomplete");
    expect_ack(workflow.call(Request::ReleaseProject {
        client_id: client.clone(),
        projection_id,
    }));
    assert!(workflow.implementation.projections.is_empty());

    // A caller can abandon its connection while a delivered provider request
    // is allocating a transaction. Lost completion must undo that allocation.
    workflow.cancel_after_effect(begin());
    assert!(workflow.implementation.projections.is_empty());
    let undelivered = workflow.admit(CONSUMER, &begin());
    assert_ok(workflow.broker.dispatch(
        CONSUMER,
        Command::Cancel {
            request: undelivered,
        },
    ));
    assert_ok(workflow.broker.dispatch(
        CONSUMER,
        Command::ReleaseResult {
            request: undelivered,
        },
    ));
    assert!(matches!(
        workflow
            .broker
            .dispatch(workflow.provider, Command::Poll {}),
        Response::Pending
    ));
    assert!(workflow.implementation.projections.is_empty());

    let mut retained = Vec::new();
    for _ in 0..session_presentation::MAX_PROJECTIONS {
        let Reply::Started { projection_id } = workflow.call(begin()) else {
            panic!("bounded transaction admission");
        };
        retained.push(projection_id);
    }
    expect_contract_error(workflow.call(begin()), "projection_capacity");
    assert_eq!(
        workflow.implementation.projections.len(),
        session_presentation::MAX_PROJECTIONS
    );
    for projection_id in retained {
        expect_ack(workflow.call(Request::ReleaseProject {
            client_id: client.clone(),
            projection_id,
        }));
    }
    // More than 512 completed broker calls plus repeated transaction release
    // prove both the broker ledger and provider transaction bound are reusable.
    for _ in 0..300 {
        let Reply::Started { projection_id } = workflow.call(begin()) else {
            panic!("capacity was not retired");
        };
        expect_ack(workflow.call(Request::ReleaseProject {
            client_id: client.clone(),
            projection_id,
        }));
    }
    assert!(workflow.implementation.projections.is_empty());
    assert!(workflow.implementation.sources.is_empty());
}

#[test]
fn session_presentation_projection_authority_and_retirement_survive_replacement() {
    for (provider, strategy) in [
        ("vendor.eager", FixtureStrategy::Immediate),
        ("vendor.staged", FixtureStrategy::Staged),
    ] {
        projection_ownership_and_cleanup(&mut ContractWorkflow::new(provider, strategy));
    }
}
