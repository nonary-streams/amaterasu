//! Public fixed retirement adapters route unchanged consumers through real broker
//! grants to independently selected providers. Fixtures implement wire contracts;
//! native retirement and owner/observer authorization are provider tests.
use super::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::requests::RequestOptions;
use amaterasu_module_sdk::retirement::{self, Contract, OperationRef, State, Status};
use serde_json::{Value, json};

const CONSUMER: &str = "example.original-owner";
const OUTSIDER: &str = "example.ungranted";
const UNSELECTED: &str = "example.unselected-provider";
const EPOCH: &str = "0123456789abcdef0123456789abcdef";
const NEXT_EPOCH: &str = "fedcba9876543210fedcba9876543210";

fn reference(contract: Contract) -> OperationRef {
    OperationRef {
        operation_id: match contract {
            Contract::Transport => "original-transport-operation".into(),
            Contract::Application => format!("ale1:{EPOCH}:application-operation"),
        },
        epoch: EPOCH.into(),
    }
}

fn options(contract: Contract) -> RequestOptions<'static> {
    RequestOptions {
        service: contract.service(),
        timeout_ms: 1_000,
        operation_id: "exact-retirement-observation",
        parent: None,
    }
}

fn status_command(contract: Contract) -> Command {
    retirement::status_command(options(contract), contract, &reference(contract)).unwrap()
}

fn broker(provider: &str, contract: Contract) -> Broker {
    let service = contract.service();
    let configuration = RuntimeConfig {
        principals: [
            (
                CONSUMER.into(),
                Grants {
                    call: [service.into()].into(),
                    ..Default::default()
                },
            ),
            (
                provider.into(),
                Grants {
                    provide: [service.into()].into(),
                    ..Default::default()
                },
            ),
            (OUTSIDER.into(), Grants::default()),
            (UNSELECTED.into(), Grants::default()),
        ]
        .into(),
        ..Default::default()
    };
    let mut broker = Broker::new(configuration).unwrap();
    assert_ok(broker.dispatch(
        provider,
        Command::Register {
            service: service.into(),
        },
    ));
    broker
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

fn status_response(contract: Contract, state: &str, retired: bool) -> Value {
    let reference = reference(contract);
    let mut response = json!({
        "ok":true,"operation_id":reference.operation_id,
        "expected_runtime_epoch":reference.epoch,"runtime_epoch":EPOCH,
        "startup_reconciled":contract == Contract::Transport,
        "state":state,"retired":retired,
    });
    if contract == Contract::Transport {
        response["active_operations"] = json!(0);
        response["pending_admissions"] = json!(0);
        response["cleanup_pending"] = json!(false);
    }
    response
}

/// Identical consumer code for every selected provider: it understands only the
/// published SDK and the saved operation reference, with no provider branch.
fn consume(broker: &mut Broker, contract: Contract, request: u64) -> Result<Status, String> {
    let Response::Result { payload } = broker.dispatch(CONSUMER, Command::TakeResult { request })
    else {
        panic!("consumer did not receive the selected provider's result");
    };
    let result = retirement::parse_status(contract, &reference(contract), &payload);
    assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
    result
}

fn deliver(broker: &mut Broker, provider: &str, contract: Contract, response: Value) -> u64 {
    let command = status_command(contract);
    let expected_payload = match &command {
        Command::Request { payload, .. } => payload.clone(),
        _ => unreachable!(),
    };
    let Response::Accepted { id } = broker.dispatch(CONSUMER, command) else {
        panic!("fixed SDK request was not admitted");
    };
    let Response::Request {
        id: delivered,
        caller,
        initiator,
        service,
        payload,
        ..
    } = broker.dispatch(provider, Command::Poll {})
    else {
        panic!("selected provider did not receive the request");
    };
    assert_eq!(delivered, id);
    assert_eq!(caller, CONSUMER);
    assert_eq!(initiator, CONSUMER);
    assert_eq!(service, contract.service());
    assert_eq!(payload, expected_payload);
    assert_eq!(payload.as_object().unwrap().len(), 3);
    assert!(payload.get("owner").is_none());
    assert!(matches!(
        broker.dispatch(UNSELECTED, Command::Poll {}),
        Response::Pending
    ));
    assert_error(
        broker.dispatch(
            UNSELECTED,
            Command::Complete {
                request: id,
                payload: response.clone(),
            },
        ),
        "permission_denied",
    );
    assert_ok(broker.dispatch(
        provider,
        Command::Complete {
            request: id,
            payload: response,
        },
    ));
    id
}

#[test]
fn retirement_contract_consumer_survives_selected_provider_replacement() {
    for contract in [Contract::Transport, Contract::Application] {
        let unchanged_command = serde_json::to_value(status_command(contract)).unwrap();
        for (provider, state) in [
            ("vendor.first-retirement-provider", "retired"),
            ("vendor.replacement-retirement-provider", "unknown"),
        ] {
            let mut broker = broker(provider, contract);
            assert_eq!(
                serde_json::to_value(status_command(contract)).unwrap(),
                unchanged_command
            );
            assert_error(
                broker.dispatch(OUTSIDER, status_command(contract)),
                "permission_denied",
            );
            assert_error(
                broker.dispatch(
                    UNSELECTED,
                    Command::Register {
                        service: contract.service().into(),
                    },
                ),
                "permission_denied",
            );
            let request = deliver(
                &mut broker,
                provider,
                contract,
                status_response(contract, state, state == "retired"),
            );
            let observed = consume(&mut broker, contract, request).unwrap();
            assert_eq!(observed.retired, state == "retired");
            assert_eq!(observed.runtime_epoch, EPOCH);
            assert_eq!(
                observed.state,
                if state == "retired" {
                    State::Retired
                } else {
                    State::Unknown
                }
            );
            assert_error(
                broker.dispatch(CONSUMER, Command::TakeResult { request }),
                "not_found",
            );
        }
    }
}

#[test]
fn retirement_contract_rejects_malformed_or_different_tuple_provider_results() {
    for contract in [Contract::Transport, Contract::Application] {
        let valid = status_response(contract, "retired", true);
        let mut invalid_responses = Vec::new();
        for (field, value) in [
            ("ok", json!(false)),
            ("operation_id", json!("another-operation")),
            ("expected_runtime_epoch", json!(NEXT_EPOCH)),
            ("runtime_epoch", json!(NEXT_EPOCH)),
            ("runtime_epoch", json!("not-an-admission-epoch")),
            ("retired", json!("true")),
            ("retired", json!(false)),
            ("state", json!("closed")),
            ("state", json!("unknown")),
            ("operation_id", Value::Null),
            ("expected_runtime_epoch", Value::Null),
            ("application_generation", json!(0)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            invalid_responses.push(invalid);
        }
        for field in [
            "ok",
            "operation_id",
            "expected_runtime_epoch",
            "runtime_epoch",
            "startup_reconciled",
            "retired",
            "state",
        ] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            invalid_responses.push(missing);
        }
        let mut oversized = valid;
        oversized["unbounded_metadata"] = json!("x".repeat(retirement::MAX_RESPONSE_BYTES));
        invalid_responses.push(oversized);
        // A mutation acknowledgment never substitutes for a read-only proof.
        invalid_responses.push(json!({"ok":true,"retired":true,"fenced":true,
            "operation_id":reference(contract).operation_id,"runtime_epoch":EPOCH}));
        invalid_responses.push(json!({"ok":true,"launch":{"state":"stopped"}}));
        for response in invalid_responses {
            let provider = "vendor.selected-retirement-provider";
            let mut broker = broker(provider, contract);
            let request = deliver(&mut broker, provider, contract, response.clone());
            assert!(
                consume(&mut broker, contract, request).is_err(),
                "accepted {response}"
            );
        }
    }
}

#[test]
fn retirement_contract_epoch_fences_require_authoritative_native_recovery() {
    let contract = Contract::Transport;
    let mut fenced = status_response(contract, "epoch_fenced", true);
    fenced["runtime_epoch"] = json!(NEXT_EPOCH);
    assert!(
        retirement::parse_status(contract, &reference(contract), &fenced)
            .unwrap()
            .retired
    );
    for (field, value) in [
        ("startup_reconciled", json!(false)),
        ("cleanup_pending", json!(true)),
        ("pending_admissions", json!(1)),
        ("cleanup_pending", json!("false")),
        ("pending_admissions", json!("0")),
        ("active_operations", json!(null)),
        ("runtime_epoch", json!(EPOCH)),
        ("retired", json!(false)),
    ] {
        let mut invalid = fenced.clone();
        invalid[field] = value;
        assert!(retirement::parse_status(contract, &reference(contract), &invalid).is_err());
    }
    let contract = Contract::Application;
    let mut fenced = status_response(contract, "epoch_fenced", false);
    fenced["runtime_epoch"] = json!(NEXT_EPOCH);
    assert!(
        !retirement::parse_status(contract, &reference(contract), &fenced)
            .unwrap()
            .retired
    );
    // Launch v1 has no restart-native journal. Claiming startup reconciliation
    // or retirement after a restart is therefore unsupported evidence.
    for field in ["startup_reconciled", "retired"] {
        let mut invalid = fenced.clone();
        invalid[field] = json!(true);
        assert!(retirement::parse_status(contract, &reference(contract), &invalid).is_err());
    }
    for contract in [Contract::Transport, Contract::Application] {
        let unknown = status_response(contract, "unknown", false);
        assert!(
            !retirement::parse_status(contract, &reference(contract), &unknown)
                .unwrap()
                .retired
        );
        assert!(
            retirement::parse_status(
                contract,
                &reference(contract),
                &json!({"ok":true,"items":[]})
            )
            .is_err()
        );
    }
}

#[test]
fn retirement_contract_builders_fix_services_operations_and_retry_identity() {
    for contract in [Contract::Transport, Contract::Application] {
        let reference = reference(contract);
        for (command, action) in [
            (
                retirement::status_command(options(contract), contract, &reference).unwrap(),
                match contract {
                    Contract::Transport => "retirement_status",
                    Contract::Application => "application_retirement_status",
                },
            ),
            (
                retirement::retire_command(options(contract), contract, &reference).unwrap(),
                "retire_operation",
            ),
        ] {
            let Command::Request {
                service,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } = command
            else {
                panic!("expected fixed request");
            };
            assert_eq!(service, contract.service());
            assert_eq!(
                payload,
                json!({"action":action,"operation_id":reference.operation_id,
                "expected_runtime_epoch":reference.epoch})
            );
            assert_eq!(timeout_ms, 1_000);
            assert_eq!(operation_id, "exact-retirement-observation");
            assert_eq!(parent, None);
        }
        let mut wrong_service = options(contract);
        wrong_service.service = "provider-from-recovery-metadata/1";
        assert!(retirement::status_command(wrong_service, contract, &reference).is_err());
        for timeout in [0, retirement::MAX_TIMEOUT_MS + 1] {
            let mut invalid = options(contract);
            invalid.timeout_ms = timeout;
            assert!(retirement::retire_command(invalid, contract, &reference).is_err());
        }
        for (operation_id, epoch) in [
            ("".into(), EPOCH.into()),
            ("x".repeat(129), EPOCH.into()),
            ("bad\noperation".into(), EPOCH.into()),
            (reference.operation_id.clone(), "untrusted-epoch".into()),
        ] {
            let invalid = OperationRef {
                operation_id,
                epoch,
            };
            assert!(retirement::status_command(options(contract), contract, &invalid).is_err());
        }
    }
    let mut application = reference(Contract::Application);
    application.epoch = NEXT_EPOCH.into();
    assert!(application.validate(Contract::Application).is_err());
    application.operation_id = format!("ale1:{NEXT_EPOCH}:bad:nonce");
    assert!(application.validate(Contract::Application).is_err());
}

#[test]
fn retirement_contract_epoch_reads_bind_before_effects_without_claiming_retirement() {
    let ready = json!({"ok":true,"runtime_epoch":EPOCH,"operation_id":null,
        "expected_runtime_epoch":null,"startup_reconciled":true,"state":"ready",
        "retired":false,"active_operations":0,"pending_admissions":0,"cleanup_pending":false});
    assert_eq!(
        retirement::parse_epoch(Contract::Transport, &ready).unwrap(),
        EPOCH
    );
    // A ready transport may be bound although the sessions of an earlier
    // runtime were never confirmed retired. That is a separate statement.
    let mut unreconciled = ready.clone();
    unreconciled["startup_reconciled"] = json!(false);
    assert_eq!(
        retirement::parse_epoch(Contract::Transport, &unreconciled).unwrap(),
        EPOCH
    );
    for (field, value) in [
        ("startup_reconciled", json!("false")),
        ("state", json!("recovery_pending")),
        ("state", json!("recovery_blocked")),
        ("cleanup_pending", json!("false")),
        ("pending_admissions", json!("0")),
        ("active_operations", json!(null)),
        ("retired", json!(true)),
        ("operation_id", json!("unexpected-operation")),
        ("expected_runtime_epoch", json!(EPOCH)),
        ("runtime_epoch", json!(EPOCH.to_uppercase())),
    ] {
        let mut invalid = ready.clone();
        invalid[field] = value;
        assert!(retirement::parse_epoch(Contract::Transport, &invalid).is_err());
    }
    assert_eq!(
        retirement::parse_epoch(
            Contract::Application,
            &json!({"ok":true,"operation_epoch":EPOCH})
        )
        .unwrap(),
        EPOCH
    );
    for contract in [Contract::Transport, Contract::Application] {
        let Command::Request { payload, .. } =
            retirement::epoch_command(options(contract), contract).unwrap()
        else {
            panic!("expected epoch request");
        };
        assert_eq!(payload.as_object().unwrap().len(), 1);
        assert_eq!(
            payload["action"],
            match contract {
                Contract::Transport => "retirement_status",
                Contract::Application => "operation_epoch",
            }
        );
    }
    let Command::Request {
        service, payload, ..
    } = retirement::reconcile_transport_command(options(Contract::Transport)).unwrap()
    else {
        panic!("expected reconciliation request");
    };
    assert_eq!(service, Contract::Transport.service());
    assert_eq!(payload, json!({"action":"reconcile_startup"}));
}

#[test]
fn retirement_contract_epoch_binding_does_not_wait_for_unrelated_current_operations() {
    for provider in [
        "vendor.first-retirement-provider",
        "vendor.replacement-retirement-provider",
    ] {
        let contract = Contract::Transport;
        let mut broker = broker(provider, contract);
        let Response::Accepted { id } = broker.dispatch(
            CONSUMER,
            retirement::epoch_command(options(contract), contract).unwrap(),
        ) else {
            panic!("fixed epoch read was not admitted");
        };
        let Response::Request {
            id: delivered,
            service,
            payload,
            ..
        } = broker.dispatch(provider, Command::Poll {})
        else {
            panic!("selected provider did not receive epoch read");
        };
        assert_eq!(delivered, id);
        assert_eq!(service, contract.service());
        assert_eq!(payload, json!({"action":"retirement_status"}));
        let busy = json!({"ok":true,"runtime_epoch":EPOCH,"operation_id":null,
            "expected_runtime_epoch":null,"startup_reconciled":true,"state":"ready",
            "retired":false,"active_operations":2,"pending_admissions":1,"cleanup_pending":true});
        assert_ok(broker.dispatch(
            provider,
            Command::Complete {
                request: id,
                payload: busy.clone(),
            },
        ));
        let Response::Result { payload } =
            broker.dispatch(CONSUMER, Command::TakeResult { request: id })
        else {
            panic!("consumer did not receive epoch result");
        };
        assert_eq!(retirement::parse_epoch(contract, &payload).unwrap(), EPOCH);
        assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request: id }));
        // The transport decides when it may be bound and says so with
        // `ready`. While it still waits for an earlier runtime's sessions it
        // may not be; once it is ready their cleanup no longer holds it back.
        for (state, bound) in [
            ("ready", true),
            ("recovery_pending", false),
            ("recovery_blocked", false),
        ] {
            let mut unreconciled = busy.clone();
            unreconciled["startup_reconciled"] = json!(false);
            unreconciled["state"] = json!(state);
            assert_eq!(
                retirement::parse_epoch(contract, &unreconciled).is_ok(),
                bound,
                "{state}"
            );
        }
        // The admission snapshot remains unsuitable as any exact retirement proof.
        assert!(retirement::parse_status(contract, &reference(contract), &busy).is_err());
    }
}
