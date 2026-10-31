use super::*;
use serde_json::json;

struct Journal;
impl AuthorityJournal for Journal {
    fn persist(&mut self, _: &AuthorityState) -> ModuleResult<()> {
        Ok(())
    }
}

fn selection() -> ProviderSelection {
    let method = |name: &str| json!({"service":"display/1","method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
    let participant: ParticipantSettings = serde_json::from_value(json!({"provider":"display","plan_method":method("plan"),"callback_method":method("callback"),"status_method":method("status"),"capabilities":[],"cleanup_methods":[method("cleanup")],"settings_revision":"revision","settings":{}})).unwrap();
    let approvals = [&participant.plan_method,&participant.callback_method,&participant.status_method].into_iter().map(|m| MethodApproval {method:m.clone(),broker_approval:json!({"provider":"display","method":{"service":m.service,"method":m.method,"interface_major":m.interface_major,"interface_minor":m.interface_minor}})}).collect();
    ProviderSelection {
        cleanup_methods: participant.cleanup_methods.clone(),
        participant,
        approvals,
        capabilities: BTreeSet::new(),
        authorized_borrowers: BTreeSet::new(),
    }
}

fn unresolved(id: &str, revision: u64) -> ResourcePlan {
    serde_json::from_value(json!({"plan_id":format!("plan-{id}"),"revision":1,"resources":[{"provider":"display","resource_id":id,"incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}],"mutation_domains":[{"domain":"topology","expected_revision":revision,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[{"obligation_id":format!("cleanup-{id}"),"provider":"display","resource_id":id,"mutation_domain":"topology","depends_on":[],"cleanup_method":{"service":"display/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1},"payload":{}}],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap()
}
fn resource(id: &str) -> ResourceRef {
    ResourceRef {
        provider: "display".into(),
        resource_id: id.into(),
        incarnation: "native-1".into(),
        revision: 1,
        execution_domain: "seat".into(),
        mutation_domains: ["topology".into()].into(),
    }
}
fn outcome(id: &str) -> CleanupOutcome {
    CleanupOutcome {
        obligation_id: format!("cleanup-{id}"),
        state: CleanupState::Released,
        reason: None,
    }
}
fn domain(revision: u64) -> (String, DomainState) {
    (
        "topology".into(),
        DomainState {
            revision,
            borrowers: BTreeMap::new(),
        },
    )
}
fn authority() -> CoordinatorPrepareAuthority<Journal> {
    CoordinatorPrepareAuthority::new(AuthorityState::default(), vec![selection()], Journal).unwrap()
}

#[test]
fn admitted_unresolved_resource_materializes_and_can_refresh() {
    let mut authority = authority();
    let admission = authority
        .admit_plan(&selection().participant, &unresolved("output", 1))
        .unwrap();
    authority
        .finish_admission_with_resource("display", &admission, Some(resource("output")))
        .unwrap();
    assert_eq!(
        authority.state.snapshot.resources["display"]["output"],
        resource("output")
    );
    let mut next = resource("output");
    next.revision = 2;
    next.incarnation = "native-2".into();
    authority
        .refresh_observations("display", &[domain(2)], std::slice::from_ref(&next))
        .unwrap();
    assert_eq!(
        authority.state.snapshot.resources["display"]["output"],
        next
    );
    assert!(authority.state.settled.contains_key(&admission));
}

#[test]
fn materialization_rejects_identity_scope_and_domain_substitution_atomically() {
    let mut authority = authority();
    let admission = authority
        .admit_plan(&selection().participant, &unresolved("output", 1))
        .unwrap();
    let before = serde_json::to_value(&authority.state).unwrap();
    let mut cases = Vec::new();
    let mut candidate = resource("foreign");
    cases.push(candidate.clone());
    candidate = resource("output");
    candidate.provider = "other".into();
    cases.push(candidate.clone());
    candidate = resource("output");
    candidate.execution_domain = "other-seat".into();
    cases.push(candidate.clone());
    candidate = resource("output");
    candidate.mutation_domains = ["other-domain".into()].into();
    cases.push(candidate.clone());
    candidate = resource("output");
    candidate.incarnation.clear();
    cases.push(candidate.clone());
    candidate = resource("output");
    candidate.revision = 0;
    cases.push(candidate);
    for candidate in cases {
        assert!(
            authority
                .finish_admission_with_resource("display", &admission, Some(candidate))
                .is_err()
        );
        assert_eq!(serde_json::to_value(&authority.state).unwrap(), before);
        assert!(authority.state.admissions.contains_key(&admission));
        assert_eq!(authority.state.domain_locks["topology"], admission);
    }
}

#[test]
fn no_candidate_retains_reservation_and_only_exact_first_readback_is_accepted() {
    let mut authority = authority();
    let admission = authority
        .admit_plan(&selection().participant, &unresolved("output", 1))
        .unwrap();
    authority.finish_admission("display", &admission).unwrap();
    let before = serde_json::to_value(&authority.state).unwrap();
    let mut wrong = resource("output");
    wrong.execution_domain = "other-seat".into();
    assert!(
        authority
            .refresh_observations("display", &[domain(2)], &[wrong])
            .is_err()
    );
    assert!(
        authority
            .refresh_observations("display", &[domain(2)], &[resource("other")])
            .is_err()
    );
    assert_eq!(serde_json::to_value(&authority.state).unwrap(), before);
    authority
        .refresh_observations("display", &[domain(2)], &[resource("output")])
        .unwrap();
    assert_eq!(
        authority.state.snapshot.resources["display"]["output"],
        resource("output")
    );
}

#[test]
fn orphan_pruning_requires_no_references_locks_staleness_or_borrowers() {
    let mut state = AuthorityState::default();
    state
        .snapshot
        .resources
        .entry("display".into())
        .or_default()
        .insert("output".into(), resource("output"));
    state
        .snapshot
        .domains
        .insert("topology".into(), domain(1).1);
    state.refresh_required.insert("topology".into());
    prune_idle_orphan_resources(&mut state);
    assert!(state.snapshot.resources["display"].contains_key("output"));
    state.refresh_required.clear();
    state.domain_locks.insert("topology".into(), "lock".into());
    prune_idle_orphan_resources(&mut state);
    assert!(state.snapshot.resources["display"].contains_key("output"));
    state.domain_locks.clear();
    state
        .snapshot
        .domains
        .get_mut("topology")
        .unwrap()
        .borrowers
        .insert("borrower".into(), 1);
    prune_idle_orphan_resources(&mut state);
    assert!(state.snapshot.resources["display"].contains_key("output"));
    state
        .snapshot
        .domains
        .get_mut("topology")
        .unwrap()
        .borrowers
        .clear();
    state.settled.insert(
        "ref".into(),
        Admission {
            provider: "display".into(),
            selection: selection(),
            plan: unresolved("output", 1),
            rights: vec![],
        },
    );
    prune_idle_orphan_resources(&mut state);
    assert!(state.snapshot.resources["display"].contains_key("output"));
    state.settled.clear();
    prune_idle_orphan_resources(&mut state);
    assert!(state.snapshot.resources.is_empty());
}

#[test]
fn repeated_distinct_completed_resources_do_not_exhaust_retained_capacity() {
    let mut authority = authority();
    for index in 0..160 {
        let id = format!("output-{index}");
        let admission = authority
            .admit_plan(&selection().participant, &unresolved(&id, index + 1))
            .unwrap();
        authority
            .finish_admission_with_resource("display", &admission, Some(resource(&id)))
            .unwrap();
        authority
            .refresh_observations("display", &[domain(index + 2)], &[])
            .unwrap();
        authority
            .retire_settled("display", &admission, &[outcome(&id)])
            .unwrap();
        assert!(authority.state.snapshot.resources.is_empty());
        assert!(authority.state.settled.is_empty());
    }
}
