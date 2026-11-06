use super::*;

fn client() -> Client {
    Client {
        id: "certificate-sha256".into(),
        paired: true,
        may_view: true,
        may_launch: true,
        may_terminate: false,
    }
}
fn app() -> Application {
    Application {
        id: "opaque-entry".into(),
        uuid: "".into(),
        title: "A game".into(),
        aliases: vec![],
        visible: true,
        monitor_profile: false,
        properties: json!({}),
    }
}
fn context() -> Context {
    Context {
        client: client(),
        game: None,
    }
}

#[test]
fn api_roundtrip_requires_exact_selected_service_and_declared_envelope() {
    let request = Request::Info { context: context() };
    let options = || RequestOptions {
        service: SERVICE,
        timeout_ms: 500,
        operation_id: "info1",
        parent: None,
    };
    let Command::CallApi {
        service,
        method,
        interface_major,
        interface_minor,
        payload,
        ..
    } = command(options(), &request).unwrap()
    else {
        panic!()
    };
    assert_eq!(decode(&service,json!({"method":method,"interface_major":interface_major,"interface_minor":interface_minor,"arguments":payload})).unwrap(),request);
    assert!(decode(SERVICE, serde_json::to_value(&request).unwrap()).is_err());
    assert!(decode("other/1", json!({})).is_err());
    let mut wrong = options();
    wrong.service = "other/1";
    assert!(command(wrong, &request).is_err());
}

#[test]
fn catalog_pages_enforce_rows_and_serialized_bytes_independently() {
    let page = |entries| Request::AppendProject {
        client_id: "client".into(),
        projection_id: "transaction".into(),
        entries,
        final_page: false,
    };
    assert!(page(vec![app(); MAX_PAGE_ENTRIES]).validate().is_ok());
    assert!(page(vec![app(); MAX_PAGE_ENTRIES + 1]).validate().is_err());
    let mut large = app();
    large.properties = json!({"data":"x".repeat(4000)});
    assert!(large.validate().is_ok());
    assert!(page(vec![large; 4]).validate().is_err());
    let mut hidden = app();
    hidden.visible = false;
    hidden.aliases = vec!["retained-id".into()];
    assert!(page(vec![hidden]).validate().is_ok());
}

#[test]
fn decorated_catalog_titles_retain_their_complete_original_name() {
    let mut decorated = app();
    decorated.title = format!("     {}", "g".repeat(160));
    assert!(decorated.validate().is_ok());
    let projected = Reply::Projected {
        presentation: Presentation {
            busy: false,
            current_game: None,
            current_game_uuid: None,
        },
        entries: vec![decorated.clone()],
        next_offset: None,
    };
    assert!(projected.validate().is_ok());
    decorated.title = "x".repeat(MAX_TITLE_BYTES + 1);
    assert!(decorated.validate().is_err());
}

#[test]
fn identities_and_permissions_remain_authenticated_facts() {
    let mut request = context();
    request.client.paired = false;
    assert!(Request::Info { context: request }.validate().is_err());
    let mut request = context();
    request.game = Some(Game {
        application: app(),
        owner_client_id: "owner".into(),
        generation: 0,
    });
    assert!(Request::Info { context: request }.validate().is_err());
    let mut bad = client();
    bad.id = "caller\nspoof".into();
    assert!(bad.validate().is_err());
}

#[test]
fn input_only_plans_cannot_request_host_capture_or_audio() {
    let mut plan = AuxiliaryStreamPlan {
        session_id: "session".into(),
        generation: 1,
        reservation_id: "operation".into(),
        capture: CaptureSource::SyntheticBlack,
        mode: Mode {
            width: 1280,
            height: 720,
            refresh_millihz: 60_000,
            hdr: false,
        },
        stream_audio: false,
        host_audio: false,
    };
    assert!(plan.validate().is_ok());
    plan.stream_audio = true;
    assert!(plan.validate().is_err());
    plan.stream_audio = false;
    plan.host_audio = true;
    assert!(plan.validate().is_err());
    plan.host_audio = false;
    plan.generation = 0;
    assert!(plan.validate().is_err());
    assert!(
        SharedCapture {
            session_id: "session".into(),
            generation: 1,
            reservation_id: "operation".into(),
            capture: CaptureSource::SyntheticBlack,
            mode: Mode {
                width: 1280,
                height: 720,
                refresh_millihz: 60_000,
                hdr: false
            },
        }
        .validate()
        .is_err()
    );
}

#[test]
fn stop_and_replacement_decisions_require_observed_generation() {
    let mut decision = Decision::StopApplication {
        application_id: "game".into(),
        generation: 7,
        completion: ControlCompletion {
            status_code: 410,
            message: "Terminated".into(),
        },
    };
    assert!(decision.validate().is_ok());
    if let Decision::StopApplication { generation, .. } = &mut decision {
        *generation = 0;
    }
    assert!(decision.validate().is_err());
    assert!(
        Decision::ReplaceApplication {
            application_id: "next".into(),
            expected_current_generation: 0,
            source: None
        }
        .validate()
        .is_err()
    );
}

#[test]
fn replies_reject_malformed_state_and_unbounded_retry_or_artwork() {
    assert!(decode_reply(json!({"result":"info","presentation":{"busy":true,"current_game":null,"current_game_uuid":null}})).is_err());
    assert!(decode_reply(json!({"result":"pending","retry_after_ms":0})).is_err());
    assert!(decode_reply(json!({"result":"pending","retry_after_ms":1001})).is_err());
    assert!(decode_reply(json!({"result":"acknowledged","extra":true})).is_err());
    assert!(
        Reply::Artwork {
            artwork: Artwork::InlinePng { bytes: vec![0; 16] }
        }
        .validate()
        .is_err()
    );
    assert!(
        Reply::Info {
            presentation: Presentation {
                busy: false,
                current_game: None,
                current_game_uuid: None
            }
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn method_contracts_have_unique_exact_identity_and_no_raw_action_field() {
    let methods = methods();
    assert_eq!(methods.len(), 12);
    let names: std::collections::BTreeSet<_> = methods.iter().map(|m| m.method.as_str()).collect();
    assert_eq!(methods.len(), names.len());
    for method in methods {
        assert_eq!(method.service, SERVICE);
        assert_eq!((method.interface_major, method.interface_minor), (1, 0));
        assert!(!method.request.allow_extra);
        assert!(!method.request.required.contains_key("action"));
        assert_eq!(method.response.required["result"], ApiFieldKind::String);
    }
}

#[test]
fn transport_binding_requires_exact_owner_epoch_before_effects() {
    let request = Request::BindTransport {
        client_id: "client".into(),
        session_id: "session".into(),
        generation: 9,
        reservation_id: "admission".into(),
        transport_id: "stream-operation".into(),
        transport_epoch: "server-epoch".into(),
    };
    assert!(request.validate().is_ok());
    let mut invalid = request.clone();
    if let Request::BindTransport {
        transport_epoch, ..
    } = &mut invalid
    {
        *transport_epoch = String::new();
    }
    assert!(invalid.validate().is_err());
    let mut invalid = request;
    if let Request::BindTransport { generation, .. } = &mut invalid {
        *generation = 0;
    }
    assert!(invalid.validate().is_err());
    let legacy = serde_json::json!({"action":"observe","client_id":"client","session_id":"session","generation":9,
        "reservation_id":"admission","transport_id":"stream-operation","event":"failed_start"});
    let decoded: Request = serde_json::from_value(legacy).unwrap();
    assert!(decoded.validate().is_ok());
    let method = methods()
        .into_iter()
        .find(|method| method.method == "observe")
        .unwrap();
    assert_eq!(
        method.request.optional["transport_epoch"],
        ApiFieldKind::Any
    );
}
