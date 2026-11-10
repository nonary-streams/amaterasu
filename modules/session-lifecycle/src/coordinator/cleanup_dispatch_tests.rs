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
    unavailable_approval: Option<(String, String)>,
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
            unavailable_approval: None,
        }
    }

    fn tick(&mut self) {
        let Self {
            coordinator,
            next_id,
            results,
            callbacks,
            uncertain_phase,
            unavailable_approval,
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
                    if unavailable_approval.as_ref().is_some_and(
                        |(missing_provider, missing_method)| {
                            missing_provider == provider && missing_method == &method
                        },
                    ) {
                        return Ok(Response::Error {
                            code: "not_found".into(),
                            message: "selected provider method is not registered".into(),
                        });
                    }
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
fn independent_v1_stream_release_does_not_require_composed_output() {
    let mut h = Harness::new("none");
    h.coordinator.settings.participants = vec![participant("a", &[]), participant("c", &[])];
    let started = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    assert_eq!(h.run("prepare")["status"], "succeeded");
    let stream = h.coordinator.handle("caller", "initiator", json!({"operation":"prepare_stream","operation_id":"stream","target":started["target"],"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(stream["status"], "pending");
    let pause = h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"pause_stream","operation_id":"pause","target":stream["target"]}),
    );
    assert_eq!(pause["status"], "pending");
    let work = h
        .coordinator
        .shared
        .borrow()
        .value
        .receipts
        .values()
        .find(|r| r.request["operation_id"] == "pause")
        .unwrap()
        .work
        .clone()
        .unwrap();
    assert!(h.coordinator.work_selections(&work).is_ok());
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
fn unavailable_configured_provider_does_not_starve_independent_cleanup() {
    for missing_method in ["plan", "callback"] {
        let mut h = Harness::new("none");
        // Put an independent provider after the unavailable one in discovery order.
        h.coordinator.settings.participants = vec![
            participant("a", &[]),
            participant("b", &["a"]),
            participant("c", &[]),
        ];
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
        // Re-discover the unchanged installed selection while b is unavailable.
        h.coordinator.selections.clear();
        h.coordinator.approval_cursor = 0;
        h.unavailable_approval = Some(("b".into(), missing_method.into()));
        h.coordinator.handle(
            "caller",
            "initiator",
            json!({
                "operation":"end_session","operation_id":"end","target":started["target"]
            }),
        );
        let ended = h.run("end");
        assert_eq!(ended["status"], "recovery_required", "{ended}");
        let releases: Vec<_> = h
            .callbacks
            .iter()
            .filter(|(_, phase)| phase == "session.release")
            .map(|(provider, _)| provider.as_str())
            .collect();
        assert_eq!(releases, ["c"], "missing {missing_method}");
    }
}

#[test]
fn preparation_waits_for_all_methods_and_retries_unavailable_approval() {
    let mut h = Harness::new("none");
    h.unavailable_approval = Some(("b".into(), "callback".into()));
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({
            "operation":"prepare","operation_id":"prepare"
        }),
    );
    for _ in 0..40 {
        h.tick();
    }
    assert!(
        h.callbacks.is_empty(),
        "partial approval must not admit preparation"
    );
    assert_eq!(
        h.coordinator.handle(
            "caller",
            "initiator",
            json!({
                "operation":"lookup","operation_id":"prepare"
            })
        )["status"],
        "pending"
    );
    h.unavailable_approval = None;
    assert_eq!(h.run("prepare")["status"], "succeeded");
    assert!(
        h.coordinator
            .selections
            .values()
            .all(|selection| selection.approvals.len() == 3)
    );
}

#[test]
fn approval_rediscovery_preserves_retained_admission_order() {
    let mut h = Harness::new("none");
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({
            "operation":"prepare","operation_id":"prepare"
        }),
    );
    let mut admitted = None;
    for _ in 0..100 {
        h.tick();
        admitted = h
            .coordinator
            .shared
            .borrow()
            .value
            .authority
            .admissions
            .values()
            .find(|admission| admission.provider == "b")
            .cloned();
        if admitted.is_some() {
            break;
        }
    }
    let admitted = admitted.expect("b must have a durably admitted plan");
    h.coordinator.selections.clear();
    h.coordinator.approval_cursor = 0;
    h.unavailable_approval = Some(("b".into(), "plan".into()));
    for _ in 0..40 {
        h.tick();
    }
    assert_eq!(h.coordinator.selections["b"].approvals.len(), 2);
    assert!(
        h.coordinator
            .shared
            .borrow()
            .value
            .authority
            .admissions
            .values()
            .any(|current| current.selection == admitted.selection)
    );
    h.unavailable_approval = None;
    h.tick();
    assert_eq!(
        h.coordinator.selections["b"].approvals, admitted.selection.approvals,
        "discovery timing must not change the exact retained admission identity"
    );
    assert_eq!(h.run("prepare")["status"], "succeeded");
}

#[test]
fn unsupported_control_proposal_is_rejected_before_admission_or_callback() {
    let mut h = Harness::new("proposal");
    h.coordinator.handle(
        "caller",
        "initiator",
        json!({"operation":"prepare","operation_id":"prepare"}),
    );
    // Nothing was admitted or sent, so the outcome is known: the prepare
    // fails cleanly instead of fencing the session for recovery.
    let result = h.run("prepare");
    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(result["cleanup_completed"], true, "{result}");
    assert_eq!(result["reason"], "provider_plan_not_admitted", "{result}");
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
