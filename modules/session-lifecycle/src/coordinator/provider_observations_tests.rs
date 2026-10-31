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
        approved_control_bindings: Vec::new(),
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
        resource_generations: Default::default(),
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

#[test]
fn nested_finalize_plan_refresh_checks_exact_owner_and_request() {
    use sha2::{Digest, Sha256};
    let path = std::env::temp_dir().join(format!(
        "amaterasu-finalize-positive-{}-{}",
        std::process::id(),
        identifier().unwrap()
    ));
    std::fs::create_dir(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut coordinator = Coordinator::open(&path.join("journal.json")).unwrap();
    let epoch = coordinator.shared.borrow().value.host_epoch.clone();
    let session =
        json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":1});
    let stream = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":1});
    let document = r#"{"$id":"example.schema/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false}"#;
    let schema = json!({"id":"example.schema/1","major":1,"minor":0,"digest":format!("{:x}", Sha256::digest(document.as_bytes()))});
    let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
    let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":session,"resource":resource,"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}],"data":{}});
    let method = |name: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
    let approval = |name: &str| json!({"method":method(name),"broker_approval":{"provider":"owner","method":{"service":"owner/2","method":name,"interface_major":2,"interface_minor":0}}});
    let participant = json!({"provider":"owner","contract_version":2,"plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[method("callback")],"settings_revision":"one","settings":{}});
    let selection = json!({"participant":participant,"approvals":[approval("plan"),approval("callback"),approval("status"),approval("finalize")],"capabilities":[],"cleanup_methods":[method("callback")],"authorized_borrowers":[]});
    let obligation = json!({"obligation_id":"restore","provider":"owner","resource_id":"output","mutation_domain":"domain","depends_on":[],"cleanup_method":method("callback"),"payload":{}});
    let source_plan = json!({"plan_id":"source","revision":1,"resources":[{"provider":"owner","resource_id":"output","incarnation":"native","expected_revision":1,"execution_domain":"seat","mutation_domains":["domain"],"access":"exclusive"}],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[obligation],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
    let mut final_plan = source_plan.clone();
    final_plan["plan_id"] = json!("final");
    final_plan["candidate_constraint"] = json!({"capture_provider":"capture","selector_schema":"selector","resource_provider":"owner","resource_id":"output","incarnation":"native","execution_domain":"seat","mode":null,"plan_revision":1});
    let requirements = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector"]});
    let callback = json!({"version":2,"resource_generation":1,"provider_sequence":1,"operation":"lifecycle_callback","transition_id":"source-transition","operation_id":"source-op","target":session,"phase":"session.prepare","settings_revision":"one","settings":{},"intent":null,"reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":1,"admission_id":"source","approved_plan":source_plan});
    let prepared =
        json!({"status":"prepared","candidate":null,"obligations":[obligation],"outputs":[output]});
    let call = |name: &str, args: Value, operation: &str| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"approval":approval(name)["broker_approval"],"arguments":args,"timeout_ms":5000,"operation_id":operation});
    let proof = json!({"participant":participant,"request":callback,"terminal":{"call":call("callback",callback.clone(),"source-op"),"state":{"state":"completed","payload":prepared}},"pending_receipt":null});
    let stored = json!({"output":output,"admission_id":"source","transition_id":"source-transition","plan":source_plan,"state":"retained","callback_proof":proof});
    let policy = json!({"owner_provider":"owner","owner_service":"owner/2","method":method("finalize"),"status_method":method("status"),"output_capability":"display","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
    let mut staged_output = output.clone();
    staged_output["resource"]["revision"] = json!(2);
    let stage_request = json!({"version":2,"resource_generation":1,"provider_sequence":2,"operation":"lifecycle_callback","transition_id":"transition","operation_id":"stage-op","target":stream,"phase":"stream.prepare","settings_revision":"one","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":1,"admission_id":"stage-admission","approved_plan":source_plan});
    let stage_response = json!({"status":"prepared","candidate":null,"obligations":[obligation],"outputs":[staged_output]});
    let stage_proof = json!({"participant":participant,"request":stage_request,"terminal":{"call":call("callback",stage_request.clone(),"stage-op"),"state":{"state":"completed","payload":stage_response}},"pending_receipt":null});
    let stage = json!({"target":stream,"parent_generation":1,"admission_id":"stage-admission","transition_id":"transition","plan":source_plan,"before":output,"after":staged_output,"callback_proof":stage_proof});
    let frozen = json!({"target":stream,"parent_generation":1,"transition_id":"transition","input":stored,"stage":stage,"policy":policy,"finalize_approval":approval("finalize"),"finalize_status_approval":approval("status"),"schema_documents":[{"reference":schema,"document":document}]});
    let plan_request = json!({"version":2,"resource_generation":1,"provider_sequence":2,"operation":"resource_finalize_plan","transition_id":"transition","operation_id":"plan-op","target":stream,"phase":"stream.prepare","settings_revision":"one","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[staged_output],"control_results":[]});
    let request: amaterasu_extension_api::ResourceFinalizeRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":3,"target":stream,"output":output,"control_results":[],"admission_id":"fresh","permit_id":"permit","remaining_ms":5000})).unwrap();
    let context = json!({"policy":policy,"output":output,"control_results":[],"target":stream,"transition_id":"transition","parent_generation":1});
    let effect_call = call(
        "finalize",
        serde_json::to_value(&request).unwrap(),
        "effect",
    );
    let status_call = call("status", json!({}), "status-op");
    let dispatch = json!({"request":request,"context":context,"effect_call":effect_call,"status_selection":status_call,"phase":{"effect":{}},"status_polls":0,"dispatch":null});
    // Serialize the enum using its actual wire tag instead of assuming a unit representation.
    let mut dispatch = dispatch;
    dispatch["phase"] = json!("effect");
    let owner_plan = json!({"provider":"owner","participant":participant,"request":plan_request,"phase":"plan","plan":final_plan,"admission_id":"fresh","pending_receipt":null,"status_polls":0,"last_sequence":2,"dispatch":null,"terminal_dispatch":null,"response":null});
    let progress = json!({"inputs":[stored],"selections":[],"controls":[],"finalize":{"selection":frozen,"owner_plan":owner_plan,"dispatch":dispatch}});
    let work = json!({"composition_progress":progress,"phase":"stream.prepare","target":stream,"intent":"initial","reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false});
    let session_record = json!({"host_epoch":epoch,"id":"session","generation":1,"resource_generations":{"owner":1},"phase":"ready","owner":{"caller":"transport","initiator":"transport"},"delegates":[],"application_ref":{},"settings_revision":"one","application_retired":false,"streams":{"stream":{"generation":1,"phase":"preparing","candidate":null,"required":[],"acknowledgements":[],"retirements":[]}},"admission_ids":["source","fresh"],"admission_targets":{"source":session,"fresh":stream},"admission_phases":{"source":"session.prepare","fresh":"stream.prepare"}});
    let admission = |plan: Value| json!({"provider":"owner","selection":selection,"plan":plan,"rights":[["domain","Exclusive"]]});
    let mut journal = coordinator.shared.borrow().value.clone();
    journal.sessions.insert(
        "session".into(),
        serde_json::from_value(session_record).unwrap(),
    );
    journal.receipts.insert("work".into(), serde_json::from_value(json!({"caller":"transport","initiator":"transport","request":{},"response":{"status":"pending"},"work":work})).unwrap());
    journal.composition = serde_json::from_value(
        json!({"outputs":[stored],"staged_outputs":[stage],"selections":[],"finalize_selections":[frozen],"refinements":[]}),
    )
    .unwrap();
    journal
        .admission_transitions
        .insert("source".into(), "source-transition".into());
    journal
        .admission_transitions
        .insert("fresh".into(), "transition".into());
    journal.authority = serde_json::from_value(json!({"sequences":{"owner":3},"snapshot":{"resources":{"owner":{"output":{"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domains":["domain"]}}},"domains":{"domain":{"revision":1,"borrowers":{}}}},"admissions":{"fresh":admission(final_plan)},"domain_locks":{"domain":"fresh"},"domain_owners":{"domain":"owner"},"refresh_required":[],"settled":{"source":admission(source_plan.clone())},"finalize_permits":{"permit":{"context":context,"request":request,"consumed":false,"settled":false}}})).unwrap();
    coordinator
        .settings
        .finalize_policies
        .push(serde_json::from_value(policy).unwrap());
    coordinator
        .settings
        .schema_documents
        .push(serde_json::from_value(json!({"reference":schema,"document":document})).unwrap());
    coordinator
        .settings
        .participants
        .push(serde_json::from_value(participant.clone()).unwrap());
    coordinator.selections.insert(
        "owner".into(),
        serde_json::from_value(selection.clone()).unwrap(),
    );
    journal.authority.admissions.remove("fresh");
    journal.authority.finalize_permits.clear();
    journal.authority.domain_locks.clear();
    journal.authority.refresh_required.insert("domain".into());
    save(&coordinator.shared, journal).unwrap();
    let verification = |request: &Value| {
        let request: amaterasu_extension_api::ProviderRequest =
            serde_json::from_value(request.clone()).unwrap();
        json!({"host_epoch":epoch,"provider_sequence":request.provider_sequence,"operation_id":request.operation_id,"transition_id":request.transition_id,"target":request.target,"fingerprint":super::super::security::fingerprint(&request).unwrap()})
    };
    assert_eq!(
        coordinator.verify_provider("owner", &verification(&plan_request))["status"],
        "succeeded"
    );
    let original = coordinator.shared.borrow().value.clone();
    // A post-stage owner re-observation is authorized only while the exact
    // broker-dispatched request is persisted in the dependent's work.
    let mut observation_journal = original.clone();
    observation_journal.authority.settled.insert(
        "stage-admission".into(),
        serde_json::from_value(admission(source_plan.clone())).unwrap(),
    );
    let observed_request = json!({"version":2,"resource_generation":1,"provider_sequence":9,"operation":"resource_plan","transition_id":"transition","operation_id":"observe-op","target":stream,"phase":"stream.prepare","settings_revision":"one","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":requirements,"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[staged_output],"control_results":[]});
    let observed_request = serde_json::to_value(
        serde_json::from_value::<amaterasu_extension_api::ProviderRequest>(observed_request)
            .unwrap(),
    )
    .unwrap();
    {
        let work = observation_journal
            .receipts
            .get_mut("work")
            .unwrap()
            .work
            .as_mut()
            .unwrap();
        work.composition_progress = None;
        work.provider_graph = Some(serde_json::from_value(json!({"order":["owner","dependent"],"dependencies":{"owner":[],"dependent":["owner"]}})).unwrap());
        work.states.insert("owner".into(), serde_json::from_value(json!({"provider":"owner","participant":participant,"request":stage_request,"phase":"prepared","plan":source_plan,"admission_id":"stage-admission","pending_receipt":null,"status_polls":0,"last_sequence":2,"dispatch":null,"terminal_dispatch":null,"response":stage_response})).unwrap());
        work.owner_observations.insert(serde_json::to_string(&("dependent", "owner")).unwrap(), serde_json::from_value(json!({"participant":participant,"request":observed_request,"phase":"ready","dispatch":{"call":call("plan",observed_request.clone(),"observe-op"),"state":{"state":"outcome_unknown"}},"plan":null})).unwrap());
    }
    let observed_work = observation_journal.receipts["work"].work.as_ref().unwrap();
    let observed_state = observed_work.owner_observations.values().next().unwrap();
    super::super::plan_observation::PlanObservationDriver::new(observed_state.clone()).unwrap();
    assert!(
        super::super::observation_scheduler::validate_work_observations(
            &observation_journal,
            observed_work
        )
    );
    save(&coordinator.shared, observation_journal).unwrap();
    assert_eq!(
        coordinator.verify_provider("owner", &verification(&observed_request))["status"],
        "succeeded"
    );
    assert_eq!(
        coordinator.verify_provider("foreign", &verification(&observed_request))["status"],
        "rejected"
    );
    let mut forged_observation = observed_request.clone();
    forged_observation["provider_sequence"] = json!(10);
    assert_eq!(
        coordinator.verify_provider("owner", &verification(&forged_observation))["status"],
        "rejected"
    );
    save(&coordinator.shared, original.clone()).unwrap();
    let mut root_request = plan_request.clone();
    root_request["dependency_inputs"] = json!([output]);
    let mut tampered = original.clone();
    tampered
        .receipts
        .get_mut("work")
        .unwrap()
        .work
        .as_mut()
        .unwrap()
        .composition_progress
        .as_mut()
        .unwrap()
        .finalize
        .as_mut()
        .unwrap()
        .owner_plan
        .request = serde_json::from_value(root_request.clone()).unwrap();
    save(&coordinator.shared, tampered).unwrap();
    assert_eq!(
        coordinator.verify_provider("owner", &verification(&root_request))["status"],
        "rejected"
    );
    let mut missing_stage = original.clone();
    let mut encoded = serde_json::to_value(&missing_stage.composition).unwrap();
    encoded["staged_outputs"] = json!([]);
    missing_stage.composition = serde_json::from_value(encoded).unwrap();
    save(&coordinator.shared, missing_stage).unwrap();
    assert_eq!(
        coordinator.verify_provider("owner", &verification(&plan_request))["status"],
        "rejected"
    );
    save(&coordinator.shared, original).unwrap();
    let mut payload = json!({"operation_id":"refresh", "target":stream, "provider_request":plan_request, "domains":[{"domain":"domain","revision":2,"borrowers":{}}],"resources":[]});
    payload["provider_request"]["provider_sequence"] = json!(99);
    assert_eq!(
        coordinator
            .refresh_provider_observations("owner", "owner", payload.clone(), "forged".into())
            .unwrap_err(),
        "finalize_refresh_mismatch"
    );
    payload["provider_request"]["provider_sequence"] = json!(2);
    assert_eq!(
        coordinator
            .refresh_provider_observations("other", "other", payload.clone(), "foreign".into())
            .unwrap_err(),
        "provider_not_selected"
    );
    assert_eq!(
        coordinator
            .refresh_provider_observations("owner", "owner", payload, "exact".into())
            .unwrap()["status"],
        "succeeded"
    );
    assert_eq!(
        coordinator.shared.borrow().value.authority.snapshot.domains["domain"].revision,
        2
    );
    let _ = std::fs::remove_dir_all(path);
}
