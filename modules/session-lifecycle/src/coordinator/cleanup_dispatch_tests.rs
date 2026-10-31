use super::*;
use crate::adapter::ParticipantSettings;
use std::collections::BTreeMap;

struct Harness {
    coordinator: Coordinator,
    dir: std::path::PathBuf,
    next_id: u64,
    results: BTreeMap<u64, Value>,
    callbacks: Vec<(String, String)>,
    uncertain_phase: String,
}

impl Harness {
    fn new(uncertain_phase: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lifecycle-cleanup-dispatch-{}",
            identifier().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&dir.join("journal.json")).unwrap();
        coordinator.configured = true;
        coordinator.settings.consumer_requirements.execution_domain = "seat".into();
        coordinator.settings.consumer_requirements.capture_providers = vec!["capture".into()];
        coordinator.settings.consumer_requirements.selector_schemas = vec!["selector/1".into()];
        coordinator.settings.participants = [
            participant("a", &[]),
            participant("c", &[]),
            participant("b", &["a"]),
        ]
        .into();
        Self {
            coordinator,
            dir,
            next_id: 0,
            results: BTreeMap::new(),
            callbacks: vec![],
            uncertain_phase: uncertain_phase.into(),
        }
    }

    fn tick(&mut self) {
        let Self {
            coordinator,
            next_id,
            results,
            callbacks,
            uncertain_phase,
            ..
        } = self;
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::QueryApiApproval {
                    service,
                    method,
                    interface_major,
                    min_minor,
                } => {
                    let provider = service.strip_suffix("/1").unwrap();
                    Ok(Response::Result {
                        payload: json!({"provider":provider,"method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
                    })
                }
                Command::CallApi {
                    service,
                    method,
                    payload,
                    ..
                } => {
                    let provider = service.strip_suffix("/1").unwrap().to_owned();
                    let result = if method == "plan" {
                        let deferred = *uncertain_phase == "deferred"
                            && provider == "b"
                            && payload["phase"] == "session.release";
                        let resources = if deferred {
                            json!([{"provider":"b","resource_id":"output","incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":[],"access":"read"}])
                        } else {
                            json!([])
                        };
                        let obligations = if deferred {
                            json!([{"obligation_id":"restore","provider":"b","resource_id":"output","mutation_domain":null,"depends_on":[],"cleanup_method":{"service":"b/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1},"payload":{}}])
                        } else {
                            json!([])
                        };
                        let proposals = if *uncertain_phase == "proposal" && provider == "a" {
                            json!([{"proposal_id":"change", "output_id":"output", "owner_provider":"a", "owner_service":"a/1", "target":payload["target"], "resource":{"provider":"a","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]}, "method":{"service":"a/1","method":"control","interface_major":1,"interface_minor":0,"schema_revision":1}, "argument_schema":{"id":"control/1","major":1,"minor":0,"digest":"a".repeat(64)}, "arguments":{},"access":"read","mutation_domains":[]}])
                        } else {
                            json!([])
                        };
                        json!({"status":"planned","plan":{"plan_id":"plan","revision":1,"resources":resources,"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":obligations,"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[],"control_proposals":proposals}})
                    } else if method == "callback" {
                        let phase = payload["phase"].as_str().unwrap().to_owned();
                        callbacks.push((provider.clone(), phase.clone()));
                        if provider == "b" && phase == *uncertain_phase {
                            json!({"status":"recovery_required","reason":"unknown_effect"})
                        } else if provider == "b"
                            && phase == "session.release"
                            && *uncertain_phase == "deferred"
                        {
                            json!({"status":"cleanup","outcomes":[{"obligation_id":"restore","state":"deferred_restoration","reason":"waiting"}]})
                        } else if phase == "session.release" {
                            json!({"status":"cleanup","outcomes":[]})
                        } else {
                            json!({"status":"prepared","candidate":null,"obligations":[]})
                        }
                    } else {
                        panic!("unexpected method {method}")
                    };
                    *next_id += 1;
                    results.insert(*next_id, result);
                    Ok(Response::Accepted { id: *next_id })
                }
                Command::TakeResult { request } => Ok(Response::Result {
                    payload: results[&request].clone(),
                }),
                Command::ReleaseResult { request } => {
                    results.remove(&request);
                    Ok(Response::Ok)
                }
                other => panic!("unexpected command: {other:?}"),
            }
        };
        coordinator.refresh_approvals(&mut send).unwrap();
        coordinator.advance_work(&mut send).unwrap();
    }

    fn run(&mut self, operation_id: &str) -> Value {
        for _ in 0..500 {
            self.tick();
            let response = self.coordinator.handle(
                "caller",
                "initiator",
                json!({"operation":"lookup","operation_id":operation_id}),
            );
            if response["status"] != "pending" {
                return response;
            }
        }
        panic!(
            "operation did not settle: {operation_id}; callbacks: {:?}",
            self.callbacks
        )
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn participant(provider: &str, depends_on: &[&str]) -> ParticipantSettings {
    let method = |name: &str| json!({"service":format!("{provider}/1"),"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
    serde_json::from_value(json!({"provider":provider,"depends_on":depends_on,"plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[{"service":format!("{provider}/1"),"method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1}],"settings_revision":"initial","settings":{}})).unwrap()
}

#[test]
fn unknown_cleanup_blocks_prerequisite_but_cleans_independent_provider() {
    let mut h = Harness::new("session.release");
    let started = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "succeeded");
    h.coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(started["session_id"].as_str().unwrap())
        .unwrap()
        .application_retired = true;
    let ended = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"end_session","operation_id":"end","target":started["target"]}),
    );
    assert_eq!(ended["status"], "pending");
    assert_eq!(h.run("end")["status"], "recovery_required");
    let releases: Vec<_> = h
        .callbacks
        .iter()
        .filter(|(_, phase)| phase == "session.release")
        .map(|(provider, _)| provider.as_str())
        .collect();
    assert_eq!(releases, ["b", "c"]);
}

#[test]
fn uncertain_dependent_prepare_never_releases_its_prerequisite() {
    let mut h = Harness::new("session.prepare");
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "recovery_required");
    let releases: Vec<_> = h
        .callbacks
        .iter()
        .filter(|(_, phase)| phase == "session.release")
        .map(|(provider, _)| provider.as_str())
        .collect();
    assert_eq!(releases, ["c"]);
}

#[test]
fn deferred_dependent_restoration_blocks_prerequisite_but_not_independent() {
    let mut h = Harness::new("deferred");
    let started = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "succeeded");
    h.coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(started["session_id"].as_str().unwrap())
        .unwrap()
        .application_retired = true;
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"end_session","operation_id":"end","target":started["target"]}),
    );
    assert_eq!(h.run("end")["status"], "recovery_required");
    let releases: Vec<_> = h
        .callbacks
        .iter()
        .filter(|(_, phase)| phase == "session.release")
        .map(|(provider, _)| provider.as_str())
        .collect();
    assert_eq!(releases, ["b", "c"]);
}

#[test]
fn missing_dependent_provider_blocks_prerequisite_but_not_independent() {
    let mut h = Harness::new("none");
    let started = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "succeeded");
    h.coordinator
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut(started["session_id"].as_str().unwrap())
        .unwrap()
        .application_retired = true;
    h.coordinator
        .settings
        .participants
        .retain(|participant| participant.provider != "b");
    h.coordinator.selections.remove("b");
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"end_session","operation_id":"end","target":started["target"]}),
    );
    assert_eq!(h.run("end")["status"], "recovery_required");
    let releases: Vec<_> = h
        .callbacks
        .iter()
        .filter(|(_, phase)| phase == "session.release")
        .map(|(provider, _)| provider.as_str())
        .collect();
    assert_eq!(releases, ["c"]);
}

#[test]
fn unsupported_control_proposal_is_rejected_before_admission_or_callback() {
    let mut h = Harness::new("proposal");
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "recovery_required");
    assert!(h.callbacks.is_empty());
    assert!(
        h.coordinator
            .shared
            .borrow()
            .value
            .authority
            .admissions
            .is_empty()
    );
    assert!(
        h.coordinator
            .shared
            .borrow()
            .value
            .authority
            .settled
            .is_empty()
    );
}

#[test]
fn owner_control_and_finalize_methods_each_require_exact_broker_approval() {
    let mut h = Harness::new("none");
    let method = |name: &str| json!({"service":"a/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
    let schema = json!({"id":"schema/1","major":1,"minor":0,"digest":"a".repeat(64)});
    h.coordinator.settings.control_policies.push(serde_json::from_value(json!({"dependent_provider":"b","owner_provider":"a","owner_service":"a/2","owner_method":method("control"),"status_method":method("control_status"),"settlement_method":method("settle"),"argument_schema":schema,"output_capability":"output","output_schema":schema,"max_access":"exclusive","mutation_domains":[]})).unwrap());
    h.coordinator.settings.finalize_policies.push(serde_json::from_value(json!({"owner_provider":"a","owner_service":"a/2","method":method("finalize"),"status_method":method("finalize_status"),"output_capability":"output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","mutation_domains":[]})).unwrap());
    let mut seen = Vec::new();
    let mut send = |command| match command {
        Command::QueryApiApproval {
            service,
            method,
            interface_major,
            min_minor,
        } => {
            let provider = service.split('/').next().unwrap();
            seen.push((service.clone(), method.clone(), interface_major));
            Ok(Response::Result {
                payload: json!({"provider":provider,"method":{"service":service,"method":method,"interface_major":interface_major,"interface_minor":min_minor}}),
            })
        }
        _ => unreachable!(),
    };
    for _ in 0..14 {
        h.coordinator.refresh_approvals(&mut send).unwrap();
    }
    for name in [
        "control",
        "control_status",
        "settle",
        "finalize",
        "finalize_status",
    ] {
        assert!(seen.contains(&("a/2".into(), name.into(), 2)));
        assert!(
            h.coordinator.selections["a"]
                .approvals
                .iter()
                .any(|approval| approval.method.service == "a/2" && approval.method.method == name)
        );
    }
    assert_eq!(seen.len(), 14);
}

#[test]
fn substituted_broker_method_is_rejected_before_caching() {
    let mut h = Harness::new("none");
    let response = h.coordinator.refresh_approvals(&mut |command| match command {
        Command::QueryApiApproval { service, method, interface_major, min_minor } => Ok(Response::Result {
            payload: json!({"provider":"a","method":{"service":service,"method":format!("{method}-other"),"interface_major":interface_major,"interface_minor":min_minor}}),
        }),
        _ => unreachable!(),
    });
    assert!(response.is_err());
    assert!(h.coordinator.selections.is_empty());
}
