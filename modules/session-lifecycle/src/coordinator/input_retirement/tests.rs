use super::*;
use amaterasu_runtime::Broker;
use std::path::PathBuf;

struct Fixture {
    coordinator: Option<Coordinator>,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("input-retirement-{}", identifier().unwrap()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            coordinator: Some(Coordinator::open(&root.join("journal.json")).unwrap()),
            root,
        }
    }

    fn coordinator(&mut self) -> &mut Coordinator {
        self.coordinator.as_mut().unwrap()
    }

    fn receipt(&mut self, token: &str) -> InputBindingRetirement {
        InputBindingRetirement {
            version: INPUT_BINDING_RETIREMENT_VERSION,
            token: token.into(),
            target: LifecycleTarget::Stream {
                host_epoch: self.coordinator().shared.borrow().value.host_epoch.clone(),
                session_id: "closed-session".into(),
                stream_id: token.into(),
                generation: 1,
            },
            revoke_operation_id: format!("revoke-{token}"),
            input_boot_epoch: "provider-boot".into(),
        }
    }

    fn persist(&mut self, receipt: InputBindingRetirement) {
        let coordinator = self.coordinator();
        let mut next = coordinator.shared.borrow().value.clone();
        next.inputs.record_retirement(receipt).unwrap();
        save(&coordinator.shared, next).unwrap();
    }

    fn pending(&mut self) -> usize {
        self.coordinator()
            .shared
            .borrow()
            .value
            .inputs
            .pending_retirements()
            .count()
    }

    fn seed_revocation(&mut self, saved_boot: bool) -> (String, LifecycleTarget) {
        let coordinator = self.coordinator();
        let mut next = coordinator.shared.borrow().value.clone();
        let epoch = next.host_epoch.clone();
        let binding = crate::input_authority::ActiveBinding {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
            geometry_revision: 1,
            input_consumer: "input-provider".into(),
            allowed_actions: Default::default(),
        };
        let token = next.inputs.issue_input_binding(&binding).unwrap();
        next.inputs.fence(&binding);
        next.input_bindings.insert(token.clone(), binding);
        if saved_boot {
            next.input_boot_epochs
                .insert(token.clone(), "provider-boot".into());
        }
        next.revoked_inputs.insert(token.clone());
        let session: SessionRecord = serde_json::from_value(json!({
            "host_epoch":epoch,"id":"session","generation":1,"phase":"ready",
            "owner":{"caller":"transport","initiator":"client"},
            "delegates":[],"application_ref":null,"settings_revision":"one",
            "application_retired":false,"streams":{
                "stream":{"generation":1,"phase":"active","candidate":null,
                    "binding_owner":"capture","required":["input"],
                    "acknowledgements":[],"retirements":[]}
            }
        }))
        .unwrap();
        next.sessions.insert("session".into(), session);
        save(&coordinator.shared, next).unwrap();
        let target = LifecycleTarget::Stream {
            host_epoch: epoch,
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        (token, target)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.coordinator.take();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn pending_retirement_metadata_survives_restart_without_reopening_input_authority() {
    let mut fixture = Fixture::new();
    let receipt = fixture.receipt("binding");
    fixture.persist(receipt.clone());
    fixture.coordinator.take();
    fixture.coordinator = Some(Coordinator::open(&fixture.root.join("journal.json")).unwrap());
    assert_eq!(fixture.pending(), 1);
    let journal = fixture.coordinator().shared.borrow();
    assert!(journal.value.input_bindings.is_empty());
    assert!(journal.value.input_boot_epochs.is_empty());
    assert_eq!(
        journal.value.inputs.pending_retirements().next(),
        Some(&receipt)
    );
}

#[test]
fn unchanged_consumer_acknowledges_replacement_input_providers_through_the_sdk() {
    for provider in ["independent-input-a", "independent-input-b"] {
        let mut fixture = Fixture::new();
        let receipt = fixture.receipt("binding");
        fixture.persist(receipt.clone());
        let configuration = serde_json::from_value(json!({
            "principals": {
                "coordinator": {"call":[INPUT_SERVICE]},
                provider: {"provide":[INPUT_SERVICE]}
            }
        }))
        .unwrap();
        let mut broker = Broker::new(configuration).unwrap();
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::Register {
                    service: INPUT_SERVICE.into()
                }
            ),
            Response::Ok
        ));
        let mut dispatched = 0;
        for _ in 0..8 {
            fixture
                .coordinator()
                .advance_input_retirement_acknowledgements(&mut |command| {
                    Ok(broker.dispatch("coordinator", command))
                })
                .unwrap();
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = broker.dispatch(provider, Command::Poll {})
            {
                assert_eq!(caller, "coordinator");
                assert_eq!(
                    payload,
                    InputRequest::AcknowledgeBindingRetirement { receipt: &receipt }.payload()
                );
                dispatched += 1;
                assert!(matches!(
                    broker.dispatch(
                        provider,
                        Command::Complete {
                            request: id,
                            payload: json!({"ok":true,"status":"acknowledged","receipt":receipt})
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        assert_eq!(dispatched, 1);
        assert_eq!(fixture.pending(), 0);
    }
}

#[test]
fn wrong_acknowledgement_never_discards_native_retirement_metadata() {
    let mut fixture = Fixture::new();
    let receipt = fixture.receipt("binding");
    fixture.persist(receipt.clone());
    let mut wrong = receipt.clone();
    wrong.input_boot_epoch = "another-instance".into();
    let mut send = |command| {
        Ok(match command {
            Command::Request { .. } => Response::Accepted { id: 1 },
            Command::TakeResult { .. } => Response::Result {
                payload: json!({"ok":true,"status":"acknowledged","receipt":wrong}),
            },
            Command::ReleaseResult { .. } => Response::Ok,
            _ => panic!("unexpected command {command:?}"),
        })
    };
    for _ in 0..3 {
        fixture
            .coordinator()
            .advance_input_retirement_acknowledgements(&mut send)
            .unwrap();
    }
    assert_eq!(fixture.pending(), 1);
}

#[test]
fn failed_result_release_retains_exact_acknowledgement_request() {
    let mut fixture = Fixture::new();
    let receipt = fixture.receipt("binding");
    fixture.persist(receipt.clone());
    let mut requests = 0;
    for _ in 0..4 {
        fixture
            .coordinator()
            .advance_input_retirement_acknowledgements(&mut |command| {
                Ok(match command {
                    Command::Request { .. } => {
                        requests += 1;
                        Response::Accepted { id: 7 }
                    }
                    Command::TakeResult { request: 7 } => Response::Result {
                        payload: json!({"ok":true,"status":"acknowledged","receipt":receipt}),
                    },
                    Command::ReleaseResult { request: 7 } => Response::Error {
                        code: "busy".into(),
                        message: String::new(),
                    },
                    _ => panic!("unexpected command {command:?}"),
                })
            })
            .unwrap();
    }
    assert_eq!(fixture.pending(), 1);
    fixture
        .coordinator()
        .advance_input_retirement_acknowledgements(&mut |command| {
            Ok(match command {
                Command::ReleaseResult { request: 7 } => Response::Ok,
                _ => panic!("unexpected command {command:?}"),
            })
        })
        .unwrap();
    assert_eq!(requests, 1);
    assert_eq!(fixture.pending(), 0);
}

#[test]
fn native_retirement_is_durable_before_any_optional_acknowledgement() {
    for (advertises_ack, saved_boot) in [(true, true), (true, false), (false, true)] {
        let mut fixture = Fixture::new();
        let (token, target) = fixture.seed_revocation(saved_boot);
        let mut revoke_operation = String::new();
        let mut expected_receipt = None;
        for _ in 0..3 {
            fixture
                .coordinator()
                .advance_validations(&mut |command| {
                    Ok(match command {
                        Command::Request {
                            service, payload, ..
                        } => {
                            assert_eq!(service, INPUT_SERVICE);
                            assert_eq!(payload["action"], "revoke_binding");
                            revoke_operation = payload["operation_id"].as_str().unwrap().into();
                            Response::Accepted { id: 11 }
                        }
                        Command::TakeResult { request: 11 } => {
                            let receipt = InputBindingRetirement {
                                version: INPUT_BINDING_RETIREMENT_VERSION,
                                token: token.clone(),
                                target: target.clone(),
                                revoke_operation_id: revoke_operation.clone(),
                                input_boot_epoch: "provider-boot".into(),
                            };
                            expected_receipt = Some(receipt.clone());
                            let mut payload =
                                json!({"ok":true,"status":"retired","token":token,"target":target});
                            if advertises_ack {
                                payload["retirement_receipt"] = json!(receipt);
                            }
                            Response::Result { payload }
                        }
                        Command::ReleaseResult { request: 11 } => Response::Ok,
                        _ => panic!("native proof was not persisted before Ack: {command:?}"),
                    })
                })
                .unwrap();
        }
        let disk: Journal =
            serde_json::from_slice(&std::fs::read(fixture.root.join("journal.json")).unwrap())
                .unwrap();
        assert!(
            disk.sessions["session"].streams["stream"]
                .retirements
                .contains("input")
        );
        assert!(!disk.input_bindings.contains_key(&token));
        assert_eq!(
            disk.inputs.pending_retirements().count(),
            usize::from(advertises_ack)
        );

        let journal_path = fixture.root.join("journal.json");
        fixture
            .coordinator()
            .advance_validations(&mut |command| {
                assert!(
                    advertises_ack,
                    "legacy providers must not need the optional operation"
                );
                let Command::Request {
                    service, payload, ..
                } = command
                else {
                    panic!("unexpected command {command:?}")
                };
                assert_eq!(service, INPUT_SERVICE);
                assert_eq!(
                    payload,
                    InputRequest::AcknowledgeBindingRetirement {
                        receipt: expected_receipt.as_ref().unwrap()
                    }
                    .payload()
                );
                let persisted: Journal =
                    serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
                assert_eq!(
                    persisted.inputs.pending_retirements().next(),
                    expected_receipt.as_ref()
                );
                Ok(Response::Accepted { id: 12 })
            })
            .unwrap();
    }
}

#[test]
fn lost_native_result_release_acknowledgement_never_loses_retirement_proof() {
    let mut fixture = Fixture::new();
    let (token, target) = fixture.seed_revocation(true);
    let mut operation = String::new();
    let mut takes = 0;
    let mut releases = 0;
    let mut requests = 0;
    for _ in 0..4 {
        fixture.coordinator().advance_validations(&mut |command| match command {
            Command::Request { payload, .. } => {
                assert_eq!(payload["action"], "revoke_binding");
                operation = payload["operation_id"].as_str().unwrap().into();
                requests += 1;
                Ok(Response::Accepted { id:11 })
            }
            Command::TakeResult { request:11 } => {
                takes += 1;
                Ok(Response::Result { payload:json!({"status":"retired","token":token,"target":target,
                    "retirement_receipt":InputBindingRetirement {
                        version:INPUT_BINDING_RETIREMENT_VERSION, token:token.clone(), target:target.clone(),
                        revoke_operation_id:operation.clone(), input_boot_epoch:"provider-boot".into(),
                    }}) })
            }
            Command::ReleaseResult { request:11 } => {
                releases += 1;
                if releases == 1 { Err("lost release acknowledgement".into()) }
                else { Ok(Response::Error { code:"not_found".into(), message:String::new() }) }
            }
            _ => panic!("unexpected command {command:?}"),
        }).unwrap();
    }
    assert_eq!((requests, takes, releases), (1, 1, 2));
    assert_eq!(fixture.pending(), 1);
    assert!(
        fixture.coordinator().shared.borrow().value.sessions["session"].streams["stream"]
            .retirements
            .contains("input")
    );
}

#[test]
fn saved_epoch_mismatch_fences_only_the_exact_target() {
    let mut fixture = Fixture::new();
    let (token, target) = fixture.seed_revocation(true);
    let mut operation = String::new();
    for _ in 0..4 {
        fixture.coordinator().advance_validations(&mut |command| Ok(match command {
            Command::Request { payload, .. } => {
                assert_eq!(payload["action"], "revoke_binding");
                operation = payload["operation_id"].as_str().unwrap().into();
                Response::Accepted { id:11 }
            }
            Command::TakeResult { request:11 } => Response::Result { payload:json!({"status":"retired","token":token,"target":target,
                "retirement_receipt":InputBindingRetirement {
                    version:INPUT_BINDING_RETIREMENT_VERSION, token:token.clone(), target:target.clone(),
                    revoke_operation_id:operation.clone(), input_boot_epoch:"different-provider".into(),
                }}) },
            Command::ReleaseResult { request:11 } => Response::Ok,
            _ => panic!("unexpected command {command:?}"),
        })).unwrap();
    }
    assert_eq!(fixture.pending(), 0);
    let journal = fixture.coordinator().shared.borrow();
    assert!(journal.value.input_bindings.contains_key(&token));
    assert!(matches!(
        journal.value.sessions["session"].streams["stream"].phase,
        StreamPhase::RecoveryRequired
    ));
    assert!(
        !journal.value.sessions["session"].streams["stream"]
            .retirements
            .contains("input")
    );
}

#[test]
fn blocked_receipts_do_not_starve_independent_acknowledgements() {
    let mut fixture = Fixture::new();
    for index in 0..9 {
        let receipt = fixture.receipt(&format!("binding-{index}"));
        fixture.persist(receipt);
    }
    let mut requests = BTreeMap::new();
    for _ in 0..8 {
        fixture.coordinator().advance_input_retirement_acknowledgements(&mut |command| Ok(match command {
            Command::Request { payload, .. } => {
                let id = requests.len() as u64 + 1;
                requests.insert(id, payload["receipt"].clone());
                Response::Accepted { id }
            }
            Command::TakeResult { request } if requests[&request]["token"] == "binding-8" => {
                Response::Result { payload:json!({"ok":true,"status":"acknowledged","receipt":requests[&request]}) }
            }
            Command::TakeResult { .. } => Response::Pending,
            Command::ReleaseResult { .. } => Response::Ok,
            _ => panic!("unexpected command {command:?}"),
        })).unwrap();
    }
    assert_eq!(requests.len(), 9);
    assert_eq!(fixture.pending(), 8);
    assert!(
        fixture
            .coordinator()
            .shared
            .borrow()
            .value
            .inputs
            .pending_retirements()
            .all(|receipt| receipt.token != "binding-8")
    );
}

#[test]
fn failed_native_proof_write_cannot_send_ack_or_repeat_the_native_revoke() {
    let mut fixture = Fixture::new();
    let (token, target) = fixture.seed_revocation(true);
    let mut operation = String::new();
    for _ in 0..2 {
        fixture.coordinator().advance_validations(&mut |command| Ok(match command {
            Command::Request { payload, .. } => {
                assert_eq!(payload["action"], "revoke_binding");
                operation = payload["operation_id"].as_str().unwrap().into();
                Response::Accepted { id:11 }
            }
            Command::TakeResult { request:11 } => Response::Result { payload:json!({"status":"retired","token":token,"target":target,
                "retirement_receipt":InputBindingRetirement {
                    version:INPUT_BINDING_RETIREMENT_VERSION, token:token.clone(), target:target.clone(),
                    revoke_operation_id:operation.clone(), input_boot_epoch:"provider-boot".into(),
                }}) },
            _ => panic!("unexpected command {command:?}"),
        })).unwrap();
    }
    let original_revision = {
        let mut shared = fixture.coordinator().shared.borrow_mut();
        let revision = &mut shared
            .value
            .sessions
            .get_mut("session")
            .unwrap()
            .settings_revision;
        std::mem::replace(revision, "x".repeat(1_100_000))
    };
    let failure = fixture
        .coordinator()
        .advance_validations(&mut |command| match command {
            Command::ReleaseResult { request: 11 } => Ok(Response::Ok),
            _ => panic!("unexpected command {command:?}"),
        });
    assert_eq!(failure.unwrap_err(), "journal_write_failed");
    assert_eq!(fixture.pending(), 0);
    let disk: Journal =
        serde_json::from_slice(&std::fs::read(fixture.root.join("journal.json")).unwrap()).unwrap();
    assert!(disk.input_bindings.contains_key(&token));
    assert!(
        !disk.sessions["session"].streams["stream"]
            .retirements
            .contains("input")
    );
    fixture
        .coordinator()
        .shared
        .borrow_mut()
        .value
        .sessions
        .get_mut("session")
        .unwrap()
        .settings_revision = original_revision;
    fixture.coordinator().advance_validations(&mut |command| -> ModuleResult<Response> {
        panic!("durability retry must reuse exact native result without another broker call: {command:?}");
    }).unwrap();
    assert_eq!(fixture.pending(), 1);
}

#[test]
fn retained_binding_budget_reserves_live_and_acknowledgement_obligations() {
    let mut fixture = Fixture::new();
    let (_, target) = fixture.seed_revocation(true);
    let mut next = fixture.coordinator().shared.borrow().value.clone();
    for index in 0..(crate::input_authority::MAX_RETAINED_INPUT_BINDINGS - 1) {
        next.inputs
            .record_retirement(fixture.receipt(&format!("retired-{index}")))
            .unwrap();
    }
    save(&fixture.coordinator().shared, next).unwrap();
    let request =
        json!({"operation":"issue_input_binding","operation_id":"fresh-binding","target":target});
    assert_eq!(
        fixture
            .coordinator()
            .security_operation("transport", "client", &request)["reason"],
        "input_retirement_ack_pending"
    );
    let mut next = fixture.coordinator().shared.borrow().value.clone();
    let released = next.inputs.pending_retirements().next().unwrap().clone();
    next.inputs.acknowledge_retirement(&released).unwrap();
    save(&fixture.coordinator().shared, next).unwrap();
    assert_ne!(
        fixture
            .coordinator()
            .security_operation("transport", "client", &request)["reason"],
        "input_retirement_ack_pending"
    );
}
