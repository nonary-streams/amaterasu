//! Bounded sequences for shared-reference operation invariants.
use super::*;
use amaterasu_module_sdk::virtual_display_service::{Mode, ModeAcceptance};
use serde_json::json;
use std::collections::BTreeSet;

struct SharedBackend {
    references: BTreeSet<String>,
    revision: u64,
}
impl SharedBackend {
    fn new() -> Self {
        Self {
            references: ["peer".into()].into(),
            revision: 0,
        }
    }
    fn topology(&self) -> TopologyRevision {
        TopologyRevision {
            domain_id: "domain".into(),
            revision: self.revision,
        }
    }
}
impl Backend for SharedBackend {
    type Admission = ();
    fn admit(&mut self, _: &str, operation: Operation) -> (Value, Option<()>) {
        match operation {
            Operation::Status { reference_id, .. } => {
                let owned = reference_id.map(|reference| self.references.contains(&reference));
                let monitors = if self.references.is_empty() {
                    vec![]
                } else {
                    vec![
                        json!({"state":"ready", "specification":{"identity":"display","name":"Display","width":1920,"height":1080,"refresh_millihz":60000,"hdr_enabled":false}, "virtual_display_layout":"extended", "dd_virtual_display_scale":0}),
                    ]
                };
                (
                    json!({"ok":true,"enabled":true,"recovery_ready":true,"reference_owned":owned,
                    "mutation_domain":{"id":"domain","revision":self.revision,"pending_mutations":0},"monitors":monitors}),
                    None,
                )
            }
            _ => (json!({"ok":false}), None),
        }
    }
    fn admit_with_effect(&mut self, _: &str, operation: Operation) -> MutationResult<()> {
        let Operation::Create {
            reference_id: Some(reference),
            ..
        } = operation
        else {
            panic!("unexpected operation")
        };
        let inserted = self.references.insert(reference);
        MutationResult {
            response: json!({"ok":true}),
            admission: inserted.then_some(()),
            effect: if inserted {
                MutationEffect::Attempted
            } else {
                MutationEffect::Unchanged
            },
            confirmed_shared_release: None,
        }
    }
    fn completion_lost(&mut self, _: Option<()>) {}
    fn remove_owned(&mut self, _: &str, owner: &OwnerReference) -> RemovalOutcome {
        if !self.references.remove(&owner.reference_id) {
            return RemovalOutcome::Unchanged(
                json!({"ok":true,"removed":false,"reference_released":false,"reference_owned":false}),
            );
        }
        if self.references.is_empty() {
            self.revision += 1;
            RemovalOutcome::Attempted(json!({"ok":true,"removed":true}))
        } else {
            RemovalOutcome::Attempted(
                json!({"ok":true,"removed":false,"reference_released":true,"reference_owned":false}),
            )
        }
    }
}
fn owner(reference: &str) -> OwnerReference {
    OwnerReference {
        identity: "display".into(),
        reference_id: reference.into(),
    }
}
fn create(id: &str, topology: TopologyRevision) -> Request {
    Request::Create {
        owner: owner("stream"),
        operation_id: id.into(),
        expected: topology,
        mode: Mode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            hdr: false,
        },
        acceptance: ModeAcceptance {
            max_refresh_delta_ppm: 0,
            require_hdr_match: true,
        },
        name: "Display".into(),
        layout: Layout::Extended,
        scale: Some(0),
    }
}
fn run(adapter: &mut Adapter, backend: &mut SharedBackend, request: Request) -> Reply {
    adapter.invocation(backend).execute("alice", &request)
}
fn effect(reply: Reply, expected: EffectState) -> bool {
    matches!(reply, Reply::Effect { state, .. } if state == expected)
}

#[test]
fn bounded_shared_reference_sequences_preserve_terminal_and_unresolved_outcomes() {
    const ACTIONS: usize = 5;
    const STEPS: usize = 4;
    for mut trace in 0..ACTIONS.pow(STEPS as u32) {
        let mut backend = SharedBackend::new();
        let mut adapter = Adapter::default();
        let expected = backend.topology();
        assert!(effect(
            run(&mut adapter, &mut backend, create("old", expected)),
            EffectState::Pending
        ));
        let expected = backend.topology();
        assert!(effect(
            run(
                &mut adapter,
                &mut backend,
                Request::Remove {
                    owner: owner("stream"),
                    operation_id: "initial-remove".into(),
                    expected
                }
            ),
            EffectState::Completed
        ));
        let old_recover = || Request::Recover {
            owner: owner("stream"),
            operation_id: "old".into(),
        };
        assert!(effect(
            run(&mut adapter, &mut backend, old_recover()),
            EffectState::Absent
        ));
        // A separately fenced epoch must not be settled by later releases.
        let expected = backend.topology();
        adapter.operations.insert(
            ("alice".into(), "unresolved".into()),
            OperationRecord {
                request: create("unresolved", expected),
                reply: Invocation::<SharedBackend>::effect(EffectState::Unknown, None),
                cleanup: false,
                creation_fenced: true,
                fenced_by_removal: None,
            },
        );
        let mut old_released = false;
        let mut completed_removals = vec!["initial-remove".to_string()];
        for step in 0..STEPS {
            let action = trace % ACTIONS;
            trace /= ACTIONS;
            match action {
                0 => {
                    let expected = backend.topology();
                    assert!(effect(
                        run(
                            &mut adapter,
                            &mut backend,
                            create(&format!("create-{step}"), expected)
                        ),
                        EffectState::Pending
                    ));
                }
                1 if backend.references.contains("stream") => {
                    let operation_id = format!("remove-{step}");
                    let expected = backend.topology();
                    assert!(effect(
                        run(
                            &mut adapter,
                            &mut backend,
                            Request::Remove {
                                owner: owner("stream"),
                                operation_id: operation_id.clone(),
                                expected
                            }
                        ),
                        EffectState::Completed
                    ));
                    completed_removals.push(operation_id);
                }
                2 => {
                    let reply = run(&mut adapter, &mut backend, old_recover());
                    assert!(effect(
                        reply,
                        if old_released {
                            EffectState::Unknown
                        } else {
                            EffectState::Absent
                        }
                    ));
                }
                3 => {
                    let reply = run(
                        &mut adapter,
                        &mut backend,
                        Request::ReleaseOperation {
                            owner: owner("stream"),
                            operation_id: "old".into(),
                        },
                    );
                    assert_eq!(
                        reply,
                        if old_released {
                            Reply::NotOwned
                        } else {
                            Reply::OperationReleased
                        }
                    );
                    old_released = true;
                }
                _ => {}
            }
            assert!(backend.references.contains("peer"));
            assert!(effect(
                run(
                    &mut adapter,
                    &mut backend,
                    Request::Inspect {
                        owner: owner("peer")
                    }
                ),
                EffectState::Applied
            ));
            if !old_released {
                assert!(effect(
                    run(&mut adapter, &mut backend, old_recover()),
                    EffectState::Absent
                ));
            }
            assert!(effect(
                run(
                    &mut adapter,
                    &mut backend,
                    Request::Recover {
                        owner: owner("stream"),
                        operation_id: "unresolved".into()
                    }
                ),
                EffectState::Unknown
            ));
            for id in &completed_removals {
                assert!(effect(
                    run(
                        &mut adapter,
                        &mut backend,
                        Request::Recover {
                            owner: owner("stream"),
                            operation_id: id.clone()
                        }
                    ),
                    EffectState::Completed
                ));
            }
        }
    }
}
