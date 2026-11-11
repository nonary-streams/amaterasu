use super::*;

#[test]
fn external_clients_receive_independent_input_bindings_without_capture_candidates() {
    let root = std::env::temp_dir().join(format!("external-input-{}", identifier().unwrap()));
    std::fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
    coordinator
        .settings
        .consumers
        .input
        .insert("input-provider".into());
    coordinator.settings.input_provider = Some("input-provider".into());
    let mut next = coordinator.shared.borrow().value.clone();
    let epoch = next.host_epoch.clone();
    let session: SessionRecord = serde_json::from_value(json!({
        "capture_binding_mode":"external","host_epoch":epoch,"id":"session",
        "generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"client"},
        "delegates":[],"application_ref":null,"settings_revision":"one","application_retired":false,
        "streams":{
            "first":{"generation":1,"phase":"ready","candidate":null,"required":["capture"],"acknowledgements":["capture"],"retirements":[]},
            "second":{"generation":2,"phase":"ready","candidate":null,"required":["capture"],"acknowledgements":["capture"],"retirements":[]}
        }
    })).unwrap();
    next.sessions.insert("session".into(), session);
    save(&coordinator.shared, next).unwrap();
    let mut tokens = Vec::new();
    for (stream, generation) in [("first", 1), ("second", 2)] {
        let target = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":stream,"generation":generation});
        let request = json!({"operation":"issue_input_binding","operation_id":format!("input-{stream}"),"target":target});
        let issued = coordinator.security_operation("transport", "client", &request);
        assert_eq!(issued["status"], "succeeded", "{issued}");
        assert!(issued["plan_revision"].is_null());
        assert_eq!(issued["geometry_revision"], generation);
        assert_eq!(
            coordinator.security_operation("transport", "client", &request),
            issued
        );
        let token = issued["token"].as_str().unwrap().to_owned();
        let validate =
            json!({"operation":"validate_input_binding","token":token,"input_boot_epoch":"boot"});
        let validated =
            coordinator.security_operation("input-provider", "input-provider", &validate);
        assert_eq!(validated["target"], target);
        assert_eq!(validated["geometry_revision"], generation);
        assert_eq!(
            coordinator.security_operation("foreign-provider", "foreign-provider", &validate)["reason"],
            "permission_denied"
        );
        tokens.push(token);
    }
    assert_ne!(tokens[0], tokens[1]);
    coordinator
        .settings
        .consumers
        .input
        .insert("transport".into());
    for (index, stream) in ["first", "second"].into_iter().enumerate() {
        let target = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":stream,"generation":index+1});
        let stored = coordinator.shared.borrow().value.sessions["session"].streams[stream].clone();
        assert_eq!(
            stored.required,
            BTreeSet::from(["capture".into(), "input".into()])
        );
        assert!(!stored.acknowledgements.contains("input"));
        let request = json!({"operation":"ack_binding","operation_id":format!("ack-{stream}"),"target":target,"candidate":null,"plan_revision":null,"consumer":"input","token":tokens[index],"geometry_revision":index+1});
        let key = receipt_key(
            "transport",
            "client",
            request["operation_id"].as_str().unwrap(),
        );
        let admitted = coordinator
            .admit_binding_validation(
                "transport",
                "client",
                request,
                key.clone(),
                serde_json::from_value(target.clone()).unwrap(),
                true,
            )
            .unwrap();
        assert_eq!(admitted["status"], "pending");
        let job = coordinator.validations.remove(&key).unwrap();
        let bound = json!({"ok":true,"status":"bound","target":target,"token":tokens[index],"geometry_revision":index+1,"lease":"independent-native-lease"});
        coordinator.finish_validation(&job, Some(bound)).unwrap();
        assert!(
            coordinator.shared.borrow().value.sessions["session"].streams[stream]
                .acknowledgements
                .contains("input")
        );
    }
    let mut next = coordinator.shared.borrow().value.clone();
    let target = LifecycleTarget::Stream {
        host_epoch: epoch.clone(),
        session_id: "session".into(),
        stream_id: "first".into(),
        generation: 1,
    };
    coordinator.fence_stream(&mut next, &target).unwrap();
    next.sessions
        .get_mut("session")
        .unwrap()
        .streams
        .get_mut("first")
        .unwrap()
        .phase = StreamPhase::Quiescing;
    assert!(next.revoked_inputs.contains(&tokens[0]));
    assert!(!next.revoked_inputs.contains(&tokens[1]));
    save(&coordinator.shared, next).unwrap();
    for (index, expected) in [(0, "rejected"), (1, "succeeded")] {
        let validate = json!({"operation":"validate_input_binding","token":tokens[index],"input_boot_epoch":"boot"});
        assert_eq!(
            coordinator.security_operation("input-provider", "input-provider", &validate)["status"],
            expected
        );
    }
    let mut next = coordinator.shared.borrow().value.clone();
    next.sessions
        .get_mut("session")
        .unwrap()
        .streams
        .get_mut("second")
        .unwrap()
        .acknowledgements
        .clear();
    save(&coordinator.shared, next).unwrap();
    let request = json!({"operation":"issue_input_binding","operation_id":"unacknowledged","target":{"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"second","generation":2}});
    assert_eq!(
        coordinator.security_operation("transport", "client", &request)["reason"],
        "capture_not_acknowledged"
    );
    drop(coordinator);
    std::fs::remove_dir_all(root).unwrap();
}
