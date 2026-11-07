use super::*;

#[derive(Clone, Copy, Debug)]
enum Route {
    Executable,
    Playnite,
    Generic,
}

fn configured(route: Route) -> (Pipeline, Harness, Value) {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    match route {
        Route::Executable => harness.application_preparing_statuses = 8,
        Route::Playnite => {
            harness.catalog_playnite_game_id = Some("a1b2c3d4-1234-5678-9abc-0123456789ab".into());
        }
        Route::Generic => {
            harness.catalog_generic = true;
            harness.catalog_provenance =
                Some(json!({"provider":"example.importer","source_id":"42"}));
            pipeline.set_provider_routes(
                [(
                    "example".into(),
                    ProviderRoute {
                        importer: "example.importer".into(),
                        service: "example.provider/1".into(),
                    },
                )]
                .into(),
            );
        }
    }
    let preflight = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "management",
        json!({"action":"preflight_export","operation_id":"admission-race",
            "app_id":"app-0000000000000001"}),
    );
    assert_eq!(preflight["ok"], true, "{preflight}");
    let mut request = launch_export("admission-race");
    request["preflight_token"] = preflight["preflight_token"].clone();
    request["return_on_admission"] = json!(true);
    (pipeline, harness, request)
}

fn enqueue(harness: &mut Harness, payload: Value) {
    harness.output = None;
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "amaterasu.nvhttp".into(),
        initiator: "management".into(),
        service: SERVICE.into(),
        payload,
        remaining_ms: 30_000,
    });
}

fn effect_count(harness: &Harness, route: Route) -> usize {
    let (service, action) = match route {
        Route::Executable => (crate::workflow::APPLICATION, "launch"),
        Route::Playnite => (crate::workflow::PLAYNITE, "launch_start"),
        Route::Generic => ("example.provider/1", "launch"),
    };
    harness
        .commands
        .iter()
        .filter(|(candidate, payload, _)| candidate == service && payload["action"] == action)
        .count()
}

#[test]
fn cancelled_admission_retires_all_three_routes_without_replay() {
    for route in [Route::Executable, Route::Playnite, Route::Generic] {
        for rejection in ["request_ended", "invalid_request"] {
            let (mut pipeline, mut harness, request) = configured(route);
            enqueue(&mut harness, request);
            let mut rejected = false;
            for _ in 0..150 {
                pipeline
                    .tick(|command| {
                        if let Command::Complete { ref payload, .. } = command
                            && !rejected
                            && payload["launch"]["state"] == "starting"
                        {
                            rejected = true;
                            harness.root_active = false;
                            return Ok(Response::error(rejection, "admission rejected"));
                        }
                        harness.send(command)
                    })
                    .unwrap();
                if rejected && pipeline.job.is_none() {
                    break;
                }
            }
            assert!(rejected, "{route:?}: admission was not attempted");
            assert_eq!(effect_count(&harness, route), 1, "{route:?}");
            let record = &pipeline.records.values[&1];
            assert!(
                record.state == State::Cancelled,
                "{route:?}: {}",
                record.response()
            );
            assert!(record.native_retirement_confirmed, "{route:?}");
            assert!(
                harness
                    .sessions
                    .iter()
                    .all(|session| session.lease.is_none())
            );
            assert!(
                harness
                    .processes
                    .iter()
                    .all(|process| process.child.is_none())
            );
            assert!(
                harness
                    .provider_launches
                    .iter()
                    .all(|launch| launch.state == "stopped")
            );
        }
    }
}

#[test]
fn ambiguous_admission_keeps_owned_effects_stoppable_without_replay() {
    for route in [Route::Executable, Route::Playnite, Route::Generic] {
        let (mut pipeline, mut harness, request) = configured(route);
        enqueue(&mut harness, request);
        let mut transport_lost = false;
        let mut ended_seen = false;
        for _ in 0..150 {
            let result = pipeline.tick(|command| {
                if let Command::Complete { ref payload, .. } = command
                    && payload["launch"]["state"] == "starting"
                {
                    if !transport_lost {
                        transport_lost = true;
                        // The broker accepted completion, but its response was lost.
                        assert!(matches!(harness.send(command), Ok(Response::Ok)));
                        return Err("injected lost completion response".into());
                    }
                    ended_seen = true;
                    return Ok(Response::error("request_ended", "already completed"));
                }
                harness.send(command)
            });
            if result.is_err() && !transport_lost {
                panic!("unexpected transport failure");
            }
            if transport_lost
                && (ended_seen || pipeline.job.as_ref().is_some_and(|j| j.request.is_none()))
            {
                break;
            }
        }
        assert!(transport_lost, "{route:?}");
        assert_eq!(effect_count(&harness, route), 1, "{route:?}");
        assert!(
            pipeline.job.is_some(),
            "{route:?}: lost the in-flight workflow"
        );
        assert!(harness.sessions[0].lease.is_some(), "{route:?}");
        harness.root_active = true;
        let stopped = harness.request_as(
            &mut pipeline,
            "amaterasu.nvhttp",
            "management",
            json!({"action":"stop","launch_id":1}),
        );
        assert_eq!(
            stopped["launch"]["state"], "stopped",
            "{route:?}: {stopped}"
        );
        assert_eq!(effect_count(&harness, route), 1, "{route:?}");
        assert!(harness.sessions[0].lease.is_none());
    }
}

#[test]
fn generic_provider_starting_acknowledges_admission_before_running() {
    let (mut pipeline, mut harness, request) = configured(Route::Generic);
    enqueue(&mut harness, request);
    for _ in 0..100 {
        pipeline
            .tick(|command| {
                let generic_launch_result = matches!(command, Command::TakeResult { .. })
                    && harness
                        .commands
                        .last()
                        .is_some_and(|(service, payload, _)| {
                            service == "example.provider/1" && payload["action"] == "launch"
                        });
                let mut response = harness.send(command)?;
                if generic_launch_result && let Response::Result { ref mut payload } = response {
                    payload["state"] = json!("starting");
                }
                Ok(response)
            })
            .unwrap();
        if harness.output.is_some() {
            break;
        }
    }
    assert_eq!(
        harness.output.as_ref().unwrap()["launch"]["state"],
        "starting"
    );
    assert!(
        !harness
            .commands
            .iter()
            .any(|(service, payload, _)| service == "example.provider/1"
                && payload["action"] == "lookup")
    );
    assert!(pipeline.job.is_some());
    let stopped = harness.request_as(
        &mut pipeline,
        "amaterasu.nvhttp",
        "management",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert_eq!(effect_count(&harness, Route::Generic), 1);
}

#[test]
fn lost_active_completion_retains_launch_until_owner_stops_it() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload: launch("active-completion-loss"),
        remaining_ms: 30_000,
    });
    let mut lost = false;
    for _ in 0..100 {
        let result = pipeline.tick(|command| {
            if let Command::Complete { ref payload, .. } = command
                && payload["launch"]["state"] == "active"
                && !lost
            {
                lost = true;
                assert!(matches!(harness.send(command), Ok(Response::Ok)));
                return Err("injected lost completion response".into());
            }
            harness.send(command)
        });
        if result.is_err() && !lost {
            panic!("unexpected transport failure");
        }
        if lost {
            break;
        }
    }
    assert!(lost);
    assert!(pipeline.records.values[&1].state == State::Active);
    assert!(harness.sessions[0].lease.is_some());
    assert!(harness.processes[0].child.is_some());
    let stopped = harness.request(
        &mut pipeline,
        "alice",
        json!({"action":"stop","launch_id":1}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert_eq!(effect_count(&harness, Route::Executable), 1);
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.processes[0].child.is_none());
}

#[test]
fn explicitly_rejected_active_completion_retires_launch() {
    let mut pipeline = pipeline();
    let mut harness = Harness::new();
    harness.incoming = Some(Response::Request {
        id: 7,
        caller: "alice".into(),
        initiator: "alice".into(),
        service: SERVICE.into(),
        payload: launch("active-completion-rejected"),
        remaining_ms: 30_000,
    });
    let mut rejected = false;
    for _ in 0..100 {
        pipeline
            .tick(|command| {
                if let Command::Complete { ref payload, .. } = command
                    && payload["launch"]["state"] == "active"
                    && !rejected
                {
                    rejected = true;
                    return Ok(Response::error("invalid_request", "rejected"));
                }
                harness.send(command)
            })
            .unwrap();
        if rejected && pipeline.job.is_none() {
            break;
        }
    }
    assert!(rejected);
    assert!(pipeline.records.values[&1].state == State::Cancelled);
    assert!(pipeline.records.values[&1].native_retirement_confirmed);
    assert!(harness.sessions[0].lease.is_none());
    assert!(harness.processes[0].child.is_none());
    assert_eq!(effect_count(&harness, Route::Executable), 1);
}
