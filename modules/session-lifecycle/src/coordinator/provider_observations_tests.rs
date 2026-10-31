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
