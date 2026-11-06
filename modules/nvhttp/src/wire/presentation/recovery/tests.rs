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
            self.owner_retired = true;
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
            "recovery_work" => json!(session::Reply::RecoveryWork {
                items: self.items.clone(),
                next_cursor: self.duplicate_page.then(|| "repeated-cursor".into())
            }),
            "reconcile_recovery" => {
                self.provider_reconciles += 1;
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
                let mut status = json!({"ok":true,"operation_id":if self.bad_proof {"another-operation"}else{operation},
                    "runtime_epoch":if self.epoch_fenced {"22222222222222222222222222222222"}else{expected},
                    "expected_runtime_epoch":expected,
                    "startup_reconciled":admission.service==TRANSPORT_SERVICE&&!self.recovery_blocked,
                    "state":if self.recovery_blocked {"recovery_blocked"}else if self.epoch_fenced {"epoch_fenced"}else if self.owner_retired {"retired"}else{"active"},
                    "retired":self.owner_retired});
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
    advance(&mut pump, &mut broker, now, 70, 160);
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
    advance(&mut pump, &mut broker, now, 70, 160);
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
    advance(&mut pump, &mut broker, now, 70, 160);
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
    advance(&mut pump, &mut broker, now, 30, 130);
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
