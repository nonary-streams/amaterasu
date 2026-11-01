use super::*;

fn broker_effect_call(
    broker: &mut amaterasu_runtime::Broker,
    pipeline: &mut Pipeline,
    sequence: &mut u64,
    caller: &str,
    payload: Value,
) -> Value {
    *sequence += 1;
    let Response::Accepted { id } = broker.dispatch(
        caller,
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: format!("restart-review-{sequence}"),
            parent: None,
        },
    ) else {
        panic!("request rejected")
    };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } = broker.dispatch(caller, Command::TakeResult { request: id })
    else {
        panic!("response missing")
    };
    assert!(matches!(
        broker.dispatch(caller, Command::ReleaseResult { request: id }),
        Response::Ok
    ));
    payload
}

#[test]
fn broker_restart_cannot_replace_a_globally_fenced_transport_scope() {
    let (mut pipeline, _, _, epoch, source, scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut sequence = 0;
    let mut call = |caller, payload| {
        broker_effect_call(&mut broker, &mut pipeline, &mut sequence, caller, payload)
    };
    assert_eq!(
        call(
            "nvhttp",
            json!({"action":"fence_protocol_restart","epoch":epoch,"operation_id":"protocol-fence"})
        )["state"],
        "fenced"
    );
    let mut substituted_scope = scope.clone();
    substituted_scope["transport_stream"] = json!(8);
    assert_eq!(
        call(
            "nvhttp",
            json!({"action":"fence_source","epoch":epoch,"source":source,
        "scope":substituted_scope,"operation_id":"wrong-scope"})
        )["code"],
        "stream_scope_denied"
    );
    let fenced = call(
        "input",
        json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"owner-restart"}),
    );
    assert_eq!(fenced["count"], 1);
    let record = call(
        "input",
        json!({"action":"read_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"owner-restart","index":0}),
    );
    assert_eq!(record["record"]["scope"], scope);
    let wrong_receipt = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":8,
        "provider_epoch":"c".repeat(32),"operation_id":"wrong-transport","scope":"whole_control_stream","state":"retired"});
    assert_eq!(
        call(
            "input",
            json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"owner-restart","index":0,"retirement":wrong_receipt})
        )["code"],
        "retirement_mismatch"
    );
    assert_eq!(
        call(
            "input",
            json!({"action":"drain","epoch":epoch,"fence_generation":fenced["fence_generation"]})
        )["code"],
        "restart_confirmation_pending"
    );
}

#[test]
fn broker_restart_receipt_agrees_with_live_settlement_in_both_orders() {
    for confirmation_first in [false, true] {
        let (mut pipeline, _, _, epoch, source, scope) = authorized_feedback_for_test();
        let mut broker = authority_broker();
        let mut sequence = 0;
        let mut call = |caller, payload| {
            broker_effect_call(&mut broker, &mut pipeline, &mut sequence, caller, payload)
        };
        let lease = call(
            "input",
            json!({"action":"admit_stream_lease","epoch":epoch,"source":source,
            "scope":scope,"operation_id":"lease","sequence":1}),
        );
        let ticket = lease["ticket"].clone();
        let fence = call(
            "input",
            json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
            "owner_epoch":"owner","operation_id":"restart"}),
        );
        assert_eq!(fence["count"], 1);
        assert_eq!(
            call(
                "input",
                json!({"action":"fence_source","epoch":epoch,"source":source,
            "scope":scope,"operation_id":"source-fence"})
            )["state"],
            "fenced"
        );
        let receipt_a = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
            "provider_epoch":"c".repeat(32),"operation_id":"receipt-a","scope":"whole_control_stream","state":"retired"});
        let mut receipt_b = receipt_a.clone();
        receipt_b["operation_id"] = json!("receipt-b");
        let confirm = |receipt: &Value| {
            json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
            "owner_epoch":"owner","operation_id":"restart","index":0,"retirement":receipt})
        };
        let complete_lease = |receipt: &Value| {
            json!({"action":"complete_stream_lease","epoch":epoch,
            "operation_id":"lease","ticket":ticket,"retirement":receipt})
        };
        let complete_source = |receipt: &Value| {
            json!({"action":"complete_source_retirement","epoch":epoch,
            "source":source,"scope":scope,"operation_id":"source-fence","retirement":receipt})
        };
        if confirmation_first {
            assert_eq!(call("input", confirm(&receipt_b))["ok"], true);
            assert_eq!(
                call("input", complete_lease(&receipt_a))["code"],
                "retirement_mismatch"
            );
            assert_eq!(call("input", complete_lease(&receipt_b))["ok"], true);
        } else {
            assert_eq!(call("input", complete_lease(&receipt_a))["ok"], true);
        }
        assert_eq!(
            call(
                "input",
                json!({"action":"acknowledge","epoch":epoch,
            "operation_id":"lease","ticket":ticket})
            )["ok"],
            true
        );
        if confirmation_first {
            assert_eq!(
                call("input", complete_source(&receipt_a))["code"],
                "retirement_mismatch"
            );
            assert_eq!(
                call("input", complete_source(&receipt_b))["state"],
                "retired"
            );
        } else {
            assert_eq!(
                call("input", complete_source(&receipt_a))["state"],
                "retired"
            );
            assert_eq!(
                call("input", confirm(&receipt_b))["code"],
                "retirement_mismatch"
            );
            assert_eq!(
                call(
                    "input",
                    json!({"action":"drain","epoch":epoch,
                "fence_generation":fence["fence_generation"]})
                )["code"],
                "restart_confirmation_pending"
            );
            assert_eq!(call("input", confirm(&receipt_a))["ok"], true);
        }
        assert_eq!(
            call(
                "input",
                json!({"action":"drain","epoch":epoch,
            "fence_generation":fence["fence_generation"]})
            )["ok"],
            true
        );
        assert_eq!(
            call(
                "input",
                json!({"action":"recover","epoch":epoch,"owner_epoch":"next-owner"})
            )["ok"],
            true
        );
    }
}

#[test]
fn broker_restart_receipt_agrees_with_unleased_source_in_both_orders() {
    for confirmation_first in [false, true] {
        let (mut pipeline, _, _, epoch, source, scope) = authorized_feedback_for_test();
        let mut broker = authority_broker();
        let mut sequence = 0;
        let mut call = |caller, payload| {
            broker_effect_call(&mut broker, &mut pipeline, &mut sequence, caller, payload)
        };
        let fence = call(
            "input",
            json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
            "owner_epoch":"owner","operation_id":"restart"}),
        );
        assert_eq!(
            call(
                "input",
                json!({"action":"fence_source","epoch":epoch,"source":source,
            "scope":scope,"operation_id":"source-fence"})
            )["state"],
            "fenced"
        );
        let receipt_a = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
            "provider_epoch":"c".repeat(32),"operation_id":"receipt-a","scope":"whole_control_stream","state":"retired"});
        let mut receipt_b = receipt_a.clone();
        receipt_b["operation_id"] = json!("receipt-b");
        let confirm = |receipt: &Value| {
            json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
            "owner_epoch":"owner","operation_id":"restart","index":0,"retirement":receipt})
        };
        let retire = |receipt: &Value| {
            json!({"action":"complete_source_retirement","epoch":epoch,
            "source":source,"scope":scope,"operation_id":"source-fence","retirement":receipt})
        };
        if confirmation_first {
            assert_eq!(call("input", confirm(&receipt_b))["ok"], true);
            assert_eq!(
                call("input", retire(&receipt_a))["code"],
                "retirement_mismatch"
            );
            assert_eq!(call("input", retire(&receipt_b))["state"], "retired");
        } else {
            assert_eq!(call("input", retire(&receipt_a))["state"], "retired");
            assert_eq!(
                call("input", confirm(&receipt_b))["code"],
                "retirement_mismatch"
            );
            assert_eq!(call("input", confirm(&receipt_a))["ok"], true);
        }
        assert_eq!(
            call(
                "input",
                json!({"action":"drain","epoch":epoch,
            "fence_generation":fence["fence_generation"]})
            )["ok"],
            true
        );
    }
}

#[test]
fn broker_restart_confirmed_receipt_binds_other_sources_on_same_transport() {
    let (mut pipeline, _, _, epoch, first, first_scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut sequence = 0;
    let mut call = |caller, payload| {
        broker_effect_call(&mut broker, &mut pipeline, &mut sequence, caller, payload)
    };
    let mut second = first.clone();
    second["id"] = json!("b".repeat(32));
    second["device"] = json!("second-controller");
    let mut second_scope = first_scope.clone();
    second_scope["controller_slot"] = json!(4);
    assert_eq!(
        call(
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,
        "source":second,"scope":second_scope})
        )["ok"],
        true
    );
    let fence = call(
        "input",
        json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart"}),
    );
    assert_eq!(fence["count"], 2);
    for (source, scope, operation_id) in [
        (&first, &first_scope, "first"),
        (&second, &second_scope, "second"),
    ] {
        assert_eq!(
            call(
                "input",
                json!({"action":"fence_source","epoch":epoch,"source":source,
            "scope":scope,"operation_id":operation_id})
            )["state"],
            "fenced"
        );
    }
    let receipt = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
        "provider_epoch":"c".repeat(32),"operation_id":"receipt-a","scope":"whole_control_stream","state":"retired"});
    let mut other = receipt.clone();
    other["operation_id"] = json!("receipt-b");
    assert_eq!(
        call(
            "input",
            json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart","index":0,"retirement":receipt})
        )["ok"],
        true
    );
    let first_page = call(
        "input",
        json!({"action":"read_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart","index":0}),
    );
    let (other_source, other_scope, other_id) = if first_page["record"]["source"] == first {
        (&second, &second_scope, "second")
    } else {
        (&first, &first_scope, "first")
    };
    assert_eq!(
        call(
            "input",
            json!({"action":"complete_source_retirement","epoch":epoch,"source":other_source,
        "scope":other_scope,"operation_id":other_id,"retirement":other})
        )["code"],
        "retirement_mismatch"
    );
    for (source, scope, operation_id) in [
        (&first, &first_scope, "first"),
        (&second, &second_scope, "second"),
    ] {
        assert_eq!(
            call(
                "input",
                json!({"action":"complete_source_retirement","epoch":epoch,"source":source,
            "scope":scope,"operation_id":operation_id,"retirement":receipt})
            )["state"],
            "retired"
        );
    }
    assert_eq!(
        call(
            "input",
            json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart","index":1,"retirement":receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            "input",
            json!({"action":"drain","epoch":epoch,
        "fence_generation":fence["fence_generation"]})
        )["ok"],
        true
    );
}

#[test]
fn broker_owner_restart_enumerates_and_settles_exact_prior_leases() {
    let (mut pipeline, _harness, _, epoch, first, first_scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut request_sequence = 0;
    let mut call = |pipeline: &mut Pipeline, caller: &str, payload: Value| {
        request_sequence += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: EFFECT_SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("owner-restart-test-{request_sequence}"),
                parent: None,
            },
        ) else {
            panic!("broker request rejected")
        };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("broker reply missing")
        };
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    };
    let mut second = first.clone();
    second["id"] = json!("b".repeat(32));
    second["device"] = json!("second-device");
    let mut second_scope = first_scope.clone();
    second_scope["controller_slot"] = json!(4);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,
        "certificate_pem":CLIENT_CERT,"source":second,"scope":second_scope})
        )["ok"],
        true
    );
    let mut third = first.clone();
    third["id"] = json!("d".repeat(32));
    third["device"] = json!("third-device");
    let mut third_scope = first_scope.clone();
    third_scope["controller_slot"] = json!(5);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,
        "certificate_pem":CLIENT_CERT,"source":third,"scope":third_scope})
        )["ok"],
        true
    );
    let mut fourth = first.clone();
    fourth["id"] = json!("e".repeat(32));
    fourth["device"] = json!("already-retired-device");
    let mut fourth_scope = first_scope.clone();
    fourth_scope["controller_slot"] = json!(6);
    fourth_scope["transport_stream"] = json!(8);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,
        "certificate_pem":CLIENT_CERT,"source":fourth,"scope":fourth_scope})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"fence_source","epoch":epoch,
        "source":fourth,"scope":fourth_scope,"operation_id":"already-retired"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_source_retirement","epoch":epoch,
        "source":fourth,"scope":fourth_scope,"operation_id":"already-retired","retirement":null})
        )["state"],
        "retired"
    );
    let mut fifth = first.clone();
    fifth["id"] = json!("f".repeat(32));
    fifth["device"] = json!("acknowledged-device");
    let mut fifth_scope = first_scope.clone();
    fifth_scope["controller_slot"] = json!(7);
    fifth_scope["transport_stream"] = json!(9);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,
        "certificate_pem":CLIENT_CERT,"source":fifth,"scope":fifth_scope})
        )["ok"],
        true
    );
    let first_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,
        "source":first,"scope":first_scope,"operation_id":"completed-lease","sequence":1}),
    );
    let second_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,
        "source":second,"scope":second_scope,"operation_id":"pending-lease","sequence":2}),
    );
    let first_ticket = first_lease["ticket"].clone();
    let second_ticket = second_lease["ticket"].clone();
    // The test receipt models the owner's verified control-provider reply.
    let receipt = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
        "provider_epoch":"c".repeat(32),"operation_id":"control-retire",
        "scope":"whole_control_stream","state":"retired"});
    let mut fifth_receipt = receipt.clone();
    fifth_receipt["stream"] = json!(9);
    fifth_receipt["operation_id"] = json!("control-retire-fifth");
    let fifth_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,
        "source":fifth,"scope":fifth_scope,"operation_id":"acknowledged-lease","sequence":3}),
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,
        "operation_id":"acknowledged-lease","ticket":fifth_lease["ticket"],"retirement":fifth_receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"acknowledge","epoch":epoch,
        "operation_id":"acknowledged-lease","ticket":fifth_lease["ticket"]})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,
        "operation_id":"completed-lease","ticket":first_ticket,"retirement":receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"fence_source","epoch":epoch,
        "source":second,"scope":second_scope,"operation_id":"existing-source-fence"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"fence_protocol_restart",
        "epoch":epoch,"operation_id":"earlier-protocol-restart"})
        )["state"],
        "fenced"
    );
    let fence = json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart-one"});
    assert_eq!(
        call(&mut pipeline, "foreign", fence.clone())["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut pipeline, "nvhttp", fence.clone())["code"],
        "forbidden"
    );
    assert_eq!(
        pipeline.apply_effect_request("input", "foreign", fence.clone(), now()),
        Err("forbidden")
    );
    let mut guessed = fence.clone();
    guessed["owner_epoch"] = json!("replacement-owner");
    assert_eq!(
        call(&mut pipeline, "input", guessed)["code"],
        "owner_epoch_unknown"
    );
    let fenced = call(&mut pipeline, "input", fence.clone());
    assert_eq!(fenced["count"], 5);
    let typed_fence: amaterasu_module_sdk::effect_owner_recovery::FenceReceipt =
        serde_json::from_value(fenced.clone()).unwrap();
    assert!(typed_fence.matches(&epoch, "owner", "restart-one"));
    assert_eq!(call(&mut pipeline, "input", fence.clone()), fenced);
    let mut changed = fence.clone();
    changed["operation_id"] = json!("different-restart");
    assert_eq!(
        call(&mut pipeline, "input", changed)["code"],
        "owner_restart_pending"
    );
    let read = |index| {
        json!({"action":"read_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart-one","index":index})
    };
    assert_eq!(call(&mut pipeline, "foreign", read(0))["code"], "forbidden");
    assert_eq!(
        call(&mut pipeline, "input", read(5))["code"],
        "index_out_of_range"
    );
    let pages: Vec<Value> = (0..5)
        .map(|index| call(&mut pipeline, "input", read(index)))
        .collect();
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(page["count"], 5);
        let typed_page: amaterasu_module_sdk::effect_owner_recovery::Page =
            serde_json::from_value(page.clone()).unwrap();
        assert!(typed_page.matches(&typed_fence, index as u32));
        assert!(!typed_page.matches(&typed_fence, (index + 1) as u32));
        assert!(page.to_string().find("certificate").is_none());
    }
    let mut missing_lease = pages[0].clone();
    missing_lease["record"]
        .as_object_mut()
        .unwrap()
        .remove("lease");
    assert!(
        serde_json::from_value::<amaterasu_module_sdk::effect_owner_recovery::Page>(missing_lease)
            .is_err()
    );
    let mut forged_owner = pages[0].clone();
    forged_owner["record"]["source"]["effect_owner_epoch"] = json!("other-owner");
    let forged: amaterasu_module_sdk::effect_owner_recovery::Page =
        serde_json::from_value(forged_owner).unwrap();
    assert!(!forged.matches(&typed_fence, 0));
    let first_record = pages
        .iter()
        .find(|page| page["record"]["source"] == first)
        .unwrap();
    let second_record = pages
        .iter()
        .find(|page| page["record"]["source"] == second)
        .unwrap();
    let third_record = pages
        .iter()
        .find(|page| page["record"]["source"] == third)
        .unwrap();
    assert!(third_record["record"]["lease"].is_null());
    let fourth_record = pages
        .iter()
        .find(|page| page["record"]["source"] == fourth)
        .unwrap();
    assert_eq!(
        fourth_record["record"]["source_retirement"]["complete"],
        true
    );
    assert!(fourth_record["record"]["source_retirement"]["retirement"].is_null());
    let fifth_record = pages
        .iter()
        .find(|page| page["record"]["source"] == fifth)
        .unwrap();
    assert!(fifth_record["record"]["lease"].is_null());
    assert_eq!(
        fifth_record["record"]["transport_retirement"],
        fifth_receipt
    );
    assert_eq!(first_record["record"]["lease"]["ticket"], first_ticket);
    assert_eq!(first_record["record"]["lease"]["complete"], true);
    assert_eq!(first_record["record"]["lease"]["retirement"], receipt);
    assert_eq!(second_record["record"]["lease"]["ticket"], second_ticket);
    assert_eq!(second_record["record"]["lease"]["complete"], false);
    assert_eq!(
        second_record["record"]["source_retirement"]["operation_id"],
        "existing-source-fence"
    );
    let generation = fenced["fence_generation"].clone();
    let drain = json!({"action":"drain","epoch":epoch,"fence_generation":generation});
    assert_eq!(
        call(&mut pipeline, "input", drain.clone())["code"],
        "effects_in_flight"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"recover","epoch":epoch,
        "owner_epoch":"replacement-owner"})
        )["code"],
        "recovery_required"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"fence_source","epoch":epoch,
        "source":first,"scope":first_scope,"operation_id":"first-source-fence"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"fence_source","epoch":epoch,
        "source":third,"scope":third_scope,"operation_id":"third-source-fence"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"fence_source","epoch":epoch,
        "source":fifth,"scope":fifth_scope,"operation_id":"fifth-source-fence"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,
        "operation_id":"pending-lease","ticket":second_ticket,"retirement":receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(&mut pipeline, "input", drain.clone())["code"],
        "lease_acknowledgment_pending"
    );
    for (operation_id, ticket) in [
        ("completed-lease", first_ticket),
        ("pending-lease", second_ticket),
    ] {
        assert_eq!(
            call(
                &mut pipeline,
                "input",
                json!({"action":"acknowledge","epoch":epoch,
            "operation_id":operation_id,"ticket":ticket})
            )["ok"],
            true
        );
    }
    for (source, scope, operation_id) in [
        (&first, &first_scope, "first-source-fence"),
        (&second, &second_scope, "existing-source-fence"),
        (&third, &third_scope, "third-source-fence"),
        (&fifth, &fifth_scope, "fifth-source-fence"),
    ] {
        let actual_receipt = if *source == fifth {
            &fifth_receipt
        } else {
            &receipt
        };
        assert_eq!(
            call(
                &mut pipeline,
                "input",
                json!({"action":"complete_source_retirement","epoch":epoch,
            "source":source,"scope":scope,"operation_id":operation_id,"retirement":null})
            )["code"],
            "retirement_mismatch"
        );
        assert_eq!(
            call(
                &mut pipeline,
                "input",
                json!({"action":"complete_source_retirement","epoch":epoch,
            "source":source,"scope":scope,"operation_id":operation_id,"retirement":actual_receipt})
            )["state"],
            "retired"
        );
    }
    assert_eq!(
        call(&mut pipeline, "input", drain.clone())["code"],
        "restart_confirmation_pending"
    );
    for (index, page) in pages.iter().enumerate() {
        let mut actual_receipt = receipt.clone();
        if page["record"]["source"] == fourth {
            actual_receipt["stream"] = json!(8);
            actual_receipt["operation_id"] = json!("control-retire-fourth");
        }
        if page["record"]["source"] == fifth {
            actual_receipt = fifth_receipt.clone();
        }
        let confirmation = json!({"action":"confirm_effect_owner_restart","version":1,"epoch":epoch,
            "owner_epoch":"owner","operation_id":"restart-one","index":index,"retirement":actual_receipt});
        assert_eq!(
            call(&mut pipeline, "foreign", confirmation.clone())["code"],
            "forbidden"
        );
        let mut wrong = confirmation.clone();
        wrong["retirement"]["operation_id"] = json!("wrong-receipt");
        if page["record"]["lease"]["complete"] == true {
            assert_eq!(
                call(&mut pipeline, "input", wrong)["code"],
                "retirement_mismatch"
            );
        }
        assert_eq!(
            call(&mut pipeline, "input", confirmation.clone())["ok"],
            true
        );
        assert_eq!(call(&mut pipeline, "input", confirmation)["ok"], true);
    }
    // Snapshot does not change after settlement or a lost page reply.
    assert_eq!(call(&mut pipeline, "input", read(0)), pages[0]);
    assert_eq!(call(&mut pipeline, "input", drain)["ok"], true);
    let recovered = call(
        &mut pipeline,
        "input",
        json!({"action":"recover","epoch":epoch,
        "owner_epoch":"replacement-owner"}),
    );
    assert_eq!(recovered["ok"], true);
    assert_ne!(recovered["epoch"], epoch);
    assert_eq!(call(&mut pipeline, "input", read(0))["code"], "stale_epoch");
}

#[test]
fn broker_cancel_and_lost_reply_leave_restart_reconcilable() {
    let (mut pipeline, _harness, _, epoch, _, _) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let fence = json!({"action":"fence_effect_owner_restart","version":1,"epoch":epoch,
        "owner_epoch":"owner","operation_id":"restart-cancel"});
    let request = |operation: &str| Command::Request {
        service: EFFECT_SERVICE.into(),
        payload: fence.clone(),
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent: None,
    };
    let Response::Accepted { id } = broker.dispatch("input", request("cancel-before-check")) else {
        panic!("request rejected")
    };
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result, Response::Request { id: delivered, .. } if *delivered == id) {
                assert!(matches!(
                    broker.dispatch("input", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    assert!(!pipeline.effect_gate.as_ref().unwrap().fenced());
    let Response::Accepted { id } = broker.dispatch("input", request("lose-fence-reply")) else {
        panic!("request rejected")
    };
    pipeline
        .tick_at(now(), |command| {
            if matches!(&command, Command::Complete { request, .. } if *request == id) {
                assert!(matches!(
                    broker.dispatch("input", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            Ok(broker.dispatch("pairing", command))
        })
        .unwrap();
    assert!(pipeline.effect_gate.as_ref().unwrap().fenced());
    let Response::Accepted { id } = broker.dispatch("input", request("retry-fence")) else {
        panic!("request rejected")
    };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: id })
    else {
        panic!("retry response missing")
    };
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["operation_id"], "restart-cancel");

    // A restarted Pairing authority cannot claim the old volatile ledger was empty.
    pipeline.effect_gate = Some(EffectGate::new().unwrap());
    let new_epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let mut after_restart = fence;
    after_restart["epoch"] = json!(new_epoch);
    assert_eq!(
        pipeline.apply_effect_request("input", "input", after_restart, now()),
        Err("owner_epoch_unknown")
    );
}
