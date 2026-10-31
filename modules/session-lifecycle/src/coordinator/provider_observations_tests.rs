use super::*;
use crate::planning::DomainState;
use crate::prepare_authority::{Admission, MethodApproval, ProviderSelection};

fn selection(provider: &str) -> ProviderSelection {
    let method = |name: &str| json!({"service":format!("{provider}/1"),"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
    let participant = serde_json::from_value(json!({"provider":provider,"plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}})).unwrap();
    let approvals = ["plan", "callback", "status"].into_iter().map(|name| MethodApproval {
        method: serde_json::from_value(method(name)).unwrap(),
        broker_approval:json!({"provider":provider,"method":{"service":format!("{provider}/1"),"method":name,"interface_major":1,"interface_minor":0}}),
    }).collect();
    ProviderSelection {
        participant,
        approvals,
        capabilities: BTreeSet::new(),
        cleanup_methods: vec![],
        authorized_borrowers: BTreeSet::new(),
    }
}

fn session(epoch: &str, id: &str) -> SessionRecord {
    SessionRecord {
        provider_graph: None,
        host_epoch: epoch.into(),
        id: id.into(),
        generation: 1,
        phase: SessionPhase::Ready,
        owner: crate::action_authority::Principal {
            caller: "transport".into(),
            initiator: "client".into(),
        },
        delegates: BTreeSet::new(),
        application_ref: Value::Null,
        settings_revision: "initial".into(),
        application_retired: false,
        streams: BTreeMap::new(),
        obligations: vec![],
        required_consumers: None,
        application: None,
        application_operation_id: None,
        admission_ids: BTreeSet::new(),
        admission_targets: BTreeMap::new(),
        admission_phases: BTreeMap::new(),
        provider_settings: BTreeMap::new(),
    }
}

fn add(
    journal: &mut Journal,
    session_id: &str,
    id: &str,
    provider: &str,
    domain: &str,
    phase: LifecyclePhase,
) {
    let selection = selection(provider);
    let plan = serde_json::from_value(json!({"plan_id":id,"revision":1,"resources":[],"mutation_domains":[{"domain":domain,"expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
    journal.authority.settled.insert(
        id.into(),
        Admission {
            provider: provider.into(),
            selection,
            plan,
            rights: vec![],
        },
    );
    journal
        .authority
        .snapshot
        .domains
        .entry(domain.into())
        .or_insert(DomainState {
            revision: 1,
            borrowers: BTreeMap::new(),
        });
    journal
        .authority
        .domain_owners
        .entry(domain.into())
        .or_insert(provider.into());
    let session = journal.sessions.get_mut(session_id).unwrap();
    session.admission_ids.insert(id.into());
    session.admission_targets.insert(
        id.into(),
        LifecycleTarget::Session {
            host_epoch: session.host_epoch.clone(),
            session_id: session_id.into(),
            generation: 1,
        },
    );
    session.admission_phases.insert(id.into(), phase);
}

struct Fixture {
    coordinator: Coordinator,
    directory: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-observation-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&directory.join("journal")).unwrap();
        let a = selection("provider-a");
        let b = selection("provider-b");
        coordinator.settings.participants = vec![a.participant.clone(), b.participant.clone()];
        coordinator.selections.insert("provider-a".into(), a);
        coordinator.selections.insert("provider-b".into(), b);
        let mut journal = coordinator.shared.borrow().value.clone();
        let epoch = journal.host_epoch.clone();
        for id in ["session-a", "session-b", "session-c"] {
            journal.sessions.insert(id.into(), session(&epoch, id));
        }
        add(
            &mut journal,
            "session-a",
            "admission-a",
            "provider-a",
            "shared",
            LifecyclePhase::SessionPrepare,
        );
        add(
            &mut journal,
            "session-b",
            "admission-b",
            "provider-a",
            "shared",
            LifecyclePhase::StreamPrepare,
        );
        add(
            &mut journal,
            "session-c",
            "admission-c",
            "provider-a",
            "other",
            LifecyclePhase::SessionPrepare,
        );
        add(
            &mut journal,
            "session-a",
            "admission-foreign",
            "provider-b",
            "foreign",
            LifecyclePhase::SessionPrepare,
        );
        journal.authority.refresh_required.insert("shared".into());
        coordinator.shared.borrow_mut().value = journal;
        Self {
            coordinator,
            directory,
        }
    }
    fn payload(&self, domains: Value) -> Value {
        json!({"operation_id":"refresh", "target":{"kind":"session","host_epoch":self.coordinator.shared.borrow().value.host_epoch,"session_id":"session-a","generation":1},"domains":domains,"resources":[]})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn metadata_limits_provider_and_session_scope_but_includes_shared_domain_provenance() {
    let fixture = Fixture::new();
    let journal = &fixture.coordinator.shared.borrow().value;
    let value = metadata(journal, &journal.sessions["session-a"], "provider-a").unwrap();
    let admitted = value["admitted_plans"].as_array().unwrap();
    assert_eq!(admitted.len(), 2);
    assert!(
        admitted
            .iter()
            .any(|entry| entry["admission_id"] == "admission-a"
                && entry["target"]["session_id"] == "session-a"
                && entry["phase"] == "session.prepare")
    );
    assert!(
        admitted
            .iter()
            .any(|entry| entry["admission_id"] == "admission-b"
                && entry["target"]["session_id"] == "session-b"
                && entry["phase"] == "stream.prepare")
    );
    assert_eq!(value["mutation_domains"][0]["domain"], "shared");
    assert_eq!(value["stale_mutation_domains"], json!(["shared"]));
    assert_eq!(value["resource_targets"].as_array().unwrap().len(), 2);
    assert!(
        !admitted
            .iter()
            .any(|entry| entry["admission_id"] == "admission-c"
                || entry["admission_id"] == "admission-foreign")
    );
    let foreign = metadata(journal, &journal.sessions["session-a"], "provider-b").unwrap();
    assert_eq!(foreign["admitted_plans"].as_array().unwrap().len(), 1);
    assert_eq!(foreign["mutation_domains"][0]["domain"], "foreign");
}

#[test]
fn metadata_rejects_missing_or_mismatched_admission_provenance() {
    let fixture = Fixture::new();
    let mut journal = fixture.coordinator.shared.borrow().value.clone();
    journal
        .sessions
        .get_mut("session-b")
        .unwrap()
        .admission_targets
        .remove("admission-b");
    assert_eq!(
        metadata(&journal, &journal.sessions["session-a"], "provider-a").unwrap_err(),
        "admission_target_missing"
    );
    let epoch = journal.host_epoch.clone();
    let b = journal.sessions.get_mut("session-b").unwrap();
    b.admission_targets.insert(
        "admission-b".into(),
        LifecycleTarget::Session {
            host_epoch: epoch,
            session_id: "session-a".into(),
            generation: 1,
        },
    );
    assert_eq!(
        metadata(&journal, &journal.sessions["session-a"], "provider-a").unwrap_err(),
        "admission_target_mismatch"
    );
    let b = journal.sessions.get_mut("session-b").unwrap();
    b.admission_targets.insert(
        "admission-b".into(),
        LifecycleTarget::Session {
            host_epoch: journal.host_epoch.clone(),
            session_id: "session-b".into(),
            generation: 1,
        },
    );
    b.admission_phases.remove("admission-b");
    assert_eq!(
        metadata(&journal, &journal.sessions["session-a"], "provider-a").unwrap_err(),
        "admission_phase_missing"
    );
}

#[test]
fn refresh_rejects_other_session_domains_and_duplicates_atomically() {
    let mut fixture = Fixture::new();
    for domains in [
        json!([{"domain":"shared","revision":2,"borrowers":{}},{"domain":"other","revision":2,"borrowers":{}}]),
        json!([{"domain":"shared","revision":2,"borrowers":{}},{"domain":"shared","revision":3,"borrowers":{}}]),
    ] {
        let payload = fixture.payload(domains);
        assert!(
            fixture
                .coordinator
                .refresh_provider_observations("provider-a", "provider-a", payload, "key".into())
                .is_err()
        );
        let shared = fixture.coordinator.shared.borrow();
        assert_eq!(
            shared.value.authority.snapshot.domains["shared"].revision,
            1
        );
        assert_eq!(shared.value.authority.snapshot.domains["other"].revision, 1);
        assert!(shared.value.authority.refresh_required.contains("shared"));
        assert!(!shared.value.receipts.contains_key("key"));
    }
    let payload = fixture.payload(json!([{"domain":"shared","revision":2,"borrowers":{}}]));
    let response = fixture
        .coordinator
        .refresh_provider_observations("provider-a", "provider-a", payload, "key".into())
        .unwrap();
    assert_eq!(response["status"], "succeeded");
    assert_eq!(response["stale_mutation_domains"], json!([]));
    assert_eq!(
        fixture
            .coordinator
            .shared
            .borrow()
            .value
            .authority
            .snapshot
            .domains["shared"]
            .revision,
        2
    );
}

#[test]
fn finalize_refresh_context_without_pending_stage_is_rejected() {
    let mut fixture = Fixture::new();
    let mut payload = fixture.payload(json!([{"domain":"shared","revision":2,"borrowers":{}}]));
    payload["provider_request"] = json!({});
    assert_eq!(
        fixture
            .coordinator
            .refresh_provider_observations(
                "provider-a",
                "provider-a",
                payload,
                "unexpected-context".into()
            )
            .unwrap_err(),
        "unexpected_finalize_refresh_context"
    );
    assert_eq!(
        fixture
            .coordinator
            .shared
            .borrow()
            .value
            .authority
            .snapshot
            .domains["shared"]
            .revision,
        1
    );
}

#[test]
fn three_large_admissions_are_sent_once_with_provenance() {
    let fixture = Fixture::new();
    let mut journal = fixture.coordinator.shared.borrow().value.clone();
    add(
        &mut journal,
        "session-c",
        "admission-third",
        "provider-a",
        "shared",
        LifecyclePhase::StreamPrepare,
    );
    for id in ["admission-a", "admission-b", "admission-third"] {
        journal
            .authority
            .settled
            .get_mut(id)
            .unwrap()
            .plan
            .constraints = json!({"provider_state":"x".repeat(3000)});
    }
    let value = metadata(&journal, &journal.sessions["session-a"], "provider-a").unwrap();
    assert!(value.get("plans").is_none());
    let admitted = value["admitted_plans"].as_array().unwrap();
    assert_eq!(admitted.len(), 3);
    assert!(admitted.iter().all(|entry| {
        entry["target"].is_object()
            && entry["phase"].is_string()
            && entry["plan"]["constraints"]["provider_state"]
                .as_str()
                .unwrap()
                .len()
                == 3000
    }));
    assert!(serde_json::to_vec(&value).unwrap().len() < 16 * 1024);
}

#[test]
fn v2_plan_refresh_requires_exact_active_request_before_native_observation_is_saved() {
    let mut fixture = Fixture::new();
    let epoch = fixture.coordinator.shared.borrow().value.host_epoch.clone();
    let target =
        json!({"kind":"session","host_epoch":epoch,"session_id":"session-a","generation":1});
    let request = json!({"version":2,"provider_sequence":3,"operation":"resource_plan","transition_id":"transition","operation_id":"plan-op","target":target,"phase":"session.prepare","settings_revision":"initial","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null});
    let participant =
        serde_json::to_value(&fixture.coordinator.selections["provider-a"].participant).unwrap();
    let state = json!({"provider":"provider-a","participant":participant,"request":request,"phase":"plan","plan":null,"admission_id":null,"pending_receipt":null,"status_polls":0,"last_sequence":3,"dispatch":null,"response":null});
    let work = json!({"phase":"session.prepare","target":target,"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":["provider-a"],"index":0,"states":{"provider-a":state},"candidate":null,"waiting_retirement":false});
    {
        let mut shared = fixture.coordinator.shared.borrow_mut();
        shared.value.sessions.get_mut("session-a").unwrap().phase = SessionPhase::Preparing;
        shared.value.receipts.insert("plan".into(), serde_json::from_value(json!({"caller":"transport","initiator":"client","request":{},"response":{"status":"pending"},"work":work})).unwrap());
    }
    let mut payload = fixture.payload(json!([{"domain":"shared","revision":2,"borrowers":{}}]));
    assert_eq!(
        fixture
            .coordinator
            .refresh_provider_observations(
                "provider-a",
                "provider-a",
                payload.clone(),
                "missing".into()
            )
            .unwrap_err(),
        "finalize_refresh_context_required"
    );
    payload["provider_request"] = request.clone();
    payload["provider_request"]["provider_sequence"] = json!(4);
    assert_eq!(
        fixture
            .coordinator
            .refresh_provider_observations(
                "provider-a",
                "provider-a",
                payload.clone(),
                "forged".into()
            )
            .unwrap_err(),
        "finalize_refresh_mismatch"
    );
    payload["provider_request"] = request;
    let result = fixture
        .coordinator
        .refresh_provider_observations("provider-a", "provider-a", payload, "exact".into())
        .unwrap();
    assert_eq!(result["status"], "succeeded");
    assert_eq!(
        fixture
            .coordinator
            .shared
            .borrow()
            .value
            .authority
            .snapshot
            .domains["shared"]
            .revision,
        2
    );
}
