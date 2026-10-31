//! Direct coordinator regression using the broker and simulated providers.
use crate::{Coordinator, LifecycleSettings};
use amaterasu_module_sdk::{Command, Response};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "amaterasu.session-lifecycle/1";
const COORDINATOR: &str = "amaterasu.session-lifecycle";
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn session_borrowers(target: &Value, foreign: bool) -> Value {
    let session = target["session_id"].as_str().expect("session target");
    if foreign {
        json!({session: 1, "foreign-session": 1})
    } else {
        json!({session: 1})
    }
}

struct Harness {
    broker: Rc<RefCell<Broker>>,
    coordinator: Option<Coordinator>,
    settings: LifecycleSettings,
    root: PathBuf,
    request_sequence: u64,
    provider_calls: Vec<Value>,
    provider: bool,
    provider_invalid: bool,
    simulate_stage_refresh: bool,
    stage_refresh_request: Option<u64>,
    stage_refresh_done: bool,
    refresh_round: u64,
    deferred_finalize_plan: Option<(u64, Value, Value)>,
    verify_finalize: bool,
    finalize_verify_request: Option<(u64, u64, Value)>,
    finalize_pending: bool,
    pending_finalize_result: Option<Value>,
    control_verification: Option<(u64, u64, Value, &'static str)>,
    dependent_calls: Vec<Value>,
    events: Vec<(String, Value)>,
    staged_output: Option<Value>,
    native_live: bool,
    foreign_borrower: bool,
    corrupt_settlement_ack: bool,
    dependent_refresh: Option<(u64, u64, Value)>,
    cleanup_deferred: bool,
    registry_settings: Option<(u64, Value)>,
}

impl Harness {
    fn with_provider(provider: bool) -> Self {
        Self::with_registry(provider, None)
    }

    fn with_registry(provider: bool, registry_settings: Option<(u64, Value)>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-broker-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut configuration = json!({
            "principals": {
                COORDINATOR: {"provide": [SERVICE]},
                "transport": {"call": [SERVICE]},
                "capture": {"call": [SERVICE]},
                "amaterasu.input": {"call": [SERVICE], "provide": ["amaterasu.input/1"]},
                "launch": {"provide": ["amaterasu.launch/1"]}
            },
            "settings": {
                COORDINATOR: {
                    "journal_path": root.join("journal.json"),
                    "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},
                    "trusted_callers": ["transport", "capture", "amaterasu.input"],
                    "session_delegates": ["capture", "amaterasu.input"],
                    "consumers": {"capture":["capture"], "input":["amaterasu.input"]},
                    "operation_grants": {
                        "transport": ["prepare", "lookup", "snapshot", "prepare_stream", "start_stream", "pause_stream", "resume_stream", "end_session", "bind_application", "application_retired", "issue_input_binding"],
                        "capture": ["ack_binding", "retire_stream", "lookup"],
                        "amaterasu.input": ["ack_binding", "lookup", "validate_input_binding"]
                    }
                }
            }
        });
        let mut methods = Vec::new();
        if provider {
            configuration["principals"][COORDINATOR]["call"] = json!([
                "example.display/2",
                "example.settings/2",
                SERVICE,
                "amaterasu.input/1",
                "amaterasu.launch/1"
            ]);
            configuration["principals"]["display"] =
                json!({"provide":["example.display/2"],"call":[SERVICE]});
            configuration["principals"]["settings"] =
                json!({"provide":["example.settings/2"],"call":[SERVICE]});
            configuration["settings"][COORDINATOR]["trusted_callers"] = json!([
                "transport",
                "capture",
                "amaterasu.input",
                "display",
                "settings"
            ]);
            configuration["settings"][COORDINATOR]["operation_grants"]["display"] = json!([
                "verify_provider_epoch",
                "refresh_provider_observations",
                "verify_resource_finalize",
                "describe_resource_control",
                "verify_resource_control",
                "validate_resource_control",
                "verify_control_settlement"
            ]);
            configuration["settings"][COORDINATOR]["operation_grants"]["settings"] =
                json!(["verify_resource_control", "verify_control_settlement"]);
            let method_ref = |name: &str| json!({"service":"example.display/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
            configuration["settings"][COORDINATOR]["consumer_requirements"] = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]});
            configuration["settings"][COORDINATOR]["participants"] = json!([{"provider":"display","contract_version":2,"plan_method":method_ref("resource_plan"),"callback_method":method_ref("lifecycle_callback"),"status_method":method_ref("lifecycle_status"),"binding_validate_method":method_ref("binding_validate"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[method_ref("lifecycle_callback")]}]);
            let dependent_method = |name: &str| json!({"service":"example.settings/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
            configuration["settings"][COORDINATOR]["participants"].as_array_mut().unwrap().push(json!({"provider":"settings","contract_version":2,"depends_on":["display"],"plan_method":dependent_method("resource_plan"),"callback_method":dependent_method("lifecycle_callback"),"status_method":dependent_method("lifecycle_status"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[dependent_method("lifecycle_callback")]}));
            let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
            let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
            configuration["settings"][COORDINATOR]["schema_documents"] =
                json!([{"reference":schema,"document":document}]);
            configuration["settings"][COORDINATOR]["finalize_policies"] = json!([{"owner_provider":"display","owner_service":"example.display/2","method":method_ref("finalize"),"status_method":method_ref("lifecycle_status"),"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"example/1","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]}]);
            configuration["settings"][COORDINATOR]["control_policies"] = json!([{"dependent_provider":"settings","owner_provider":"display","owner_service":"example.display/2","owner_method":method_ref("control"),"status_method":method_ref("lifecycle_status"),"settlement_method":method_ref("settle"),"argument_schema":schema,"output_capability":"display/output","output_schema":schema,"max_access":"exclusive","domain_scope":"resource","mutation_domains":[]}]);
            for name in [
                "resource_plan",
                "lifecycle_callback",
                "lifecycle_status",
                "binding_validate",
                "finalize",
                "control",
                "settle",
            ] {
                let method = json!({"service":"example.display/2","method":name,"interface_major":2,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"});
                methods.push(method);
            }
            for name in ["resource_plan", "lifecycle_callback", "lifecycle_status"] {
                methods.push(json!({"service":"example.settings/2","method":name,"interface_major":2,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"}));
            }
            configuration["api_methods"]=json!(methods.iter().map(|method|json!({"provider":if method["service"] == "example.display/2" {"display"} else {"settings"},"callers":[COORDINATOR],"method":method})).collect::<Vec<_>>());
        }
        if registry_settings.is_some() {
            configuration["principals"][COORDINATOR]["call"]
                .as_array_mut()
                .unwrap()
                .push(json!("amaterasu.extension-registry/1"));
            configuration["principals"]["registry"] =
                json!({"provide":["amaterasu.extension-registry/1"]});
            configuration["settings"][COORDINATOR]["settings_registry_service"] =
                json!("amaterasu.extension-registry/1");
        }
        let settings: LifecycleSettings =
            serde_json::from_value(configuration["settings"][COORDINATOR].clone()).unwrap();
        let configuration = serde_json::from_value(configuration).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
        for method in methods {
            let response = broker.borrow_mut().dispatch(
                if method["service"] == "example.display/2" {
                    "display"
                } else {
                    "settings"
                },
                Command::RegisterApi {
                    method: serde_json::from_value(method).unwrap(),
                },
            );
            assert!(matches!(response, Response::Ok));
        }
        for (principal, service) in [
            ("amaterasu.input", "amaterasu.input/1"),
            ("launch", "amaterasu.launch/1"),
        ] {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    principal,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        if registry_settings.is_some() {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    "registry",
                    Command::Register {
                        service: "amaterasu.extension-registry/1".into()
                    }
                ),
                Response::Ok
            ));
        }
        let mut harness = Self {
            broker,
            coordinator: None,
            settings,
            root,
            request_sequence: 0,
            provider_calls: Vec::new(),
            provider,
            provider_invalid: false,
            simulate_stage_refresh: false,
            stage_refresh_request: None,
            stage_refresh_done: false,
            refresh_round: 0,
            deferred_finalize_plan: None,
            verify_finalize: false,
            finalize_verify_request: None,
            finalize_pending: false,
            pending_finalize_result: None,
            control_verification: None,
            dependent_calls: Vec::new(),
            events: Vec::new(),
            staged_output: None,
            native_live: false,
            foreign_borrower: false,
            corrupt_settlement_ack: false,
            dependent_refresh: None,
            cleanup_deferred: false,
            registry_settings,
        };
        harness.start();
        harness
    }

    fn start(&mut self) {
        let mut coordinator = Coordinator::open(&self.root.join("journal.json")).unwrap();
        coordinator.configure_for_test(self.settings.clone());
        self.coordinator = Some(coordinator);
        assert!(matches!(
            self.broker.borrow_mut().dispatch(
                COORDINATOR,
                Command::Register {
                    service: SERVICE.into()
                }
            ),
            Response::Ok
        ));
    }

    fn tick(&mut self) {
        let coordinator = self.coordinator.as_mut().unwrap();
        coordinator
            .advance(&mut |command| Ok(self.broker.borrow_mut().dispatch(COORDINATOR, command)))
            .unwrap();
        let polled = self
            .broker
            .borrow_mut()
            .dispatch(COORDINATOR, Command::Poll {});
        if let Response::Request {
            id,
            caller,
            initiator,
            service,
            payload,
            ..
        } = polled
        {
            let response = if service == SERVICE
                && self.settings.trusted_callers.contains(&caller)
                && payload
                    .get("operation")
                    .and_then(Value::as_str)
                    .is_some_and(|operation| {
                        self.settings
                            .operation_grants
                            .get(&caller)
                            .is_some_and(|grants| grants.contains(operation))
                    }) {
                coordinator.handle(&caller, &initiator, payload)
            } else {
                json!({"ok":false,"code":"permission_denied"})
            };
            assert!(matches!(
                self.broker.borrow_mut().dispatch(
                    COORDINATOR,
                    Command::Complete {
                        request: id,
                        payload: response
                    }
                ),
                Response::Ok
            ));
        }
        if self.provider {
            let request = self
                .broker
                .borrow_mut()
                .dispatch("display", Command::Poll {});
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = request
            {
                assert_eq!(caller, COORDINATOR);
                self.provider_calls.push(payload.clone());
                self.events.push(("display".into(), payload.clone()));
                let args = &payload["arguments"];
                if payload["method"] == "resource_plan" {
                    let stream = args["phase"] == "stream.prepare"
                        || args["phase"] == "stream.release"
                        || args["phase"] == "session.release";
                    let release =
                        args["phase"] == "stream.release" || args["phase"] == "session.release";
                    let finalize = args["operation"] == "resource_finalize_plan";
                    let resource = json!({"provider":"display","resource_id":"output","incarnation":if stream {json!("native")} else {Value::Null},"expected_revision":if stream {self.staged_output.as_ref().map(|output|output["resource"]["revision"].clone()).unwrap_or(json!(1))} else {Value::Null},"execution_domain":"seat","mutation_domains":["topology"],"access":if finalize || release {"exclusive"} else {"read"}});
                    let constraint = if finalize {
                        json!({"capture_provider":"capture","selector_schema":"example/1","resource_provider":"display","resource_id":"output","incarnation":"native","execution_domain":"seat","mode":args["requested_video"],"plan_revision":1,"geometry_revision":1})
                    } else {
                        Value::Null
                    };
                    let obligations = if args["phase"] == "session.prepare"
                        || args["phase"] == "session.release"
                    {
                        json!([{"obligation_id":"owner-session-resource","provider":"display","resource_id":"output","mutation_domain":"topology","cleanup_method":{"service":"example.display/2","method":"lifecycle_callback","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]}])
                    } else if finalize || release {
                        json!([{"obligation_id":"finalize-cleanup","provider":"display","resource_id":"output","mutation_domain":"topology","cleanup_method":{"service":"example.display/2","method":"lifecycle_callback","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]}])
                    } else {
                        json!([])
                    };
                    let plan = json!({"plan_id":if finalize {"finalize-plan"} else {"session-plan"},"revision":1,"resources":[resource],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower),"access":if finalize || release {"exclusive"} else {"read"}}],"required_capabilities":[],"constraints":{},"obligations":obligations,"affected_consumers":[],"consumer_change":"live","candidate_constraint":constraint,"allowed_degradation":[]});
                    if stream && self.simulate_stage_refresh {
                        self.stage_refresh_done = false;
                        self.stage_refresh_request = None;
                        self.refresh_round += 1;
                        self.deferred_finalize_plan = Some((id, plan, args["target"].clone()));
                    } else {
                        assert!(matches!(
                            self.broker.borrow_mut().dispatch(
                                "display",
                                Command::Complete {
                                    request: id,
                                    payload: json!({"status":"planned","plan":plan})
                                }
                            ),
                            Response::Ok
                        ));
                    }
                } else if payload["method"] == "lifecycle_callback"
                    && args["phase"] == "session.release"
                {
                    assert!(self.native_live);
                    self.native_live = false;
                    self.staged_output = None;
                    let outcomes: Vec<Value> = args["approved_plan"]["obligations"].as_array().unwrap().iter().map(|obligation|json!({"obligation_id":obligation["obligation_id"],"state":"released","reason":null})).collect();
                    assert!(matches!(self.broker.borrow_mut().dispatch("display",Command::Complete {request:id,payload:json!({"status":"cleanup","outcomes":outcomes,"outputs":[]})}),Response::Ok));
                } else if payload["method"] == "lifecycle_callback"
                    && args["phase"] == "stream.release"
                {
                    let outcomes: Vec<Value> = args["approved_plan"]["obligations"].as_array().unwrap().iter().map(|obligation| json!({"obligation_id":obligation["obligation_id"],"state":if self.cleanup_deferred {"deferred_restoration"} else {"released"},"reason":null})).collect();
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: json!({"status":"cleanup","outcomes":outcomes,"outputs":[self.staged_output.clone().expect("released owner output")]})
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "lifecycle_callback" {
                    let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
                    let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
                    let origin = if args["phase"] == "session.prepare" {
                        args["target"].clone()
                    } else {
                        self.provider_calls
                            .iter()
                            .find(|call| {
                                call["method"] == "lifecycle_callback"
                                    && call["arguments"]["phase"] == "session.prepare"
                            })
                            .unwrap()["arguments"]["target"]
                            .clone()
                    };
                    let initial = json!({"output_id":"screen-output","capability":"display/output","schema":schema,"producer":"display","producer_service":"example.display/2","origin":origin,"resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]},"domain_observations":[{"domain":"topology","revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower),"complete":true}],"data":if self.provider_invalid {json!("invalid")} else {json!({"ready":true,"configured":false,"snapshot_id":null})}});
                    let output = if args["phase"] == "stream.prepare" {
                        self.staged_output.clone().unwrap_or(initial)
                    } else {
                        initial
                    };
                    self.staged_output = Some(output.clone());
                    if args["phase"] == "session.prepare" {
                        self.native_live = true;
                    }
                    assert!(matches!(self.broker.borrow_mut().dispatch("display",Command::Complete {request:id,payload:json!({"status":"prepared","candidate":null,"obligations":args["approved_plan"]["obligations"],"outputs":[output]})}),Response::Ok));
                } else if payload["method"] == "control" || payload["method"] == "settle" {
                    let is_control = payload["method"] == "control";
                    let mut envelope = if is_control {
                        json!({"request":args})
                    } else {
                        args.clone()
                    };
                    envelope["operation"] = json!(if is_control {
                        "verify_resource_control"
                    } else {
                        "verify_control_settlement"
                    });
                    let response = self.broker.borrow_mut().dispatch(
                        "display",
                        Command::Request {
                            service: SERVICE.into(),
                            payload: envelope,
                            timeout_ms: 5000,
                            operation_id: format!("owner-verify-{id}"),
                            parent: Some(id),
                        },
                    );
                    let Response::Accepted { id: nested } = response else {
                        panic!("owner verification rejected: {response:?}")
                    };
                    let output = if is_control {
                        {
                            let mut output = self.staged_output.clone().expect("staged output");
                            let restoring = args["proposal"]["arguments"]["operation"] == "restore";
                            if restoring {
                                assert_eq!(
                                    args["proposal"]["arguments"]["snapshot_id"],
                                    output["data"]["snapshot_id"]
                                );
                            }
                            output["resource"]["revision"] =
                                json!(output["resource"]["revision"].as_u64().unwrap() + 1);
                            output["data"]["configured"] = json!(!restoring);
                            output["data"]["snapshot_id"] = json!("snapshot-1");
                            output
                        }
                    } else {
                        args["output"].clone()
                    };
                    self.control_verification = Some((
                        nested,
                        id,
                        output,
                        if is_control { "execute" } else { "valid" },
                    ));
                } else if payload["method"] == "lifecycle_status" {
                    assert_eq!(args["receipt_id"], "finalize-receipt");
                    let result = self
                        .pending_finalize_result
                        .take()
                        .expect("pending finalized result");
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: result
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "finalize" {
                    let candidate = json!({"capture_provider":"capture","selector_schema":"example/1","selector":{},"resource":args["output"]["resource"],"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":args["target"]["generation"]});
                    let result =
                        json!({"status":"applied","output":args["output"],"candidate":candidate});
                    if self.verify_finalize {
                        let payload =
                            json!({"operation":"verify_resource_finalize","request":args});
                        let response = self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Request {
                                service: SERVICE.into(),
                                payload,
                                timeout_ms: 5000,
                                operation_id: "verify-finalize-request".into(),
                                parent: Some(id),
                            },
                        );
                        let Response::Accepted { id: verify_id } = response else {
                            panic!("verify request rejected: {response:?}");
                        };
                        self.finalize_verify_request = Some((verify_id, id, result));
                    } else {
                        assert!(matches!(
                            self.broker.borrow_mut().dispatch(
                                "display",
                                Command::Complete {
                                    request: id,
                                    payload: result
                                }
                            ),
                            Response::Ok
                        ));
                    }
                } else {
                    panic!("unexpected provider method: {payload}");
                }
            }
        }
        let dependent_request = self
            .broker
            .borrow_mut()
            .dispatch("settings", Command::Poll {});
        if let Response::Request {
            id,
            caller,
            payload,
            ..
        } = dependent_request
        {
            assert_eq!(caller, COORDINATOR);
            self.dependent_calls.push(payload.clone());
            self.events.push(("settings".into(), payload.clone()));
            let args = &payload["arguments"];
            let response = if payload["method"] == "resource_plan" {
                let mut plan = json!({"plan_id":"dependent-plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[],"control_proposals":[]});
                if args["phase"] == "stream.prepare" || args["phase"] == "stream.release" {
                    let input = &args["dependency_inputs"][0];
                    let restoring = args["phase"] == "stream.release";
                    if restoring {
                        assert_eq!(input["data"]["snapshot_id"], "snapshot-1");
                    }
                    let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
                    let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
                    plan["resources"] = json!([{"provider":"display","resource_id":"output","incarnation":"native","expected_revision":input["resource"]["revision"],"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}]);
                    plan["mutation_domains"] = json!([{"domain":"topology","expected_revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower),"access":"exclusive"}]);
                    plan["obligations"] = json!([{"obligation_id":"restore-settings","provider":"settings","resource_id":"output","mutation_domain":"topology","cleanup_method":{"service":"example.settings/2","method":"lifecycle_callback","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]}]);
                    plan["control_proposals"] = json!([{"proposal_id":if restoring {"restore"} else {"configure"},"output_id":input["output_id"],"owner_provider":"display","owner_service":"example.display/2","target":args["target"],"resource":input["resource"],"method":{"service":"example.display/2","method":"control","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":if restoring {json!({"operation":"restore","snapshot_id":input["data"]["snapshot_id"]})} else {json!({"operation":"configure"})},"access":"exclusive","mutation_domains":["topology"]}]);
                }
                if args["phase"] == "stream.prepare" || args["phase"] == "stream.release" {
                    let refresh = json!({"operation":"refresh_provider_observations","operation_id":format!("dependent-observe-{id}"),"target":args["target"],"domains":[{"domain":"topology","revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower)}],"resources":[{"provider":"display","resource_id":"output","incarnation":"native","revision":self.staged_output.as_ref().unwrap()["resource"]["revision"],"execution_domain":"seat","mutation_domains":["topology"]}]});
                    let response = self.broker.borrow_mut().dispatch(
                        "display",
                        Command::Request {
                            service: SERVICE.into(),
                            payload: refresh,
                            timeout_ms: 5000,
                            operation_id: format!("dependent-observation-request-{id}"),
                            parent: None,
                        },
                    );
                    let Response::Accepted { id: refresh_id } = response else {
                        panic!("dependent observation not admitted: {response:?}")
                    };
                    self.dependent_refresh = Some((refresh_id, id, plan));
                    Value::Null
                } else {
                    json!({"status":"planned","plan":plan})
                }
            } else if payload["method"] == "lifecycle_callback" {
                if args["phase"] == "stream.release" || args["phase"] == "session.release" {
                    let outcomes:Vec<Value> = args["approved_plan"]["obligations"].as_array().unwrap().iter().map(|item|json!({"obligation_id":item["obligation_id"],"state":"released","reason":null})).collect();
                    if args["phase"] == "stream.release" {
                        assert_eq!(args["control_results"][0]["proposal_id"], "restore");
                    }
                    json!({"status":"cleanup","outcomes":outcomes,"outputs":[]})
                } else {
                    if args["phase"] == "stream.prepare" {
                        assert_eq!(args["control_results"][0]["proposal_id"], "configure");
                        assert_eq!(
                            args["control_results"][0]["output"]["data"]["configured"],
                            true
                        );
                        assert_eq!(
                            args["control_results"][0]["output"]["data"]["snapshot_id"],
                            "snapshot-1"
                        );
                    }
                    json!({"status":"prepared","candidate":null,"obligations":args["approved_plan"]["obligations"],"outputs":[]})
                }
            } else {
                panic!("unexpected dependent request: {payload}")
            };
            if !response.is_null() {
                assert!(matches!(
                    self.broker.borrow_mut().dispatch(
                        "settings",
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
        }
        if let Some((refresh, dependent, plan)) = self.dependent_refresh.clone() {
            let reply = self
                .broker
                .borrow_mut()
                .dispatch("display", Command::TakeResult { request: refresh });
            match reply {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert_eq!(payload["status"], "succeeded", "{payload}");
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch("display", Command::ReleaseResult { request: refresh }),
                        Response::Ok
                    ));
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "settings",
                            Command::Complete {
                                request: dependent,
                                payload: json!({"status":"planned","plan":plan})
                            }
                        ),
                        Response::Ok
                    ));
                    self.dependent_refresh = None;
                }
                other => panic!("dependent observation failed: {other:?}"),
            }
        }
        if let Some((verify, owner, output, expected)) = self.control_verification.clone() {
            let reply = self
                .broker
                .borrow_mut()
                .dispatch("display", Command::TakeResult { request: verify });
            match reply {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert_eq!(payload["status"], expected, "{payload}");
                    if expected == "execute" {
                        self.staged_output = Some(output.clone());
                    }
                    let mut output = output;
                    if expected == "valid" && self.corrupt_settlement_ack {
                        output["data"]["snapshot_id"] = json!("forged-snapshot");
                    }
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch("display", Command::ReleaseResult { request: verify }),
                        Response::Ok
                    ));
                    assert!(matches!(self.broker.borrow_mut().dispatch("display",Command::Complete {request:owner,payload:json!({"status":"applied","output":output,"candidate":null})}),Response::Ok));
                    self.control_verification = None;
                }
                other => panic!("control verification failed: {other:?}"),
            }
        }
        if self.simulate_stage_refresh && !self.stage_refresh_done {
            if let Some(request) = self.stage_refresh_request {
                let response = self
                    .broker
                    .borrow_mut()
                    .dispatch("display", Command::TakeResult { request });
                match response {
                    Response::Pending => {}
                    Response::Result { payload } => {
                        assert_eq!(payload["status"], "succeeded", "{payload}");
                        assert!(matches!(
                            self.broker
                                .borrow_mut()
                                .dispatch("display", Command::ReleaseResult { request }),
                            Response::Ok
                        ));
                        self.stage_refresh_done = true;
                        let (owner_request, plan, _) = self
                            .deferred_finalize_plan
                            .take()
                            .expect("pending finalize plan");
                        assert!(matches!(
                            self.broker.borrow_mut().dispatch(
                                "display",
                                Command::Complete {
                                    request: owner_request,
                                    payload: json!({"status":"planned","plan":plan})
                                }
                            ),
                            Response::Ok
                        ));
                    }
                    other => panic!("stage refresh failed: {other:?}"),
                }
            } else if let Some((owner_request, _, target)) = &self.deferred_finalize_plan {
                let provider_request =
                    &self.provider_calls.last().expect("stage plan")["arguments"];
                let payload = json!({"operation":"refresh_provider_observations","provider_request":provider_request,"operation_id":format!("stage-observe-{}",self.refresh_round),"target":target,"domains":[{"domain":"topology","revision":1,"borrowers":session_borrowers(target, self.foreign_borrower)}],"resources":[{"provider":"display","resource_id":"output","incarnation":"native","revision":self.staged_output.as_ref().unwrap()["resource"]["revision"],"execution_domain":"seat","mutation_domains":["topology"]}]});
                let response = self.broker.borrow_mut().dispatch(
                    "display",
                    Command::Request {
                        service: SERVICE.into(),
                        payload,
                        timeout_ms: 5000,
                        operation_id: format!("stage-refresh-request-{}", self.refresh_round),
                        parent: Some(*owner_request),
                    },
                );
                let Response::Accepted { id } = response else {
                    panic!("stage observation rejected: {response:?}");
                };
                self.stage_refresh_request = Some(id);
            }
        }
        if let Some((verify_id, owner_id, result)) = &self.finalize_verify_request {
            let response = self.broker.borrow_mut().dispatch(
                "display",
                Command::TakeResult {
                    request: *verify_id,
                },
            );
            match response {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert_eq!(payload["status"], "execute", "{payload}");
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::ReleaseResult {
                                request: *verify_id
                            }
                        ),
                        Response::Ok
                    ));
                    let owner_payload = if self.finalize_pending {
                        self.pending_finalize_result = Some(result.clone());
                        json!({"status":"pending","receipt_id":"finalize-receipt"})
                    } else {
                        result.clone()
                    };
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: *owner_id,
                                payload: owner_payload
                            }
                        ),
                        Response::Ok
                    ));
                    self.finalize_verify_request = None;
                }
                other => panic!("finalize verify failed: {other:?}"),
            }
        }
        if let Some((revision, settings)) = &self.registry_settings {
            let request = self
                .broker
                .borrow_mut()
                .dispatch("registry", Command::Poll {});
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = request
            {
                assert_eq!(caller, COORDINATOR);
                assert_eq!(payload["operation"], "settings_get");
                assert_eq!(payload["provider"], "display");
                assert_eq!(payload["scope"], json!({"kind":"host"}));
                let effective: serde_json::Map<String, Value> = settings
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| {
                        (key.clone(), json!({"value":value,"source":{"kind":"host"}}))
                    })
                    .collect();
                assert!(matches!(self.broker.borrow_mut().dispatch("registry", Command::Complete { request: id, payload: json!({"ok":true,"status":"ok","revision":revision,"effective":effective}) }), Response::Ok));
            }
        }
        for principal in ["amaterasu.input", "launch"] {
            let request = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::Poll {});
            if let Response::Request { id, payload, .. } = request {
                let response = match payload["action"].as_str() {
                    Some("binding_status") => {
                        json!({"ok":true,"status":"bound","token":payload["token"],"target":payload["target"],"geometry_revision":1,"lease":"lease-1"})
                    }
                    Some("revoke_binding") => {
                        json!({"status":"retired","token":payload["token"],"target":payload["target"]})
                    }
                    Some("verify_retirement") => {
                        json!({"ok":true,"retired":true,"owner":payload["owner"],"launch_id":payload["launch_id"],"operation_id":payload["operation_id"]})
                    }
                    _ => panic!("unexpected {principal} request: {payload}"),
                };
                assert!(matches!(
                    self.broker.borrow_mut().dispatch(
                        principal,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
        }
    }

    fn call(&mut self, caller: &str, payload: Value) -> Value {
        self.request_sequence += 1;
        let response = self.broker.borrow_mut().dispatch(
            caller,
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: format!("test-request-{}", self.request_sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request was not admitted by broker: {response:?}");
        };
        for _ in 0..32 {
            self.tick();
            let response = self
                .broker
                .borrow_mut()
                .dispatch(caller, Command::TakeResult { request: id });
            match response {
                Response::Pending => continue,
                Response::Result { payload } => {
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch(caller, Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
                response => panic!("unexpected lifecycle result: {response:?}"),
            }
        }
        panic!("lifecycle failed to reply within bounded test pump");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.coordinator.take();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn check_settlement_history(harness: &mut Harness, tuples: &[Value]) {
    for original in tuples {
        let mut query = original.clone();
        query["operation"] = json!("verify_control_settlement");
        let result = harness.call("display", query.clone());
        assert_eq!(
            result["status"], "valid",
            "historical tuple {}: {result}",
            original["request"]["proposal"]["arguments"]["operation"]
        );
        assert_ne!(harness.call("settings", query.clone())["status"], "valid");
        query["request"]["operation_id"] = json!("forged-operation");
        assert_eq!(harness.call("display", query)["status"], "rejected");
    }
}

fn check_closed_effects_rejected(harness: &mut Harness, tuples: &[Value]) {
    let effects_before = harness
        .events
        .iter()
        .filter(|(provider, call)| provider == "display" && call["method"] == "control")
        .count();
    for tuple in tuples {
        for operation in [
            "describe_resource_control",
            "verify_resource_control",
            "validate_resource_control",
        ] {
            let result = harness.call(
                "display",
                json!({"operation": operation, "request": tuple["request"]}),
            );
            assert_eq!(
                result["status"], "rejected",
                "closed effect {operation}: {result}"
            );
        }
    }
    assert_eq!(
        harness
            .events
            .iter()
            .filter(|(provider, call)| { provider == "display" && call["method"] == "control" })
            .count(),
        effects_before
    );
}

fn composed_restore_and_close(last_cycle: usize) {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    assert_eq!(harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}))["status"],"pending");
    let mut ready = Value::Null;
    for _ in 0..160 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    assert_eq!(harness.call("transport",json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}))["status"],"succeeded");
    let mut previous_released = None;
    let mut historical_tuples = Vec::new();
    for cycle in 0..=last_cycle {
        let operation_id = format!("stream.{cycle}");
        let operation = if cycle == 0 {
            "prepare_stream"
        } else {
            "resume_stream"
        };
        let intent = if cycle == 0 { "initial" } else { "resume" };
        let started = harness.call("transport",json!({"operation":operation,"operation_id":operation_id,"target":session,"intent":intent,"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
        assert_eq!(started["status"], "pending", "{started}");
        let mut prepared = Value::Null;
        for _ in 0..300 {
            prepared = harness.call(
                "transport",
                json!({"operation":"lookup","operation_id":operation_id}),
            );
            if prepared["status"] != "pending" {
                break;
            }
        }
        assert_eq!(prepared["status"], "succeeded", "cycle {cycle}: {prepared}");
        if let Some(previous) = previous_released.take() {
            let journal: Value =
                serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap())
                    .unwrap();
            assert_eq!(
                journal["composition"]["staged_outputs"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["before"],
                previous
            );
        }
        let stream = prepared["target"].clone();
        let pause_id = format!("pause.{cycle}");
        let pause = harness.call(
            "transport",
            json!({"operation":"pause_stream","operation_id":pause_id,"target":stream}),
        );
        assert_eq!(pause["status"], "pending", "{pause}");
        let retired = harness.call("capture",json!({"operation":"retire_stream","operation_id":format!("retire.{cycle}"),"target":stream,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let mut released = Value::Null;
        for _ in 0..300 {
            released = harness.call(
                "transport",
                json!({"operation":"lookup","operation_id":pause_id}),
            );
            if released["status"] != "pending" {
                break;
            }
        }
        assert_eq!(released["status"], "succeeded", "cycle {cycle}: {released}");
        eprintln!(
            "test journal bytes after cycle {cycle}: {}",
            std::fs::metadata(harness.root.join("journal.json"))
                .unwrap()
                .len()
        );
        let journal: Value =
            serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap())
                .unwrap();
        if cycle == 0 {
            for name in ["configure", "restore"] {
                let mut records = journal["control_settlements"]["records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|r| {
                        r["settlement"]["request"]["target"] == stream
                            && r["settlement"]["request"]["proposal"]["arguments"]["operation"]
                                == name
                    });
                let record = records.next().expect("exact acknowledged control");
                assert!(records.next().is_none());
                historical_tuples.push(record["settlement"].clone());
            }
            check_settlement_history(&mut harness, &historical_tuples);
        }
        let proof = journal["composed_releases"]["proofs"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        previous_released = Some(proof["released_output"].clone());
        assert_eq!(proof["released_output"]["data"]["configured"], false);
        assert!(harness.native_live);
    }
    let events = &harness.events;
    let dependent: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, (provider, call))| {
            provider == "settings"
                && call["method"] == "lifecycle_callback"
                && call["arguments"]["phase"] == "stream.release"
        })
        .map(|(i, _)| i)
        .collect();
    let owner: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, (provider, call))| {
            provider == "display"
                && call["method"] == "lifecycle_callback"
                && call["arguments"]["phase"] == "stream.release"
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(dependent.len(), last_cycle + 1);
    assert_eq!(owner.len(), last_cycle + 1);
    assert!(
        dependent
            .iter()
            .zip(owner.iter())
            .all(|(dependent, owner)| dependent < owner)
    );
    let end = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":"session.end","target":session}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    check_settlement_history(&mut harness, &historical_tuples);
    let app = harness.call(
        "transport",
        json!({"operation":"application_retired","operation_id":"app.retired","target":session}),
    );
    assert_eq!(app["status"], "pending", "{app}");
    let mut ended = Value::Null;
    for _ in 0..300 {
        ended = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.end"}),
        );
        if ended["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(!harness.native_live);
    assert!(harness.staged_output.is_none());
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        journal["closed_streams"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    check_settlement_history(&mut harness, &historical_tuples);
    check_closed_effects_rejected(&mut harness, &historical_tuples);
    let calls = harness.events.len();
    harness.coordinator.take();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    check_settlement_history(&mut harness, &historical_tuples);
    check_closed_effects_rejected(&mut harness, &historical_tuples);
    assert_eq!(harness.events.len(), calls);
    assert!(!harness.native_live);
}

#[test]
fn composed_single_cycle_closes_session() {
    composed_restore_and_close(0);
}
