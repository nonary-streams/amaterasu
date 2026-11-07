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
    fresh_domain: bool,
    settings: LifecycleSettings,
    root: PathBuf,
    request_sequence: u64,
    provider_calls: Vec<Value>,
    provider: bool,
    processing: bool,
    stream_processing_enabled: bool,
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
    external: bool,
    retirement_result: Option<Value>,
}

impl Harness {
    fn with_provider(provider: bool) -> Self {
        Self::with_registry(provider, None)
    }

    fn with_registry(provider: bool, registry_settings: Option<(u64, Value)>) -> Self {
        Self::with_processing(provider, registry_settings, false)
    }

    fn with_processing(
        provider: bool,
        registry_settings: Option<(u64, Value)>,
        processing: bool,
    ) -> Self {
        Self::with_processing_mode(provider, registry_settings, processing, false)
    }

    fn with_processing_mode(
        provider: bool,
        registry_settings: Option<(u64, Value)>,
        processing: bool,
        external: bool,
    ) -> Self {
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
                "verify_control_settlement",
                "lookup_stream_closure"
            ]);
            configuration["settings"][COORDINATOR]["operation_grants"]["settings"] = json!([
                "verify_resource_control",
                "verify_control_settlement",
                "lookup_stream_closure"
            ]);
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
            if processing {
                configuration["settings"][COORDINATOR]["participants"][0]["capabilities"] =
                    json!(["source_video_mode/1"]);
                configuration["settings"][COORDINATOR]["participants"][1]["settings"] =
                    json!({"enabled":true});
                configuration["principals"][COORDINATOR]["call"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("example.processing/1"));
                configuration["principals"]["settings"]["provide"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("example.processing/1"));
                configuration["settings"][COORDINATOR]["processing_policy"] = json!({"provider":"settings","service":"example.processing/1","processor_service":"example.processor/1","source_mode_providers":["display"]});
                methods.push(json!({"service":"example.processing/1","method":"processing_intent","interface_major":1,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"}));
            }
            if external {
                configuration["settings"][COORDINATOR]["capture_binding_mode"] = json!("external");
                configuration["settings"][COORDINATOR]["consumer_requirements"] =
                    json!({"execution_domain":"seat","capture_providers":[],"selector_schemas":[]});
                configuration["settings"][COORDINATOR]["control_policies"] = json!([]);
                configuration["settings"][COORDINATOR]["finalize_policies"] = json!([]);
                for participant in configuration["settings"][COORDINATOR]["participants"]
                    .as_array_mut()
                    .unwrap()
                {
                    participant["contract_version"] = json!(1);
                    participant["binding_validate_method"] = Value::Null;
                    participant["capabilities"] =
                        json!([amaterasu_module_sdk::extension_api::STREAM_JOIN_CAPABILITY]);
                    for method in ["plan_method", "callback_method", "status_method"] {
                        participant[method]["interface_major"] = json!(1);
                        participant[method]["service"] = json!(
                            participant[method]["service"]
                                .as_str()
                                .unwrap()
                                .replace("/2", "/1")
                        );
                    }
                    for method in participant["cleanup_methods"].as_array_mut().unwrap() {
                        method["interface_major"] = json!(1);
                        method["service"] =
                            json!(method["service"].as_str().unwrap().replace("/2", "/1"));
                    }
                }
                for method in &mut methods {
                    method["interface_major"] = json!(1);
                    method["service"] =
                        json!(method["service"].as_str().unwrap().replace("/2", "/1"));
                }
                for principal in [COORDINATOR, "display", "settings"] {
                    for grant in ["provide", "call"] {
                        if let Some(services) =
                            configuration["principals"][principal][grant].as_array_mut()
                        {
                            for service in services {
                                if let Some(value) = service.as_str() {
                                    *service = json!(
                                        value
                                            .replace("example.display/2", "example.display/1")
                                            .replace("example.settings/2", "example.settings/1")
                                    );
                                }
                            }
                        }
                    }
                }
            }
            configuration["api_methods"]=json!(methods.iter().map(|method|json!({"provider":if method["service"].as_str().is_some_and(|service| service.starts_with("example.display/")) {"display"} else {"settings"},"callers":[COORDINATOR],"method":method})).collect::<Vec<_>>());
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
        if external {
            settings
                .validate()
                .expect("external fixture must use production-valid startup policy");
        }
        let configuration = serde_json::from_value(configuration).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
        for method in methods {
            let response = broker.borrow_mut().dispatch(
                if method["service"]
                    .as_str()
                    .is_some_and(|service| service.starts_with("example.display/"))
                {
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
            fresh_domain: false,
            settings,
            root,
            request_sequence: 0,
            provider_calls: Vec::new(),
            provider,
            processing,
            stream_processing_enabled: true,
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
            external,
            retirement_result: None,
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
                if args["intent"] == "join" {
                    let request = serde_json::from_value(args.clone()).unwrap();
                    let response = if self.provider_invalid && payload["method"] == "resource_plan"
                    {
                        amaterasu_module_sdk::extension_api::ProviderResponse::Rejected {
                            reason: "join unavailable".into(),
                        }
                    } else if payload["method"] == "resource_plan" {
                        amaterasu_module_sdk::extension_api::ProviderResponse::Planned {
                            plan: amaterasu_module_sdk::extension_api::stream_join_plan(
                                &request, "display",
                            )
                            .unwrap(),
                        }
                    } else {
                        amaterasu_module_sdk::extension_api::stream_join_callback(
                            &request, "display",
                        )
                        .unwrap()
                    };
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: serde_json::to_value(response).unwrap()
                            }
                        ),
                        Response::Ok
                    ));
                } else if self.external {
                    let response =
                        external_policy_response(&payload, "display", self.cleanup_deferred);
                    if payload["method"] == "lifecycle_callback"
                        && args["phase"] == "stream.prepare"
                    {
                        self.native_live = true;
                    }
                    if payload["method"] == "lifecycle_callback"
                        && args["phase"] == "stream.release"
                        && !self.cleanup_deferred
                    {
                        self.native_live = false;
                    }
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: response
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "resource_plan" {
                    let stream = args["phase"] == "stream.prepare"
                        || args["phase"] == "stream.release"
                        || args["phase"] == "session.release";
                    let release =
                        args["phase"] == "stream.release" || args["phase"] == "session.release";
                    let finalize = args["operation"] == "resource_finalize_plan";
                    let resource = json!({"provider":"display","resource_id":"output","incarnation":if stream {json!("native")} else {Value::Null},"expected_revision":if stream {self.staged_output.as_ref().map(|output|output["resource"]["revision"].clone()).unwrap_or(json!(1))} else {Value::Null},"execution_domain":"seat","mutation_domains":["topology"],"access":if finalize || release {"exclusive"} else {"read"}});
                    let constraint = if finalize {
                        json!({"capture_provider":"capture","selector_schema":"example/1","resource_provider":"display","resource_id":"output","incarnation":"native","execution_domain":"seat","mode":args.get("source_video_mode").unwrap_or(&args["requested_video"]),"plan_revision":1,"geometry_revision":1})
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
                    let mut plan = json!({"plan_id":if finalize {"finalize-plan"} else {"session-plan"},"revision":1,"resources":[resource],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower),"access":if finalize || release {"exclusive"} else {"read"}}],"required_capabilities":[],"constraints":{},"obligations":obligations,"affected_consumers":[],"consumer_change":"live","candidate_constraint":constraint,"allowed_degradation":[]});
                    if args["phase"] == "session.prepare" && self.fresh_domain {
                        plan["resources"][0]["resource_id"] = json!("output-next");
                        plan["resources"][0]["mutation_domains"] = json!(["topology-next"]);
                        plan["mutation_domains"][0]["domain"] = json!("topology-next");
                        plan["obligations"][0]["mutation_domain"] = json!("topology-next");
                        plan["obligations"][0]["resource_id"] = json!("output-next");
                    }
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
                    let mut initial = json!({"output_id":"screen-output","capability":"display/output","schema":schema,"producer":"display","producer_service":"example.display/2","origin":origin,"resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]},"domain_observations":[{"domain":"topology","revision":1,"borrowers":session_borrowers(&args["target"], self.foreign_borrower),"complete":true}],"data":if self.provider_invalid {json!("invalid")} else {json!({"ready":true,"configured":false,"snapshot_id":null})}});
                    if args["phase"] == "session.prepare" && self.fresh_domain {
                        initial["output_id"] = json!("screen-output-next");
                        initial["resource"]["resource_id"] = json!("output-next");
                        initial["resource"]["incarnation"] = json!("native-next");
                        initial["resource"]["mutation_domain"] = json!("topology-next");
                        initial["resource"]["mutation_domains"] = json!(["topology-next"]);
                        initial["domain_observations"][0]["domain"] = json!("topology-next");
                    }
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
            let response = if args["intent"] == "join" {
                let request = serde_json::from_value(args.clone()).unwrap();
                let response = if payload["method"] == "resource_plan" {
                    amaterasu_module_sdk::extension_api::ProviderResponse::Planned {
                        plan: amaterasu_module_sdk::extension_api::stream_join_plan(
                            &request, "settings",
                        )
                        .unwrap(),
                    }
                } else {
                    amaterasu_module_sdk::extension_api::stream_join_callback(&request, "settings")
                        .unwrap()
                };
                serde_json::to_value(response).unwrap()
            } else if self.external {
                external_policy_response(&payload, "settings", false)
            } else if payload["method"] == "processing_intent" {
                assert!(self.processing);
                let enabled = args["target"]["kind"] != "stream" || self.stream_processing_enabled;
                json!({"status":"succeeded","target":args["target"],"revision":args["revision"],"enabled":enabled,
                    "source_hdr_required":if enabled { Some(false) } else { None },
                    "output_hdr_required":if enabled { Some(true) } else { None },
                    "processor_settings":args["settings"],"processor_service":"example.processor/1"})
            } else if payload["method"] == "resource_plan" {
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
                        self.events.push(("launch".into(), payload.clone()));
                        self.retirement_result.clone().unwrap_or_else(|| json!({"ok":true,"retired":true,"owner":payload["owner"],"launch_id":payload["launch_id"],"operation_id":payload["operation_id"]}))
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

fn stream_closure_query(tuple: &Value, strong: bool) -> Value {
    let request: amaterasu_module_sdk::extension_api::ResourceControlRequest =
        serde_json::from_value(tuple["request"].clone()).unwrap();
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&request).unwrap())
    );
    let mut query = json!({"operation":"lookup_stream_closure", "target":request.target, "resource_generation":1, "resource_id":request.proposal.resource.resource_id, "incarnation":request.proposal.resource.incarnation, "operation_id":request.operation_id, "request_digest":digest});
    if strong {
        query["require_retirement_safe"] = json!(true);
    }
    query
}

fn check_stream_closure(harness: &mut Harness, tuple: &Value) {
    let strong = stream_closure_query(tuple, true);
    let result = harness.call("display", strong.clone());
    assert_eq!(result["status"], "closed", "strong closure: {result}");
    assert_eq!(result["retirement_safe"], true);
    for field in [
        "target",
        "resource_generation",
        "resource_id",
        "incarnation",
        "operation_id",
        "request_digest",
    ] {
        assert_eq!(
            result[field], strong[field],
            "mismatched closure field {field}"
        );
    }
    let legacy = harness.call("display", stream_closure_query(tuple, false));
    assert_eq!(legacy["status"], "closed", "legacy closure: {legacy}");
    assert!(legacy.get("retirement_safe").is_none());
    let mut forged = strong.clone();
    forged["operation_id"] = json!("forged-operation");
    assert_eq!(harness.call("display", forged)["status"], "rejected");
    assert_eq!(harness.call("settings", strong)["status"], "rejected");
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
        let active_journal: Value =
            serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap())
                .unwrap();
        let configure = active_journal["control_settlements"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["settlement"]["request"]["target"] == stream)
            .expect("active configure")["settlement"]
            .clone();
        assert_eq!(
            harness.call("display", stream_closure_query(&configure, true))["status"],
            "rejected",
            "an active stream is not closed"
        );
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
            let legacy = harness.call(
                "display",
                stream_closure_query(&historical_tuples[0], false),
            );
            assert_eq!(
                legacy["status"], "closed",
                "live release historical lookup: {legacy}"
            );
            assert!(legacy.get("retirement_safe").is_none());
            assert_eq!(
                harness.call("display", stream_closure_query(&historical_tuples[0], true))["status"],
                "rejected",
                "uncheckpointed stream must not attest retirement safety"
            );
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
    let end_request =
        json!({"operation":"end_session","operation_id":"session.end","target":session});
    let end = harness.call("transport", end_request.clone());
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
    assert!(journal["sessions"].as_object().unwrap().is_empty());
    check_settlement_history(&mut harness, &historical_tuples);
    check_closed_effects_rejected(&mut harness, &historical_tuples);
    let lookup = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_eq!(lookup, ended);
    let retired: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert!(
        retired["sessions"].as_object().unwrap().is_empty(),
        "completed session must be retired"
    );
    check_stream_closure(&mut harness, &historical_tuples[0]);
    let retry = harness.call("transport", end_request.clone());
    assert_eq!(
        retry, ended,
        "exact End retry must retain the completed receipt"
    );
    let lookup = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_eq!(lookup, ended, "lookup must retain the completed receipt");
    let forged = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":"session.end","target":null}),
    );
    assert_eq!(
        forged["status"], "rejected",
        "changed request must be fenced: {forged}"
    );
    let foreign = harness.call(
        "capture",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_ne!(
        foreign["status"], "succeeded",
        "foreign caller must not receive the receipt: {foreign}"
    );
    let calls = harness.events.len();
    harness.coordinator.take();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_eq!(recovered, ended, "lookup must survive reopening");
    assert_eq!(
        harness.call("transport", end_request),
        ended,
        "exact End retry must survive reopening"
    );
    let forged = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":"session.end","target":null}),
    );
    assert_eq!(
        forged["status"], "rejected",
        "changed request after reopening: {forged}"
    );
    let foreign = harness.call(
        "capture",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_ne!(
        foreign["status"], "succeeded",
        "foreign caller after reopening: {foreign}"
    );
    check_settlement_history(&mut harness, &historical_tuples);
    check_closed_effects_rejected(&mut harness, &historical_tuples);
    check_stream_closure(&mut harness, &historical_tuples[0]);
    assert_eq!(harness.events.len(), calls);
    assert!(!harness.native_live);
    harness.fresh_domain = true;
    let next = harness.call("transport", json!({"operation":"prepare","operation_id":"next.prepare","application_ref":{"kind":"application","app_id":"next"}}));
    assert_eq!(
        next["status"], "pending",
        "new session after retirement: {next}"
    );
    let mut next_ready = Value::Null;
    for _ in 0..160 {
        next_ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"next.prepare"}),
        );
        if next_ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(
        next_ready["status"], "succeeded",
        "new session after retirement: {next_ready}"
    );
    assert_ne!(next_ready["target"]["session_id"], session["session_id"]);
}

#[test]
fn composed_single_cycle_closes_session() {
    composed_restore_and_close(0);
}

#[test]
fn processing_preflight_sets_source_range_before_session_and_stream_effects() {
    let mut harness = Harness::with_processing(true, None, true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    let requested = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":true});
    let started = harness.call("transport", json!({"operation":"prepare","operation_id":"processing-session", "application_ref":{"kind":"application","app_id":"app"},"requested_video":requested}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut prepared = Value::Null;
    for _ in 0..180 {
        prepared = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"processing-session"}),
        );
        if prepared["status"] != "pending" {
            break;
        }
    }
    assert_eq!(prepared["status"], "succeeded", "{prepared}");
    let session = prepared["target"].clone();
    let source_plan = harness
        .provider_calls
        .iter()
        .find(|call| {
            call["method"] == "resource_plan" && call["arguments"]["phase"] == "session.prepare"
        })
        .expect("display plan");
    assert_eq!(source_plan["arguments"]["requested_video"]["hdr"], true);
    assert_eq!(source_plan["arguments"]["source_video_mode"]["hdr"], false);
    let unrelated_plan = harness
        .dependent_calls
        .iter()
        .find(|call| {
            call["method"] == "resource_plan" && call["arguments"]["phase"] == "session.prepare"
        })
        .expect("dependent plan");
    assert_eq!(unrelated_plan["arguments"]["requested_video"]["hdr"], true);
    assert!(
        unrelated_plan["arguments"]
            .get("source_video_mode")
            .is_none()
    );
    assert_eq!(harness.call("transport",json!({"operation":"bind_application","operation_id":"processing-bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}))["status"],"succeeded");
    let started = harness.call("transport", json!({"operation":"prepare_stream","operation_id":"processing-stream","target":session,"intent":"initial","requested_video":requested}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut stream = Value::Null;
    for _ in 0..400 {
        stream = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"processing-stream"}),
        );
        if stream["status"] != "pending" {
            break;
        }
    }
    assert_eq!(stream["status"], "succeeded", "{stream}");
    assert_eq!(stream["candidate"]["mode"]["hdr"], false);
    let finalize = harness
        .provider_calls
        .iter()
        .find(|call| {
            call["method"] == "resource_plan"
                && call["arguments"]["operation"] == "resource_finalize_plan"
        })
        .expect("finalize plan");
    assert_eq!(finalize["arguments"]["requested_video"]["hdr"], true);
    assert_eq!(finalize["arguments"]["source_video_mode"]["hdr"], false);
}

#[test]
fn processing_preflight_rejects_incompatible_output_before_provider_effects() {
    let mut harness = Harness::with_processing(true, None, true);
    let started = harness.call("transport", json!({"operation":"prepare","operation_id":"incompatible-session","application_ref":{"kind":"desktop"},"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..160 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"incompatible-session"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(
        result["reason"], "processing_output_range_mismatch",
        "{result}"
    );
    assert!(
        harness.provider_calls.is_empty(),
        "display effects occurred: {:?}",
        harness.provider_calls
    );
}

#[test]
fn processing_preflight_rejects_stream_policy_incompatible_with_session_source() {
    let mut harness = Harness::with_processing(true, None, true);
    let requested = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":true});
    harness.call("transport", json!({"operation":"prepare","operation_id":"policy-session","application_ref":{"kind":"application","app_id":"app"},"requested_video":requested}));
    let mut prepared = Value::Null;
    for _ in 0..180 {
        prepared = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"policy-session"}),
        );
        if prepared["status"] != "pending" {
            break;
        }
    }
    assert_eq!(prepared["status"], "succeeded", "{prepared}");
    let session = prepared["target"].clone();
    assert_eq!(harness.call("transport",json!({"operation":"bind_application","operation_id":"policy-bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}))["status"],"succeeded");
    let calls_before = harness.provider_calls.len();
    harness.stream_processing_enabled = false;
    harness.call("transport", json!({"operation":"prepare_stream","operation_id":"policy-stream","target":session,"intent":"initial","requested_video":requested}));
    let mut result = Value::Null;
    for _ in 0..160 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"policy-stream"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(
        result["reason"], "processing_source_range_changed",
        "{result}"
    );
    assert!(
        !harness.provider_calls[calls_before..]
            .iter()
            .any(|call| call["arguments"]["phase"] == "stream.prepare"),
        "stream effect occurred: {:?}",
        &harness.provider_calls[calls_before..]
    );
}

fn external_policy_response(payload: &Value, provider: &str, deferred: bool) -> Value {
    let args = &payload["arguments"];
    if deferred
        && provider == "display"
        && matches!(
            args["phase"].as_str(),
            Some("stream.prepare" | "stream.release")
        )
    {
        let obligation = json!({"obligation_id":"external-policy-restore","provider":provider,
            "resource_id":"host-policy","mutation_domain":"host-policy","depends_on":[],
            "cleanup_method":{"service":"example.display/1","method":"lifecycle_callback","interface_major":1,"interface_minor":0,"schema_revision":1},"payload":{}});
        if payload["method"] == "resource_plan" {
            return json!({"status":"planned","plan":{"plan_id":"external-deferred-policy","revision":1,
                "resources":[{"provider":provider,"resource_id":"host-policy","incarnation":null,"expected_revision":null,
                    "execution_domain":"seat","mutation_domains":["host-policy"],"access":"exclusive"}],
                "mutation_domains":[{"domain":"host-policy","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[obligation],
                "affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}});
        }
        if args["phase"] == "stream.prepare" {
            return json!({"status":"prepared","candidate":null,"obligations":[obligation]});
        }
        return json!({"status":"cleanup","outcomes":[{"obligation_id":"external-policy-restore","state":"deferred_restoration","reason":null}]});
    }
    if payload["method"] == "resource_plan" {
        json!({"status":"planned","plan":{"plan_id":"external-policy","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]}})
    } else if matches!(
        args["phase"].as_str(),
        Some("session.prepare" | "stream.prepare")
    ) {
        json!({"status":"prepared","candidate":null,"obligations":[]})
    } else {
        json!({"status":"cleanup","outcomes":[]})
    }
}

fn shared_source_harness() -> (Harness, Value, Value) {
    prepare_shared_source(Harness::with_provider(true))
}

fn prepare_shared_source(mut harness: Harness) -> (Harness, Value, Value) {
    if !harness.external {
        harness.simulate_stage_refresh = true;
        harness.verify_finalize = true;
    }
    for participant in &mut harness.settings.participants {
        participant
            .capabilities
            .insert(amaterasu_module_sdk::extension_api::STREAM_JOIN_CAPABILITY.into());
    }
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    let started=harness.call("transport",json!({"operation":"prepare","operation_id":"shared.session","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending");
    let session = wait_shared_operation(&mut harness, "shared.session")["target"].clone();
    assert_eq!(harness.call("transport",json!({"operation":"bind_application","operation_id":"shared.app","target":session,"launch_id":42,"application_operation_id":"launch-42"}))["status"],"succeeded");
    let started=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"shared.source","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(started["status"], "pending");
    let source = wait_shared_operation(&mut harness, "shared.source");
    assert_eq!(source["status"], "succeeded", "{source}");
    (harness, session, source)
}
fn wait_shared_operation(harness: &mut Harness, operation: &str) -> Value {
    let mut response = Value::Null;
    for _ in 0..400 {
        response = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":operation}),
        );
        if response["status"] != "pending" {
            return response;
        }
    }
    panic!("shared operation did not complete: {response}");
}
fn retire_shared_client(harness: &mut Harness, target: &Value, operation: &str) -> Value {
    let pause = harness.call(
        "transport",
        json!({"operation":"pause_stream","operation_id":operation,"target":target}),
    );
    assert_eq!(pause["status"], "pending", "{pause}");
    let retirement=harness.call("capture",json!({"operation":"retire_stream","operation_id":format!("retire-{operation}"),"target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert!(
        matches!(retirement["status"].as_str(), Some("pending" | "succeeded")),
        "{retirement}"
    );
    wait_shared_operation(harness, operation)
}
fn end_shared_application(harness: &mut Harness, session: &Value, prefix: &str) {
    let operation = format!("{prefix}-end");
    let end = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":operation,"target":session}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    let app=harness.call("transport",json!({"operation":"application_retired","operation_id":format!("{prefix}-app-retired"),"target":session}));
    assert!(
        matches!(app["status"].as_str(), Some("pending" | "succeeded")),
        "{app}"
    );
    let ended = wait_shared_operation(harness, &operation);
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(!harness.native_live);
}

#[test]
fn shared_source_join_keeps_geometry_and_releases_once_in_both_disconnect_orders() {
    for source_first in [false, true] {
        let (mut harness, session, source) = shared_source_harness();
        let initial_effects = harness
            .events
            .iter()
            .filter(|(_, payload)| payload["method"] == "control")
            .count();
        let started=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"shared.join","target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"],"client_context":{"client_ref":"second-client","properties":{}}}));
        assert_eq!(started["status"], "pending", "{started}");
        let joined = wait_shared_operation(&mut harness, "shared.join");
        assert_eq!(joined["status"], "succeeded", "{joined}");
        assert_eq!(joined["shared_source"], source["target"]);
        assert_eq!(
            joined["candidate"]["resource"],
            source["candidate"]["resource"]
        );
        assert_eq!(
            joined["candidate"]["geometry_revision"],
            source["candidate"]["geometry_revision"]
        );
        assert_eq!(joined["candidate"]["mode"], source["candidate"]["mode"]);
        let journal: Value =
            serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap())
                .unwrap();
        let session_id = session["session_id"].as_str().unwrap();
        let source_id = source["target"]["stream_id"].as_str().unwrap();
        let subscriber_id = joined["target"]["stream_id"].as_str().unwrap();
        let source_record = &journal["sessions"][session_id]["streams"][source_id];
        let subscriber_record = &journal["sessions"][session_id]["streams"][subscriber_id];
        assert_eq!(
            subscriber_record["provider_settings"],
            source_record["provider_settings"]
        );
        assert_eq!(
            subscriber_record["snapshot_scopes"]["paired_client"],
            "second-client"
        );
        assert_eq!(
            source_record["snapshot_scopes"]["paired_client"],
            Value::Null
        );
        assert_eq!(
            harness
                .events
                .iter()
                .filter(|(_, payload)| payload["method"] == "control")
                .count(),
            initial_effects
        );
        let (first, last) = if source_first {
            (&source["target"], &joined["target"])
        } else {
            (&joined["target"], &source["target"])
        };
        let retired = retire_shared_client(&mut harness, first, "shared.first-close");
        assert_eq!(retired["status"], "succeeded", "{retired}");
        assert_eq!(retired["consumer_retired"], true);
        assert_eq!(retired["shared_retained"], true);
        assert_eq!(retired["resources_released"], false);
        assert_eq!(retired["shared_source"], source["target"]);
        assert!(harness.native_live, "sibling keeps original producer lease");
        let retired = retire_shared_client(&mut harness, last, "shared.last-close");
        assert_eq!(retired["status"], "succeeded", "{retired}");
        for _ in 0..200 {
            harness.tick();
        }
        let snapshot = harness.call(
            "transport",
            json!({"operation":"snapshot","target":session}),
        );
        assert_eq!(snapshot["sessions"][0]["phase"], "paused", "{snapshot}");
        assert!(
            snapshot["sessions"][0]["streams"]
                .as_array()
                .unwrap()
                .iter()
                .all(|stream| stream["phase"] == "retired")
        );
        assert!(
            harness.native_live,
            "application session retains producer for resume"
        );
        let restore_count = harness
            .events
            .iter()
            .filter(|(_, payload)| {
                payload["method"] == "control"
                    && payload["arguments"]["proposal"]["arguments"]["operation"] == "restore"
            })
            .count();
        assert_eq!(restore_count, 1);
        end_shared_application(&mut harness, &session, "complete-shared");
    }
}

#[test]
fn join_capability_and_source_context_are_checked_before_provider_effects() {
    let (mut harness, session, source) = shared_source_harness();
    let forged=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"forged.join","target":session,"intent":"join","join_context":{"source":"forged"},"requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(forged["reason"], "join_context_is_coordinator_owned");
    harness.settings.participants[0].capabilities.clear();
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    let calls = harness.provider_calls.len();
    let rejected=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"unsupported.join","target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"]}));
    assert_eq!(rejected["reason"], "provider_join_unsupported");
    assert_eq!(harness.provider_calls.len(), calls);
    let snapshot = harness.call(
        "transport",
        json!({"operation":"snapshot","target":session}),
    );
    assert_eq!(
        snapshot["sessions"][0]["streams"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        snapshot["sessions"][0]["streams"][0]["stream_id"],
        source["target"]["stream_id"]
    );
}

#[test]
fn rejected_join_preserves_the_source_and_does_not_pin_a_phantom_subscriber() {
    let (mut harness, session, source) = shared_source_harness();
    harness.provider_invalid = true;
    assert_eq!(harness.call("transport",json!({"operation":"prepare_stream","operation_id":"failed.join","target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"]}))["status"],"pending");
    let rejected = wait_shared_operation(&mut harness, "failed.join");
    assert_eq!(rejected["status"], "failed", "{rejected}");
    assert!(harness.native_live);
    harness.provider_invalid = false;
    let retired = retire_shared_client(&mut harness, &source["target"], "source.after-failed-join");
    assert_eq!(retired["status"], "succeeded", "{retired}");
    assert_eq!(
        retired["resources_released"], true,
        "failed join must not retain source lease"
    );
    let snapshot = harness.call(
        "transport",
        json!({"operation":"snapshot","target":session}),
    );
    assert_eq!(snapshot["sessions"][0]["phase"], "paused");
    end_shared_application(&mut harness, &session, "failed-join-shared");
}

#[test]
fn external_join_retains_host_policy_and_confirms_final_cleanup_in_both_orders() {
    for source_first in [false, true] {
        let harness = Harness::with_processing_mode(true, None, false, true);
        let (mut harness, session, source) = prepare_shared_source(harness);
        assert_eq!(source["capture_binding_mode"], "external");
        assert_eq!(source["candidate"], Value::Null);
        harness
            .settings
            .operation_grants
            .get_mut("capture")
            .unwrap()
            .insert("ack_stream".into());
        harness
            .coordinator
            .as_mut()
            .unwrap()
            .configure_for_test(harness.settings.clone());
        assert_eq!(harness.call("capture",json!({"operation":"ack_stream","operation_id":"external-source-ack","target":source["target"],"consumer":"capture"}))["status"],"succeeded");
        assert_eq!(harness.call("transport",json!({"operation":"start_stream","operation_id":"external-source-start","target":source["target"]}))["status"],"succeeded");
        assert_eq!(harness.call("transport",json!({"operation":"prepare_stream","operation_id":"external.join","target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"],"client_context":{"client_ref":"second-client","properties":{}}}))["status"],"pending");
        let joined = wait_shared_operation(&mut harness, "external.join");
        assert_eq!(joined["status"], "succeeded", "{joined}");
        assert_eq!(joined["candidate"], Value::Null);
        assert_eq!(joined["join_context"]["capture_binding_mode"], "external");
        assert_eq!(joined["join_context"]["binding"], Value::Null);
        assert_eq!(joined["join_context"]["binding_owner"], Value::Null);
        assert_eq!(joined["shared_source"], source["target"]);
        assert_eq!(harness.call("capture",json!({"operation":"ack_stream","operation_id":"external-join-ack","target":joined["target"],"consumer":"capture"}))["status"],"succeeded");
        assert_eq!(harness.call("transport",json!({"operation":"start_stream","operation_id":"external-join-start","target":joined["target"]}))["status"],"succeeded");
        let (first, last) = if source_first {
            (&source["target"], &joined["target"])
        } else {
            (&joined["target"], &source["target"])
        };
        let retired = retire_shared_client(&mut harness, first, "external.first-close");
        assert_eq!(retired["status"], "succeeded", "{retired}");
        assert_eq!(retired["consumer_retired"], true);
        assert_eq!(retired["resources_released"], false);
        assert_eq!(retired["shared_retained"], true);
        assert!(
            harness.native_live,
            "first retirement keeps shared host policy"
        );
        let retired = retire_shared_client(&mut harness, last, "external.last-close");
        assert_eq!(retired["status"], "succeeded", "{retired}");
        assert_eq!(retired["consumer_retired"], true);
        assert_eq!(
            retired["resources_released"], true,
            "final receipt must wait for original source callbacks"
        );
        assert_eq!(retired["shared_retained"], false);
        assert!(
            !harness.native_live,
            "final policy callback completed before proof"
        );
        for provider in ["display", "settings"] {
            assert_eq!(
                harness
                    .events
                    .iter()
                    .filter(|(principal, payload)| principal == provider
                        && payload["method"] == "lifecycle_callback"
                        && payload["arguments"]["phase"] == "stream.release"
                        && payload["arguments"]["intent"] != "join")
                    .count(),
                1
            );
        }
    }
}

fn activate_external_consumer(harness: &mut Harness, target: &Value, prefix: &str) {
    assert_eq!(harness.call("capture",json!({"operation":"ack_stream","operation_id":format!("{prefix}-ack"),"target":target,"consumer":"capture"}))["status"],"succeeded");
    assert_eq!(harness.call("transport",json!({"operation":"start_stream","operation_id":format!("{prefix}-start"),"target":target}))["status"],"succeeded");
}

#[test]
fn borrowed_capture_preserves_all_required_application_and_stream_policy_hooks() {
    let mut harness = Harness::with_processing_mode(true, None, false, true);
    harness
        .settings
        .borrowed_capture_callers
        .insert("transport".into());
    harness
        .settings
        .operation_grants
        .get_mut("capture")
        .unwrap()
        .insert("ack_stream".into());
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    let borrowed = json!({"client_id":"authenticated-borrower","session_id":"retained-monitor","generation":4,
        "reservation_id":"provider-held-reservation","transport_id":"rtsp-operation","transport_epoch":"transport-epoch",
        "output":"exact-target:retained-monitor","mode":{"width":2560,"height":1440,"refresh_millihz":144000,"hdr":true}});
    let prepared = harness.call(
        "transport",
        json!({"operation":"prepare","operation_id":"borrowed.session",
        "application_ref":{"kind":"application","app_id":"app"},"borrowed_capture":borrowed,
        "requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}),
    );
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let prepared = wait_shared_operation(&mut harness, "borrowed.session");
    assert_eq!(prepared["status"], "succeeded", "{prepared}");
    let session = prepared["target"].clone();
    assert_eq!(harness.call("transport",json!({"operation":"bind_application","operation_id":"borrowed.app","target":session,"launch_id":42,"application_operation_id":"launch-42"}))["status"],"succeeded");
    let stream = harness.call(
        "transport",
        json!({"operation":"prepare_stream","operation_id":"borrowed.stream",
        "target":session,"intent":"initial","borrowed_capture":borrowed,
        "requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},
        "consumer_requirements":["capture"]}),
    );
    assert_eq!(stream["status"], "pending", "{stream}");
    let stream = wait_shared_operation(&mut harness, "borrowed.stream");
    assert_eq!(stream["status"], "succeeded", "{stream}");
    assert_eq!(stream["capture_binding_mode"], "external");
    for provider in ["display", "settings"] {
        for phase in ["session.prepare", "stream.prepare"] {
            assert!(
                harness
                    .events
                    .iter()
                    .any(|(principal, payload)| principal == provider
                        && payload["method"] == "lifecycle_callback"
                        && payload["arguments"]["phase"] == phase),
                "missing {provider} {phase}"
            );
        }
    }
    activate_external_consumer(&mut harness, &stream["target"], "borrowed-live");
    let retired = retire_shared_client(&mut harness, &stream["target"], "borrowed-close");
    assert_eq!(retired["status"], "succeeded", "{retired}");
    end_shared_application(&mut harness, &session, "borrowed-application-close");
    for provider in ["display", "settings"] {
        for phase in ["stream.release", "session.release"] {
            assert!(
                harness
                    .events
                    .iter()
                    .any(|(principal, payload)| principal == provider
                        && payload["method"] == "lifecycle_callback"
                        && payload["arguments"]["phase"] == phase),
                "missing {provider} {phase}"
            );
        }
    }
}
fn join_external_consumer(
    harness: &mut Harness,
    session: &Value,
    operation: &str,
    client: &str,
) -> Value {
    let admitted=harness.call("transport",json!({"operation":"prepare_stream","operation_id":operation,"target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"],"client_context":{"client_ref":client,"properties":{}}}));
    assert_eq!(admitted["status"], "pending", "{admitted}");
    let joined = wait_shared_operation(harness, operation);
    assert_eq!(joined["status"], "succeeded", "{joined}");
    activate_external_consumer(harness, &joined["target"], operation);
    joined
}
#[test]
fn replacement_external_client_joins_retained_source_after_original_transport_retires() {
    let harness = Harness::with_processing_mode(true, None, false, true);
    let (mut harness, session, source) = prepare_shared_source(harness);
    harness
        .settings
        .operation_grants
        .get_mut("capture")
        .unwrap()
        .insert("ack_stream".into());
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    activate_external_consumer(&mut harness, &source["target"], "replacement-source");
    let joined = join_external_consumer(&mut harness, &session, "replacement.first", "client-two");
    let retired =
        retire_shared_client(&mut harness, &source["target"], "replacement.source-retire");
    assert_eq!(retired["shared_retained"], true);
    // Source transport is gone, but its source policy remains borrowed. A
    // single active child must not reopen mutation rights on that source.
    harness.settings.action_service = "transport".into();
    harness
        .settings
        .operation_grants
        .get_mut("transport")
        .unwrap()
        .extend([
            "authorize_action".into(),
            "capture_invalidated".into(),
            "resource_invalidated".into(),
        ]);
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    let rejected = harness.call(
        "transport",
        json!({"operation":"prepare_stream",
        "operation_id":"replacement.reconfigure", "target":session,"intent":"reconfigure",
        "requested_video":{"width":640,"height":480,"refresh_millihz":30000,"hdr":false},
        "consumer_requirements":["capture"]}),
    );
    assert_eq!(rejected["reason"], "shared_source_in_use", "{rejected}");
    for operation in ["capture_invalidated", "resource_invalidated"] {
        let rejected = harness.call(
            "transport",
            json!({"operation":operation,
            "operation_id":format!("replacement.{operation}"),"target":source["target"]}),
        );
        assert_eq!(rejected["reason"], "shared_source_in_use", "{rejected}");
    }
    let snapshot = harness.call(
        "transport",
        json!({"operation":"snapshot","target":session}),
    );
    assert_eq!(snapshot["sessions"][0]["phase"], "ready");
    let owner = json!({"caller":"transport", "initiator":"transport"});
    let target = json!({"kind":"session","host_epoch":session["host_epoch"],
        "session_id":session["session_id"],"expected_generation":session["generation"]});
    let plan = json!({"plan_id":"shared-mutation","revision":1,"resources":[],
        "mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],
        "affected_consumers":["capture"],"consumer_change":"live",
        "candidate_constraint":null,"allowed_degradation":[]});
    let rejected = harness.call("transport", json!({"operation":"authorize_action",
        "operation_id":"replacement.action","action_id":"display/change","target":target,
        "source":owner,"plan_digest":format!("{:x}",Sha256::digest(serde_json::to_vec(&plan).unwrap())),
        "plan":plan}));
    assert_eq!(rejected["reason"], "shared_source_in_use", "{rejected}");
    assert!(harness.native_live);
    let replacement =
        join_external_consumer(&mut harness, &session, "replacement.second", "client-three");
    assert_eq!(replacement["shared_source"], source["target"]);
    assert_eq!(
        replacement["join_context"]["source_transition"],
        joined["join_context"]["source_transition"]
    );
    assert_ne!(replacement["target"], joined["target"]);
    let retired = retire_shared_client(&mut harness, &joined["target"], "replacement.first-retire");
    assert_eq!(retired["shared_retained"], true);
    assert!(harness.native_live);
    let retired = retire_shared_client(
        &mut harness,
        &replacement["target"],
        "replacement.last-retire",
    );
    assert_eq!(retired["resources_released"], true, "{retired}");
    assert!(!harness.native_live);
    assert_eq!(
        harness
            .provider_calls
            .iter()
            .filter(|payload| payload["method"] == "lifecycle_callback"
                && payload["arguments"]["phase"] == "stream.prepare"
                && payload["arguments"]["intent"] != "join")
            .count(),
        1
    );
}

#[test]
fn restarted_external_stream_rejects_changed_frozen_provider_contracts() {
    let mut accepted_changes = Vec::new();
    for change in ["callback", "plan", "status", "capabilities", "dependencies"] {
        let harness = Harness::with_processing_mode(true, None, false, true);
        let (mut harness, _, source) = prepare_shared_source(harness);
        harness.coordinator.take();
        let participant = &mut harness.settings.participants[1];
        match change {
            "callback" => participant.callback_method.method = "lifecycle_status".into(),
            "plan" => participant.plan_method.method = "lifecycle_status".into(),
            "status" => participant.status_method.method = "lifecycle_callback".into(),
            "capabilities" => {
                participant.capabilities.insert("changed_contract/1".into());
            }
            "dependencies" => participant.depends_on.clear(),
            _ => unreachable!(),
        }
        harness.start();
        let journal_before = std::fs::read(harness.root.join("journal.json")).unwrap();
        let events_before = harness.events.len();
        let result = harness.call(
            "capture",
            json!({"operation":"retire_stream",
            "operation_id":format!("changed-contract-{change}"),"restart_recovery":true,
            "target":source["target"],
            "retirement_receipt":{"service":"video/1","operation_id":"exact-native-owner",
                "response":{"code":"not_found"}}}),
        );
        if result["status"] != "rejected" {
            accepted_changes.push((change, result));
            continue;
        }
        assert_eq!(
            result["reason"],
            if change == "dependencies" {
                "recovery_provider_graph_changed"
            } else {
                "recovery_provider_contract_changed"
            },
            "{change}: {result}"
        );
        assert_eq!(
            std::fs::read(harness.root.join("journal.json")).unwrap(),
            journal_before,
            "rejection must preserve original recovery evidence for {change}"
        );
        assert_eq!(
            harness.events.len(),
            events_before,
            "contract mismatch must reject before release dispatch for {change}"
        );
        assert!(harness.native_live);
    }
    assert!(
        accepted_changes.is_empty(),
        "changed frozen contracts were admitted: {accepted_changes:?}"
    );
}

#[test]
fn restarted_external_stream_requires_its_exact_frozen_contract_or_legacy_prepare() {
    for scenario in [
        "legacy-retained",
        "legacy-changed",
        "legacy-missing",
        "legacy-unrelated",
        "malformed",
        "unknown-version",
    ] {
        let harness = Harness::with_processing_mode(true, None, false, true);
        let (mut harness, session, source) = prepare_shared_source(harness);
        let joined = if matches!(
            scenario,
            "legacy-retained" | "legacy-changed" | "legacy-unrelated"
        ) {
            harness
                .settings
                .operation_grants
                .get_mut("capture")
                .unwrap()
                .insert("ack_stream".into());
            harness
                .coordinator
                .as_mut()
                .unwrap()
                .configure_for_test(harness.settings.clone());
            activate_external_consumer(&mut harness, &source["target"], "legacy-source");
            Some(join_external_consumer(
                &mut harness,
                &session,
                "legacy-joined",
                "joined-client",
            ))
        } else {
            None
        };
        let target = if matches!(scenario, "legacy-retained" | "legacy-changed") {
            joined.as_ref().unwrap()["target"].clone()
        } else {
            source["target"].clone()
        };
        harness.coordinator.take();
        // This isolated fixture models journals written before contract identity
        // was persisted, or a malformed identity that must never use fallback.
        let journal_path = harness.root.join("journal.json");
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        let snapshots = document["sessions"][target["session_id"].as_str().unwrap()]["streams"]
            [target["stream_id"].as_str().unwrap()]["provider_settings"]
            .as_object_mut()
            .unwrap();
        for snapshot in snapshots.values_mut() {
            if scenario == "malformed" {
                snapshot["contract"] = json!({"version":1,"digest":"invalid-digest"});
            } else if scenario == "unknown-version" {
                snapshot["contract"] = json!({"version":99,"digest":"0".repeat(64)});
            } else {
                snapshot.as_object_mut().unwrap().remove("contract");
            }
        }
        std::fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();
        if scenario == "legacy-changed" {
            harness.settings.participants[0]
                .capabilities
                .insert("changed_contract/1".into());
        }
        harness.start();
        let journal_before = std::fs::read(&journal_path).unwrap();
        let events_before = harness.events.len();
        let operation = format!("contract-proof-{scenario}");
        let result = harness.call(
            "capture",
            json!({"operation":"retire_stream",
            "operation_id":operation,"restart_recovery":true,"target":target,
            "retirement_receipt":{"service":"video/1","operation_id":"exact-native-owner",
                "response":{"code":"not_found"}}}),
        );
        if scenario == "legacy-retained" {
            assert_eq!(result["status"], "pending", "{scenario}: {result}");
            let mut result = result;
            for _ in 0..400 {
                result = harness.call(
                    "capture",
                    json!({"operation":"lookup","operation_id":operation}),
                );
                if result["status"] != "pending" {
                    break;
                }
            }
            assert_eq!(result["status"], "succeeded", "{result}");
            assert_eq!(result["shared_retained"], true);
            assert!(
                harness.native_live,
                "legacy proof cannot release the retained source"
            );
        } else {
            assert_eq!(result["status"], "rejected", "{scenario}: {result}");
            assert_eq!(
                result["reason"],
                if matches!(scenario, "legacy-missing" | "legacy-unrelated") {
                    "recovery_contract_snapshot_missing"
                } else {
                    "recovery_provider_contract_changed"
                },
                "{scenario}: {result}"
            );
            assert_eq!(
                std::fs::read(&journal_path).unwrap(),
                journal_before,
                "rejection preserves original evidence for {scenario}"
            );
            assert_eq!(
                harness.events.len(),
                events_before,
                "rejection precedes provider dispatch for {scenario}"
            );
            assert!(harness.native_live);
        }
    }
}

#[test]
fn restarted_external_stream_releases_with_original_settings_after_current_values_change() {
    let mut harness = Harness::with_processing_mode(true, None, false, true);
    for participant in &mut harness.settings.participants {
        participant.settings = json!({"policy":"original"});
        participant.settings_revision = "original-revision".into();
    }
    let (mut harness, session, source) = prepare_shared_source(harness);
    harness
        .settings
        .operation_grants
        .get_mut("capture")
        .unwrap()
        .insert("ack_stream".into());
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    activate_external_consumer(&mut harness, &source["target"], "settings-source");
    let joined = join_external_consumer(&mut harness, &session, "settings-joined", "joined-client");
    harness.coordinator.take();
    for participant in &mut harness.settings.participants {
        participant.settings = json!({"policy":"replacement"});
        participant.settings_revision = "replacement-revision".into();
    }
    harness.start();
    let retire = |harness: &mut Harness, target: &Value, operation: &str| {
        let result = harness.call(
            "capture",
            json!({"operation":"retire_stream",
            "operation_id":operation,"restart_recovery":true,"target":target,
            "retirement_receipt":{"service":"video/1","operation_id":format!("native-{operation}"),
                "response":{"code":"not_found"}}}),
        );
        assert_eq!(result["status"], "pending", "{result}");
        let mut result = result;
        for _ in 0..400 {
            result = harness.call(
                "capture",
                json!({"operation":"lookup","operation_id":operation}),
            );
            if result["status"] != "pending" {
                break;
            }
        }
        assert_eq!(result["status"], "succeeded", "{result}");
        result
    };
    let joined_result = retire(
        &mut harness,
        &joined["target"],
        "frozen-settings-joined-release",
    );
    assert_eq!(joined_result["shared_retained"], true);
    assert!(harness.native_live);
    let result = retire(
        &mut harness,
        &source["target"],
        "frozen-settings-source-release",
    );
    assert_eq!(result["resources_released"], true);
    assert!(!harness.native_live);
    let released: Vec<_> = harness
        .events
        .iter()
        .filter(|(_, payload)| {
            payload["arguments"]["phase"] == "stream.release"
                && payload["arguments"]["intent"] != "join"
        })
        .collect();
    assert_eq!(
        released.len(),
        4,
        "both original providers plan and release"
    );
    for (_, payload) in released {
        assert_eq!(
            payload["arguments"]["settings"],
            json!({"policy":"original"})
        );
        assert_eq!(
            payload["arguments"]["settings_revision"],
            "original-revision"
        );
    }
}

#[test]
fn restarted_external_shared_source_retires_consumers_before_original_policy() {
    for (source_first, source_retired_before_restart) in
        [(false, false), (true, false), (true, true)]
    {
        let harness = Harness::with_processing_mode(true, None, false, true);
        let (mut harness, session, source) = prepare_shared_source(harness);
        harness
            .settings
            .operation_grants
            .get_mut("capture")
            .unwrap()
            .insert("ack_stream".into());
        harness
            .coordinator
            .as_mut()
            .unwrap()
            .configure_for_test(harness.settings.clone());
        activate_external_consumer(&mut harness, &source["target"], "restart-source");
        let joined = join_external_consumer(&mut harness, &session, "restart-join", "client-two");
        if source_retired_before_restart {
            let retired =
                retire_shared_client(&mut harness, &source["target"], "restart-early-source");
            assert_eq!(retired["shared_retained"], true);
        }
        harness
            .settings
            .operation_grants
            .get_mut("capture")
            .unwrap()
            .insert("resolve_stream_preparation".into());
        harness
            .coordinator
            .as_mut()
            .unwrap()
            .configure_for_test(harness.settings.clone());
        let lookup = json!({"operation":"resolve_stream_preparation","session":session,
            "preparation_operation_id":"restart-join","preparation_caller":"transport"});
        let selected = harness.call("capture", lookup.clone());
        let typed_request: amaterasu_module_sdk::extension_api::StreamPreparationLookup =
            serde_json::from_value(lookup.clone()).unwrap();
        let typed_response: amaterasu_module_sdk::extension_api::StreamPreparationResolution =
            serde_json::from_value(selected.clone()).unwrap();
        amaterasu_module_sdk::extension_api::validate_stream_preparation_resolution(
            &typed_request,
            &typed_response,
        )
        .unwrap();
        assert_eq!(selected["target"], joined["target"]);
        let mut missing = lookup.clone();
        missing["preparation_operation_id"] = json!("never-admitted");
        assert_eq!(
            harness.call("capture", missing.clone())["status"],
            "pending"
        );
        let mut foreign = lookup.clone();
        foreign["preparation_caller"] = json!("foreign");
        assert_eq!(harness.call("capture", foreign)["status"], "rejected");
        harness.coordinator.take();
        harness.start();
        assert_eq!(harness.call("capture", lookup)["target"], joined["target"]);
        let missing = harness.call("capture", missing);
        assert_eq!(missing["status"], "missing");
        assert_eq!(missing["target"], Value::Null);
        let (first, last) = if source_first {
            (&source["target"], &joined["target"])
        } else {
            (&joined["target"], &source["target"])
        };
        let retire = |harness: &mut Harness, target: &Value, operation: &str| {
            let result = harness.call("capture",json!({"operation":"retire_stream",
                "operation_id":operation,"restart_recovery":true,"target":target,
                "retirement_receipt":{"service":"video/1","operation_id":format!("native-{operation}"),
                    "response":{"code":"not_found"}}}));
            assert_eq!(result["status"], "pending", "{result}");
            let mut result = result;
            for _ in 0..400 {
                result = harness.call(
                    "capture",
                    json!({"operation":"lookup","operation_id":operation}),
                );
                if result["status"] != "pending" {
                    break;
                }
            }
            assert_eq!(result["status"], "succeeded", "{result}");
            assert_eq!(result["target"], *target);
            result
        };
        if !source_retired_before_restart {
            let result = retire(&mut harness, first, "restart-first");
            assert_eq!(result["consumer_retired"], true);
            assert_eq!(result["resources_released"], false);
            assert_eq!(result["shared_retained"], true);
            assert!(harness.native_live);
        }
        let result = retire(&mut harness, last, "restart-last");
        assert_eq!(result["resources_released"], true, "{result}");
        assert_eq!(result["shared_retained"], false);
        assert!(!harness.native_live);
        for provider in ["display", "settings"] {
            assert_eq!(
                harness
                    .events
                    .iter()
                    .filter(|(p, call)| p == provider
                        && call["method"] == "lifecycle_callback"
                        && call["arguments"]["phase"] == "stream.release"
                        && call["arguments"]["intent"] != "join")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn failed_last_join_keeps_failure_after_deferred_source_cleanup() {
    let harness = Harness::with_processing_mode(true, None, false, true);
    let (mut harness, session, source) = prepare_shared_source(harness);
    let admitted=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"failed-last-join",
        "target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},
        "consumer_requirements":["capture"]}));
    assert_eq!(admitted["status"], "pending");
    harness.provider_invalid = true;
    let retired = retire_shared_client(&mut harness, &source["target"], "failed-last-source");
    assert_eq!(retired["status"], "succeeded", "{retired}");
    assert_eq!(
        retired["shared_retained"], true,
        "source must defer while final Join is pending"
    );
    let joined = wait_shared_operation(&mut harness, "failed-last-join");
    assert_eq!(joined["status"], "failed", "{joined}");
    assert_ne!(joined["reason"], Value::Null);
    assert_eq!(
        joined["resources_released"], true,
        "failure waits final source cleanup"
    );
    assert!(
        !harness.native_live,
        "failed pending Join cannot pin the source"
    );
}

#[test]
fn joined_client_churn_compacts_only_closed_history_and_reopens_safely() {
    let harness = Harness::with_processing_mode(true, None, false, true);
    let (mut harness, session, source) = prepare_shared_source(harness);
    harness
        .settings
        .operation_grants
        .get_mut("capture")
        .unwrap()
        .insert("ack_stream".into());
    harness
        .coordinator
        .as_mut()
        .unwrap()
        .configure_for_test(harness.settings.clone());
    activate_external_consumer(&mut harness, &source["target"], "churn-source");
    let mut first_join = Value::Null;
    for i in 0..68 {
        let join = join_external_consumer(
            &mut harness,
            &session,
            &format!("churn-join-{i}"),
            "same-client",
        );
        if i == 0 {
            first_join = join.clone();
        }
        let retired =
            retire_shared_client(&mut harness, &join["target"], &format!("churn-close-{i}"));
        assert_eq!(retired["shared_retained"], true, "{retired}");
        assert!(harness.native_live);
    }
    let old = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"churn-join-0"}),
    );
    assert!(
        old["status"] == "succeeded" || old["reason"] == "receipt_expired",
        "{old}"
    );
    let stale=harness.call("transport",json!({"operation":"pause_stream","operation_id":"churn-stale","target":first_join["target"]}));
    assert_eq!(stale["status"], "rejected", "{stale}");
    harness.coordinator.take();
    harness.start();
    assert!(harness.native_live, "reopening only fences old consumers");
    let retired = harness.call(
        "capture",
        json!({"operation":"retire_stream","operation_id":"churn-source-recovery",
        "restart_recovery":true,"target":source["target"],"retirement_receipt":{"service":"video/1",
        "operation_id":"source-video","response":{"code":"not_found"}}}),
    );
    assert_eq!(retired["status"], "pending", "{retired}");
    let mut retired = retired;
    for _ in 0..400 {
        retired = harness.call(
            "capture",
            json!({"operation":"lookup","operation_id":"churn-source-recovery"}),
        );
        if retired["status"] != "pending" {
            break;
        }
    }
    assert_eq!(retired["resources_released"], true, "{retired}");
    assert!(!harness.native_live);
}

#[test]
fn shared_external_deferred_restoration_never_certifies_resource_release() {
    for source_first in [false, true] {
        let mut harness = Harness::with_processing_mode(true, None, false, true);
        harness.cleanup_deferred = true;
        let (mut harness, session, source) = prepare_shared_source(harness);
        let joined=harness.call("transport",json!({"operation":"prepare_stream","operation_id":"deferred-join",
        "target":session,"intent":"join","requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"consumer_requirements":["capture"]}));
        assert_eq!(joined["status"], "pending");
        let joined = wait_shared_operation(&mut harness, "deferred-join");
        assert_eq!(joined["status"], "succeeded", "{joined}");
        let (first_target, last_target) = if source_first {
            (&source["target"], &joined["target"])
        } else {
            (&joined["target"], &source["target"])
        };
        let first = retire_shared_client(&mut harness, first_target, "deferred-source");
        assert_eq!(first["shared_retained"], true);
        assert_eq!(harness.call("transport",json!({"operation":"pause_stream","operation_id":"deferred-last","target":last_target}))["status"],"pending");
        let retired=harness.call("capture",json!({"operation":"retire_stream","operation_id":"deferred-last-capture",
        "target":last_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert!(
            matches!(retired["status"].as_str(), Some("pending" | "succeeded")),
            "{retired}"
        );
        for _ in 0..100 {
            harness.tick();
        }
        let last = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"deferred-last"}),
        );
        assert_eq!(
            last["status"],
            if source_first {
                "pending"
            } else {
                "recovery_required"
            },
            "{last}"
        );
        assert_ne!(last["resources_released"], true);
        assert!(harness.native_live);
        let journal: Value =
            serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap())
                .unwrap();
        assert!(journal["receipts"].as_object().unwrap().values().any(
            |receipt| receipt["response"]["reason"] == "shared_source_restoration_deferred"
                && !receipt["work"].is_null()
        ));
        harness.coordinator.take();
        harness.start();
        assert!(
            harness.native_live,
            "restart must preserve unconfirmed source restoration"
        );
    }
}

#[path = "coordinator/session_recovery_tests.rs"]
mod session_recovery_tests;
