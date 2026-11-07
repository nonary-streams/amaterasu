use super::*;
use serde_json::json;
use std::collections::BTreeMap;

const EPOCH: &str = "11111111111111111111111111111111";

#[derive(Clone)]
struct Admission {
    service: String,
    method: String,
    payload: Value,
    cancelled: bool,
}

struct Broker {
    items: Vec<RecoveryWorkItem>,
    admissions: BTreeMap<u64, Admission>,
    operation_ids: BTreeMap<String, u64>,
    commands: Vec<(String, String, Value, u64)>,
    next: u64,
    owner_retired: bool,
    allow_retirement: bool,
    hold_retire_reply: bool,
    bad_proof: bool,
    recovery_blocked: bool,
    epoch_fenced: bool,
    pending_admissions: u64,
    cleanup_pending: bool,
    lose_retire_admission: bool,
    lost_retire_admission: bool,
    lose_release: bool,
    lost_release: bool,
    releases: usize,
    terminal_takes: usize,
    cancellations: usize,
    provider_reconciles: usize,
    duplicate_page: bool,
    paired_retirement: bool,
    application_retired: bool,
    transport_retired: bool,
    application_waits_transport: bool,
    hold_application_retire_reply: bool,
    pending_policy: bool,
    policy_pending: Vec<String>,
    pending_retry_ms: u64,
    split_pair_pages: bool,
    policy_error_for_app: bool,
}

impl Broker {
    fn new(item: RecoveryWorkItem) -> Self {
        Self {
            items: vec![item],
            admissions: BTreeMap::new(),
            operation_ids: BTreeMap::new(),
            commands: vec![],
            next: 0,
            owner_retired: false,
            allow_retirement: true,
            hold_retire_reply: false,
            bad_proof: false,
            recovery_blocked: false,
            epoch_fenced: false,
            pending_admissions: 0,
            cleanup_pending: false,
            lose_retire_admission: false,
            lost_retire_admission: false,
            lose_release: false,
            lost_release: false,
            releases: 0,
            terminal_takes: 0,
            cancellations: 0,
            provider_reconciles: 0,
            duplicate_page: false,
            paired_retirement: false,
            application_retired: false,
            transport_retired: false,
            application_waits_transport: false,
            hold_application_retire_reply: false,
            pending_policy: false,
            policy_pending: Vec::new(),
            pending_retry_ms: 25,
            split_pair_pages: false,
            policy_error_for_app: false,
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        match command {
            Command::CallApi {
                service,
                method,
                payload,
                operation_id,
                timeout_ms,
                interface_major,
                interface_minor,
                ..
            } => {
                assert_eq!(service, session::SERVICE);
                assert_eq!((interface_major, interface_minor), (1, 0));
                self.admit(service, method, payload, operation_id, timeout_ms)
            }
            Command::Request {
                service,
                payload,
                operation_id,
                timeout_ms,
                ..
            } => {
                assert!(matches!(
                    service.as_str(),
                    TRANSPORT_SERVICE | APPLICATION_SERVICE
                ));
                let method = payload["action"].as_str().unwrap().to_owned();
                self.admit(service, method, payload, operation_id, timeout_ms)
            }
            Command::TakeResult { request } => {
                let admission = self
                    .admissions
                    .get(&request)
                    .expect("live broker admission")
                    .clone();
                if admission.cancelled {
                    return Ok(Response::Error {
                        code: "cancelled".into(),
                        message: String::new(),
                    });
                }
                if self.hold_retire_reply && admission.method == "retire_operation" {
                    return Ok(Response::Pending);
                }
                if self.hold_application_retire_reply
                    && admission.method == "retire_operation"
                    && admission.service == APPLICATION_SERVICE
                    && !self.transport_retired
                {
                    return Ok(Response::Pending);
                }
                self.terminal_takes += 1;
                Ok(Response::Result {
                    payload: self.response(&admission),
                })
            }
            Command::ReleaseResult { request } => {
                if self.lose_release && !self.lost_release {
                    self.lost_release = true;
                    return Err("release reply lost".into());
                }
                self.admissions.remove(&request);
                self.releases += 1;
                Ok(Response::Ok)
            }
            Command::Cancel { request } => {
                self.cancellations += 1;
                self.admissions
                    .get_mut(&request)
                    .expect("cancelled live request")
                    .cancelled = true;
                Ok(Response::Ok)
            }
            other => panic!("unexpected recovery broker command {other:?}"),
        }
    }

    fn admit(
        &mut self,
        service: String,
        method: String,
        payload: Value,
        operation_id: String,
        timeout_ms: u64,
    ) -> ModuleResult<Response> {
        self.commands
            .push((service.clone(), method.clone(), payload.clone(), timeout_ms));
        if let Some(id) = self.operation_ids.get(&operation_id) {
            let previous = &self.admissions[id];
            assert_eq!(previous.service, service);
            assert_eq!(previous.method, method);
            assert_eq!(previous.payload, payload);
            return Ok(Response::Accepted { id: *id });
        }
        self.next += 1;
        self.operation_ids.insert(operation_id, self.next);
        if method == "retire_operation" && self.allow_retirement && !self.recovery_blocked {
            if self.paired_retirement {
                if service == APPLICATION_SERVICE {
                    self.application_retired =
                        !self.application_waits_transport || self.transport_retired;
                } else {
                    self.transport_retired = true;
                }
                self.owner_retired = self.application_retired && self.transport_retired;
            } else {
                self.owner_retired = true;
            }
        }
        self.admissions.insert(
            self.next,
            Admission {
                service,
                method: method.clone(),
                payload,
                cancelled: false,
            },
        );
        if method == "retire_operation" && self.lose_retire_admission && !self.lost_retire_admission
        {
            self.lost_retire_admission = true;
            return Err("admission reply lost".into());
        }
        Ok(Response::Accepted { id: self.next })
    }

    fn response(&mut self, admission: &Admission) -> Value {
        match admission.method.as_str() {
            "recovery_work" => {
                if self.split_pair_pages && !self.items.is_empty() {
                    if admission.payload["cursor"].is_null() {
                        json!(session::Reply::RecoveryWork {
                            items: vec![self.items[0].clone()],
                            next_cursor: Some("paired-transport".into())
                        })
                    } else {
                        assert_eq!(admission.payload["cursor"], "paired-transport");
                        json!(session::Reply::RecoveryWork {
                            items: vec![self.items[1].clone()],
                            next_cursor: None
                        })
                    }
                } else {
                    json!(session::Reply::RecoveryWork {
                        items: self.items.clone(),
                        next_cursor: self.duplicate_page.then(|| "repeated-cursor".into())
                    })
                }
            }
            "reconcile_recovery" => {
                self.provider_reconciles += 1;
                if self.policy_error_for_app
                    && !self.transport_retired
                    && admission.payload["work_id"]
                        .as_str()
                        .unwrap()
                        .ends_with("-app")
                {
                    return json!(session::Reply::Error {
                        code: "retirement_unconfirmed".into(),
                        message: "The paired transport still owns its native source".into()
                    });
                }
                if self.pending_policy || (self.paired_retirement && !self.owner_retired) {
                    self.policy_pending
                        .push(admission.payload["work_id"].as_str().unwrap().into());
                    return json!(session::Reply::Pending {
                        retry_after_ms: self.pending_retry_ms
                    });
                }
                assert!(
                    self.owner_retired,
                    "provider reconciliation preceded owner retirement"
                );
                self.items.clear();
                json!(session::Reply::Acknowledged {})
            }
            "retire_operation" => json!({"ok":true,"accepted":true}),
            "reconcile_startup" => {
                self.recovery_blocked = false;
                if self.allow_retirement {
                    self.owner_retired = true;
                }
                json!({"ok":true,"state":"recovery_pending"})
            }
            "retirement_status" | "application_retirement_status" => {
                let operation = admission.payload["operation_id"].as_str().unwrap();
                let expected = admission.payload["expected_runtime_epoch"]
                    .as_str()
                    .unwrap();
                let retired = if self.paired_retirement {
                    if admission.service == APPLICATION_SERVICE {
                        self.application_retired
                    } else {
                        self.transport_retired
                    }
                } else {
                    self.owner_retired
                };
                let mut status = json!({"ok":true,"operation_id":if self.bad_proof {"another-operation"}else{operation},
                    "runtime_epoch":if self.epoch_fenced {"22222222222222222222222222222222"}else{expected},
                    "expected_runtime_epoch":expected,
                    "startup_reconciled":admission.service==TRANSPORT_SERVICE&&!self.recovery_blocked,
                    "state":if self.recovery_blocked {"recovery_blocked"}else if self.epoch_fenced {"epoch_fenced"}else if retired {"retired"}else{"active"},
                    "retired":retired});
                if admission.service == TRANSPORT_SERVICE {
                    status["active_operations"] = json!(3);
                    status["pending_admissions"] = json!(self.pending_admissions);
                    status["cleanup_pending"] = json!(self.cleanup_pending);
                }
                status
            }
            method => panic!("unexpected recovery method {method}"),
        }
    }
}

fn transport() -> RecoveryWorkItem {
    RecoveryWorkItem {
        id: "work-1".into(),
        client_id: "client".into(),
        session_id: "session".into(),
        generation: 2,
        reservation_id: "reservation".into(),
        budget_ms: 30_000,
        binding: RecoveryBinding::Transport {
            operation_id: "rtsp-owned-operation".into(),
            epoch: EPOCH.into(),
        },
    }
}

fn advance(pump: &mut RecoveryPump, broker: &mut Broker, now: Instant, start: u64, end: u64) {
    for tick in start..end {
        pump.poll(
            now + Duration::from_millis(tick * 20),
            &mut |command| broker.send(command),
            |_| true,
        );
    }
}

fn advance_until_idle(
    pump: &mut RecoveryPump,
    broker: &mut Broker,
    now: Instant,
    start: u64,
    end: u64,
) {
    for tick in start..end {
        pump.poll(
            now + Duration::from_millis(tick * 20),
            &mut |command| broker.send(command),
            |_| true,
        );
        if pump.is_idle() {
            return;
        }
    }
    panic!("durable retirement work did not converge within the test budget");
}

fn paired_source(split_pages: bool) -> Broker {
    let mut application = transport();
    application.id = "retirement-exact-source-app".into();
    application.binding = RecoveryBinding::Application {
        operation_id: format!("ale1:{EPOCH}:application-owned"),
        epoch: EPOCH.into(),
        application_generation: 5,
    };
    let mut transport = application.clone();
    transport.id = "retirement-exact-source-transport".into();
    transport.binding = RecoveryBinding::Transport {
        operation_id: "rtsp-owned-operation".into(),
        epoch: EPOCH.into(),
    };
    let mut broker = Broker::new(application);
    broker.items.push(transport);
    broker.paired_retirement = true;
    broker.split_pair_pages = split_pages;
    broker
}

#[test]
fn owner_retirement_and_independent_policy_ack_precede_empty_readiness() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    assert!(!pump.is_idle());
    advance(&mut pump, &mut broker, now, 0, 100);
    assert!(pump.is_idle());
    assert_eq!(broker.provider_reconciles, 1);
    let methods: Vec<_> = broker
        .commands
        .iter()
        .map(|(_, method, _, _)| method.as_str())
        .collect();
    let retire = methods
        .iter()
        .position(|method| *method == "retire_operation")
        .unwrap();
    let status = methods
        .iter()
        .position(|method| *method == "retirement_status")
        .unwrap();
    let reconcile = methods
        .iter()
        .position(|method| *method == "reconcile_recovery")
        .unwrap();
    assert!(retire < status && status < reconcile);
    assert!(broker.admissions.is_empty());
    assert_eq!(broker.releases, broker.next as usize);
}

#[test]
fn a_controller_owned_live_binding_never_receives_a_retirement_mutation() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    for tick in 0..70 {
        pump.poll(
            now + Duration::from_millis(tick * 20),
            &mut |command| broker.send(command),
            |_| false,
        );
    }
    assert!(!pump.is_idle());
    assert!(
        !broker
            .commands
            .iter()
            .any(|(_, method, _, _)| method == "retire_operation")
    );
    for tick in 70..200 {
        pump.poll(
            now + Duration::from_millis(tick * 20),
            &mut |command| broker.send(command),
            |_| true,
        );
        if pump.is_idle() {
            break;
        }
    }
    assert_eq!(broker.provider_reconciles, 1);
    assert!(pump.is_idle());
}

#[test]
fn local_admission_and_failed_cleanup_do_not_release_provider_holds() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.allow_retirement = false;
    advance(&mut pump, &mut broker, now, 0, 70);
    assert!(!pump.is_idle());
    assert_eq!(broker.provider_reconciles, 0);
    assert!(!broker.items.is_empty());
    broker.owner_retired = true;
    advance_until_idle(&mut pump, &mut broker, now, 70, 300);
    assert_eq!(broker.provider_reconciles, 1);
    assert!(pump.is_idle());
}

#[test]
fn unrelated_operation_proofs_cannot_reconcile_an_exact_intent() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.bad_proof = true;
    advance(&mut pump, &mut broker, now, 0, 70);
    assert!(!pump.is_idle());
    assert_eq!(broker.provider_reconciles, 0);
    broker.bad_proof = false;
    advance_until_idle(&mut pump, &mut broker, now, 70, 300);
    assert!(pump.is_idle());
}

#[test]
fn epoch_fencing_waits_for_startup_and_admission_cleanup() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.epoch_fenced = true;
    broker.pending_admissions = 1;
    broker.cleanup_pending = true;
    advance(&mut pump, &mut broker, now, 0, 70);
    assert_eq!(broker.provider_reconciles, 0);
    assert!(!pump.is_idle());
    broker.pending_admissions = 0;
    broker.cleanup_pending = false;
    advance_until_idle(&mut pump, &mut broker, now, 70, 300);
    assert_eq!(broker.provider_reconciles, 1);
    assert!(pump.is_idle());
}

#[test]
fn blocked_transport_startup_uses_the_owner_reconciliation_path() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.recovery_blocked = true;
    advance(&mut pump, &mut broker, now, 0, 100);
    assert!(broker.commands.iter().any(
        |(service, method, _, _)| service == TRANSPORT_SERVICE && method == "reconcile_startup"
    ));
    assert_eq!(broker.provider_reconciles, 1);
    assert!(pump.is_idle());
}

#[test]
fn application_recovery_selects_only_the_fixed_execution_owner_contract() {
    let mut item = transport();
    item.binding = RecoveryBinding::Application {
        operation_id: format!("ale1:{EPOCH}:application-owned"),
        epoch: EPOCH.into(),
        application_generation: 5,
    };
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(item);
    let now = Instant::now();
    advance(&mut pump, &mut broker, now, 0, 100);
    for (service, method, payload, _) in &broker.commands {
        if matches!(
            method.as_str(),
            "retire_operation" | "application_retirement_status"
        ) {
            assert_eq!(service, APPLICATION_SERVICE);
            assert_eq!(
                payload["operation_id"],
                format!("ale1:{EPOCH}:application-owned")
            );
            assert_eq!(payload["expected_runtime_epoch"], EPOCH);
            assert!(payload.get("owner").is_none());
        }
    }
    assert_eq!(broker.provider_reconciles, 1);
    assert!(pump.is_idle());
}

#[test]
fn lost_admission_is_recovered_with_the_identical_broker_operation() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.lose_retire_admission = true;
    advance(&mut pump, &mut broker, now, 0, 100);
    let retires: Vec<_> = broker
        .commands
        .iter()
        .filter(|(_, method, _, _)| method == "retire_operation")
        .collect();
    assert_eq!(retires.len(), 2);
    assert_eq!(retires[0], retires[1]);
    assert!(pump.is_idle());
}

#[test]
fn a_lost_result_release_is_retried_without_repeating_the_consumed_result() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.lose_release = true;
    advance(&mut pump, &mut broker, now, 0, 100);
    assert!(pump.is_idle());
    assert_eq!(broker.terminal_takes, broker.next as usize);
    assert_eq!(broker.releases, broker.next as usize);
}

#[test]
fn timed_out_attempts_settle_their_broker_slot_and_preserve_durable_intents() {
    let mut item = transport();
    item.budget_ms = 50;
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(item);
    let now = Instant::now();
    broker.hold_retire_reply = true;
    advance(&mut pump, &mut broker, now, 0, 30);
    assert!(broker.cancellations > 0);
    assert_eq!(broker.provider_reconciles, 0);
    assert!(!pump.is_idle());
    assert!(!broker.items.is_empty());
    broker.items[0].budget_ms = 30_000;
    broker.hold_retire_reply = false;
    advance_until_idle(&mut pump, &mut broker, now, 30, 300);
    assert!(pump.is_idle());
    assert!(
        broker
            .commands
            .iter()
            .filter(|(_, method, _, _)| method == "retire_operation")
            .all(|(_, _, _, budget)| *budget > 1)
    );
}

#[test]
fn repeated_pagination_cursors_never_establish_empty_readiness() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    let now = Instant::now();
    broker.duplicate_page = true;
    advance(&mut pump, &mut broker, now, 0, 100);
    assert!(!pump.is_idle());
    assert!(pump.seen_cursors.len() <= MAX_SCAN_PAGES);
}

#[test]
fn application_epoch_mismatch_is_never_synthetic_retirement() {
    let binding = RecoveryBinding::Application {
        operation_id: format!("ale1:{EPOCH}:application"),
        epoch: EPOCH.into(),
        application_generation: 5,
    };
    let proof = json!({"ok":true,"operation_id":format!("ale1:{EPOCH}:application"),"expected_runtime_epoch":EPOCH,
        "runtime_epoch":"22222222222222222222222222222222","state":"unknown","retired":true,"startup_reconciled":false});
    assert!(!decode_retirement_status(&binding, &proof).is_some_and(|status| status.retired));
    let mut wrong_generation = proof;
    wrong_generation["runtime_epoch"] = json!(EPOCH);
    wrong_generation["state"] = json!("retired");
    wrong_generation["application_generation"] = json!(6);
    assert!(
        !decode_retirement_status(&binding, &wrong_generation).is_some_and(|status| status.retired)
    );
}

#[test]
fn pending_application_acknowledgment_yields_to_its_paired_transport() {
    for split_pages in [false, true] {
        let mut pump = RecoveryPump::new().unwrap();
        let mut broker = paired_source(split_pages);
        let now = Instant::now();
        advance_until_idle(&mut pump, &mut broker, now, 0, 300);
        assert!(broker.policy_pending.iter().any(|id| id.ends_with("-app")));
        assert!(broker.application_retired && broker.transport_retired);
        assert!(broker.items.is_empty());
        let retire_owners: Vec<_> = broker
            .commands
            .iter()
            .filter(|(_, method, _, _)| method == "retire_operation")
            .map(|(service, _, _, _)| service.as_str())
            .collect();
        assert_eq!(retire_owners, vec![APPLICATION_SERVICE, TRANSPORT_SERVICE]);
        assert!(broker.admissions.is_empty());
        assert_eq!(broker.releases, broker.next as usize);
        assert!(pump.is_idle());
    }
}

#[test]
fn nonterminal_application_proof_yields_until_transport_retirement_opens_its_barrier() {
    for split_pages in [false, true] {
        let mut pump = RecoveryPump::new().unwrap();
        let mut broker = paired_source(split_pages);
        broker.application_waits_transport = true;
        let now = Instant::now();
        advance_until_idle(&mut pump, &mut broker, now, 0, 400);
        let retire_owners: Vec<_> = broker
            .commands
            .iter()
            .filter(|(_, method, _, _)| method == "retire_operation")
            .map(|(service, _, _, _)| service.as_str())
            .collect();
        assert!(retire_owners.len() >= 3);
        assert_eq!(
            &retire_owners[..3],
            &[APPLICATION_SERVICE, TRANSPORT_SERVICE, APPLICATION_SERVICE]
        );
        assert!(
            broker
                .policy_pending
                .iter()
                .any(|id| id.ends_with("-transport"))
        );
        assert!(broker.application_retired && broker.transport_retired);
        assert!(broker.items.is_empty());
        assert!(broker.admissions.is_empty());
        assert_eq!(broker.releases, broker.next as usize);
    }
}

#[test]
fn settled_application_timeout_preserves_the_paired_transport_turn() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = paired_source(false);
    broker.items[0].budget_ms = 60;
    broker.application_waits_transport = true;
    broker.hold_application_retire_reply = true;
    let now = Instant::now();
    advance_until_idle(&mut pump, &mut broker, now, 0, 400);
    assert!(broker.cancellations > 0);
    assert!(broker.application_retired && broker.transport_retired);
    assert!(broker.items.is_empty());
    assert!(broker.admissions.is_empty());
    assert_eq!(broker.releases, broker.next as usize);
}

#[test]
fn pending_policy_intents_back_off_without_forgetting_their_obligations() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = Broker::new(transport());
    broker.pending_policy = true;
    broker.pending_retry_ms = 1000;
    let now = Instant::now();
    let mut pending_at = None;
    for tick in 0..100 {
        let current = now + Duration::from_millis(tick * 20);
        pump.poll(current, &mut |command| broker.send(command), |_| true);
        if !broker.policy_pending.is_empty() {
            pending_at = Some(current);
            break;
        }
    }
    let pending_at = pending_at.expect("initial policy reconciliation must become pending");
    let commands = broker.commands.len();
    for _ in 0..1000 {
        pump.poll(pending_at, &mut |command| broker.send(command), |_| true);
    }
    assert_eq!(broker.commands.len(), commands);
    assert_eq!(broker.provider_reconciles, 1);
    assert!(!pump.is_idle());
    assert_eq!(broker.items.len(), 1);
    assert!(broker.admissions.is_empty());
    assert_eq!(broker.releases, broker.next as usize);
    broker.pending_policy = false;
    advance_until_idle(&mut pump, &mut broker, now, 100, 400);
}

#[test]
fn a_pending_acknowledgment_does_not_consume_the_next_items_attempt_budget() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = paired_source(false);
    broker.pending_retry_ms = 1000;
    broker.items[1].budget_ms = 500;
    let now = Instant::now();
    advance_until_idle(&mut pump, &mut broker, now, 0, 400);
    let transport_attempt = broker
        .commands
        .iter()
        .find(|(service, method, _, _)| {
            service == TRANSPORT_SERVICE && method == "retire_operation"
        })
        .expect("paired transport must get its own attempt");
    assert!(transport_attempt.3 > 400 && transport_attempt.3 <= 500);
    assert_eq!(broker.cancellations, 0);
    assert!(broker.application_retired && broker.transport_retired);
}

#[test]
fn a_policy_error_on_one_proved_binding_does_not_starve_its_complement() {
    let mut pump = RecoveryPump::new().unwrap();
    let mut broker = paired_source(false);
    broker.policy_error_for_app = true;
    let now = Instant::now();
    advance_until_idle(&mut pump, &mut broker, now, 0, 300);
    assert!(broker.application_retired && broker.transport_retired);
    assert!(broker.items.is_empty());
    assert!(broker.admissions.is_empty());
    assert_eq!(broker.releases, broker.next as usize);
}
