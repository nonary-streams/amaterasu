use amaterasu_module_sdk::session_presentation::{
    AuxiliaryStreamPlan, CaptureSource, Client, Mode, SessionEvent,
};

fn auxiliary_plan(key: u8, capture: CaptureSource) -> AuxiliaryStreamPlan {
    AuxiliaryStreamPlan {
        session_id: "retained-source".into(),
        generation: 7,
        reservation_id: format!("reservation-{key}"),
        mode: Mode { width: 1280, height: 720, refresh_millihz: 60_000, hdr: false },
        stream_audio: !matches!(capture, CaptureSource::SyntheticBlack),
        host_audio: !matches!(capture, CaptureSource::SyntheticBlack),
        capture,
    }
}

fn auxiliary_controller() -> Controller {
    let mut controller = Controller::new(Some(config())).unwrap();
    controller.set_borrowed_capture_verification(true);
    controller.set_auxiliary_capture_routes(
        Some(CaptureProvider::External("black".into())),
        Some(CaptureProvider::External("vkms".into())),
    );
    controller
}

fn presentation_send(broker: &mut Broker, command: Command) -> Transport {
    let response = match &command {
        Command::CallApi { service, method, payload, .. }
            if service == amaterasu_module_sdk::session_presentation::SERVICE =>
        {
            Some(match method.as_str() {
                "bind_transport" | "bind_application" => json!({"result":"acknowledged"}),
                "verify_source" => json!({"result":"verified", "source":payload["source"]}),
                _ => panic!("unexpected presentation method {method}"),
            })
        }
        Command::Request { service, payload, .. }
            if service == "amaterasu.rtsp/1" && payload["action"] == "retirement_status" =>
        {
            Some(json!({"ok":true,"runtime_epoch":"a".repeat(32),"startup_reconciled":true,
                "operation_id":null,"expected_runtime_epoch":null,"state":"ready","retired":false,
                "cleanup_pending":false,"pending_admissions":0,"active_operations":0}))
        }
        _ => None,
    };
    if let Some(response) = response {
        broker.commands.push(command);
        broker.next += 1;
        broker.results.insert(broker.next, response);
        Ok(Response::Accepted { id: broker.next })
    } else {
        if matches!(&command, Command::Request {service,payload,..} if service == "amaterasu.rtsp/1" && payload["action"] == "open") {
            broker.rtsp_closed = false;
        }
        broker.send(command)
    }
}

fn auxiliary_ready(controller: &mut Controller, broker: &mut Broker, now: Instant) -> (u64, Reply) {
    for _ in 0..128 {
        if let Some(reply) = controller.poll(now, &mut |command| presentation_send(broker, command)).into_iter().next() {
            return reply;
        }
    }
    panic!("auxiliary stream did not complete bounded preparation");
}

fn auxiliary_open(broker: &Broker) -> &Value {
    broker.commands.iter().rev().find_map(|command| match command {
        Command::Request {service,payload,..} if service == "amaterasu.rtsp/1" && payload["action"] == "open" => Some(payload),
        _ => None,
    }).expect("auxiliary RTSP open")
}

#[test]
fn auxiliary_sources_require_explicit_provider_selection() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let plan = auxiliary_plan(1, CaptureSource::SyntheticBlack);
    assert_eq!(controller.launch_auxiliary(1,request(1),peer(1),plan.clone(),now).unwrap().status,503);
    controller.set_borrowed_capture_verification(true);
    assert_eq!(controller.launch_auxiliary(1,request(1),peer(1),plan,now).unwrap().status,503);
    assert!(controller.records.is_empty());
}

#[test]
fn auxiliary_input_uses_black_source_without_application_display_or_audio_policy() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    controller.set_global_display_policy(9,json!({"dd_configuration_option":"ensure_only_display"}));
    controller.set_virtual_sink_settings(Some("virtual-managed-sink"));
    let mut broker = Broker::default();
    assert!(controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now).is_none());
    let (connection,reply) = auxiliary_ready(&mut controller,&mut broker,now);
    assert_eq!((connection,reply.status),(10,200));
    let open = auxiliary_open(&broker);
    assert_eq!(open["capture"]["capture_provider"],"black");
    assert_eq!(open["capture"]["output"],"black:1280x720@60:nv12");
    assert_eq!(open["stream_audio"],false);
    assert_eq!(open["capture"]["audio_endpoint"],"");
    assert_eq!(open["capture"]["host_audio_enabled"],false);
    assert!(open["virtual_sink"].is_null());
    assert!(open["virtual_display_intent"].is_null());
    assert!(open["display_policy_snapshot"].is_null());
    assert!(open["display_request"].is_null());
    assert_eq!(open["expected_runtime_epoch"],"a".repeat(32));
    assert!(!broker.actions().iter().any(|(service,_)| matches!(*service,"amaterasu.launch/1"|"amaterasu.display/1"|"amaterasu.virtual-display/1")));
    let bind = broker.commands.iter().position(|command| matches!(command,Command::CallApi{method,..} if method=="bind_transport")).unwrap();
    let open_index = broker.commands.iter().position(|command| matches!(command,Command::Request{service,payload,..} if service=="amaterasu.rtsp/1" && payload["action"]=="open")).unwrap();
    assert!(bind < open_index);
    assert!(controller.take_auxiliary_events().is_empty());
    controller.reply_delivered(10,now);
    let events = controller.take_auxiliary_events();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].event,SessionEvent::StreamStarted);
    assert_eq!(events[0].reservation_id,"reservation-1");
    assert_eq!(events[0].transport_epoch.as_deref(),Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(controller.take_auxiliary_events().is_empty());
}

#[test]
fn auxiliary_monitor_preserves_exact_source_and_host_playback() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    let mut plan = auxiliary_plan(1,CaptureSource::ExactOutput{output:"kwin:Virtual-7".into(),route_key:"default".into()});
    plan.stream_audio = false;
    assert!(controller.launch_auxiliary(10,request(1),peer(1),plan,now).is_none());
    assert_eq!(auxiliary_ready(&mut controller,&mut broker,now).1.status,200);
    let open = auxiliary_open(&broker);
    assert_eq!(open["capture"]["capture_provider"],"vkms");
    assert_eq!(open["capture"]["output"],"kwin:Virtual-7");
    assert_eq!(open["capture"]["host_audio_enabled"],true);
    assert_eq!(open["stream_audio"],false);
    assert!(open["virtual_display_intent"].is_null());
    assert!(open["lifecycle_session_target"].is_null());
}

#[test]
fn auxiliary_game_and_input_streams_have_independent_admission_and_stop() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    assert!(controller.launch(10,request(1),peer(1),now).is_none());
    assert_eq!(poll_until_reply(&mut controller,&mut broker,now).1.status,200);
    controller.reply_delivered(10,now);
    let game = controller.records.keys().copied().next().unwrap();
    controller.records.get_mut(&game).unwrap().next_monitor = now+Duration::from_secs(3600);
    let before = broker.commands.len();
    assert!(controller.launch_auxiliary(20,request(2),peer(2),auxiliary_plan(2,CaptureSource::SyntheticBlack),now).is_none());
    assert_eq!(auxiliary_ready(&mut controller,&mut broker,now).1.status,200);
    controller.reply_delivered(20,now);
    assert_eq!(controller.current_game(&peer(2)),1);
    assert_eq!(controller.records.len(),2);
    assert!(!broker.commands[before..].iter().any(|command| matches!(command,Command::Request{service,..} if service=="amaterasu.launch/1")));
    controller.stop_auxiliary_for_client("retained-source",7,&fingerprint(peer(2).certificate()),now).unwrap();
    assert!(!controller.auxiliary_stopped("retained-source",7).unwrap());
    for _ in 0..128 {
        controller.poll(now,&mut |command|presentation_send(&mut broker,command));
        if controller.auxiliary_stopped("retained-source",7).unwrap_or(false){break}
    }
    assert!(controller.auxiliary_stopped("retained-source",7).unwrap());
    assert_eq!(controller.records[&game].state,State::Ready);
    assert!(!broker.application_stopped);
}

#[test]
fn auxiliary_stops_are_client_and_generation_bound() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    assert!(controller.launch_auxiliary(1,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now).is_none());
    assert!(controller.stop_auxiliary_for_client("retained-source",7,&fingerprint(peer(2).certificate()),now).is_err());
    assert!(controller.stop_auxiliary("retained-source",6,now).is_err());
    assert_eq!(controller.records.values().next().unwrap().state,State::Starting);
}

#[test]
fn auxiliary_reconnect_retires_previous_transport_before_opening_another() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    assert!(controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now).is_none());
    assert_eq!(auxiliary_ready(&mut controller,&mut broker,now).1.status,200);
    controller.reply_delivered(10,now);
    let initial = controller.take_auxiliary_events().pop().unwrap();
    assert!(controller.launch_auxiliary(20,request(2),peer(1),auxiliary_plan(2,CaptureSource::SyntheticBlack),now).is_none());
    let first = controller.records.values().next().unwrap();
    assert_eq!(first.state,State::Stopping);
    assert_eq!(controller.records.len(),1);
    let (connection,reply) = auxiliary_ready(&mut controller,&mut broker,now);
    assert_eq!((connection,reply.status),(20,200));
    let ended = controller.take_auxiliary_events();
    assert!(ended.iter().any(|event|event.transport_id==initial.transport_id && event.event==SessionEvent::StreamEnded));
    let close = broker.commands.iter().position(|command| matches!(command,Command::Request{service,payload,..}if service=="amaterasu.rtsp/1"&&payload["action"]=="close")).unwrap();
    let last_open = broker.commands.iter().rposition(|command| matches!(command,Command::Request{service,payload,..}if service=="amaterasu.rtsp/1"&&payload["action"]=="open")).unwrap();
    assert!(close<last_open);
    assert_eq!(controller.records.values().filter(|record|!record.state.terminal()).count(),1);
}

#[test]
fn auxiliary_observation_never_claims_uncertain_native_cleanup_finished() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    assert!(controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now).is_none());
    auxiliary_ready(&mut controller,&mut broker,now);
    controller.reply_delivered(10,now);
    controller.take_auxiliary_events();
    let record = controller.records.values_mut().next().unwrap();
    record.state = State::CleanupFailed;
    record.rtsp_requested = true;
    assert!(controller.auxiliary_stopped("retained-source",7).is_err());
    assert!(controller.take_auxiliary_events().is_empty());
}

#[test]
fn auxiliary_presentation_reports_only_the_normal_application() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    assert!(controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now).is_none());
    auxiliary_ready(&mut controller,&mut broker,now);
    controller.reply_delivered(10,now);
    let context = controller.session_presentation_context(Client{id:fingerprint(peer(1).certificate()),paired:true,may_view:true,may_launch:true,may_terminate:true});
    assert!(context.game.is_none());
    assert_eq!(controller.current_game(&peer(1)),0);
}

fn borrowed_send(broker: &mut SharedSessionBroker, command: Command) -> Transport {
    if matches!(&command,Command::CallApi {..})
        || matches!(&command,Command::Request {service,payload,..}if service=="amaterasu.rtsp/1"&&payload["action"]=="retirement_status")
    {
        return presentation_send(&mut broker.broker,command);
    }
    let response = broker.send(command.clone())?;
    if let (Command::Request{service,payload,..},Response::Accepted{id})=(&command,&response) {
        if service=="amaterasu.session-lifecycle/1"&&payload["operation"]=="prepare" {
            broker.broker.results.get_mut(id).unwrap()["capture_binding_mode"]=json!("external");
        }
        if service=="amaterasu.rtsp/1"&&payload["action"]=="open" {
            broker.broker.results.get_mut(id).unwrap()["borrowed_capture"]=payload["borrowed_capture"].clone();
            let ticket=broker.broker.results[id]["ticket"].as_u64().unwrap();
            broker.transports.get_mut(&ticket).unwrap()["borrowed_capture"]=payload["borrowed_capture"].clone();
        }
    }
    Ok(response)
}

fn borrowed_ready(controller:&mut Controller,broker:&mut SharedSessionBroker,now:Instant)->(u64,Reply) {
    for tick in 0..192 {
        if let Some(reply)=controller.poll(now+Duration::from_millis(tick),&mut|command|borrowed_send(broker,command)).into_iter().next(){return reply;}
    }
    panic!("borrowed source did not complete bounded preparation");
}

fn borrowed_source(reservation:&str)->amaterasu_module_sdk::session_presentation::SharedCapture {
    amaterasu_module_sdk::session_presentation::SharedCapture {
        session_id:"retained-source".into(),generation:7,reservation_id:reservation.into(),
        capture:CaptureSource::ExactOutput{output:"kwin:Virtual-7".into(),route_key:"default".into()},
        mode:Mode{width:2560,height:1440,refresh_millihz:60_000,hdr:false},
    }
}

#[test]
fn auxiliary_borrowed_application_verifies_and_binds_source_before_required_lifecycle() {
    let now=Instant::now();
    let mut controller=auxiliary_controller();
    controller.set_session_lifecycle_required(true);
    controller.set_session_lifecycle_capture_mode(amaterasu_module_sdk::extension_api::CaptureBindingMode::External);
    controller.set_global_display_policy(7,json!({"dd_configuration_option":"ensure_only_display"}));
    let source=borrowed_source("original-application-source");
    let mut broker=SharedSessionBroker::default();
    assert!(controller.launch_application_with_source(10,request(1),peer(1),source.clone(),now).is_none());
    let(connection,reply)=borrowed_ready(&mut controller,&mut broker,now);
    assert_eq!((connection,reply.status),(10,200));
    let commands=&broker.broker.commands;
    let verify=commands.iter().position(|command|matches!(command,Command::CallApi{method,..}if method=="verify_source")).unwrap();
    let bind=commands.iter().position(|command|matches!(command,Command::CallApi{method,..}if method=="bind_application")).unwrap();
    let prepare=commands.iter().position(|command|matches!(command,Command::Request{service,payload,..}if service=="amaterasu.session-lifecycle/1"&&payload["operation"]=="prepare")).unwrap();
    let launch=commands.iter().position(|command|matches!(command,Command::Request{service,payload,..}if service=="amaterasu.launch/1"&&payload["action"]=="launch_export")).unwrap();
    assert!(verify<bind&&bind<prepare&&prepare<launch);
    let prepare_payload=match &commands[prepare]{Command::Request{payload,..}=>payload,_=>unreachable!()};
    assert_eq!(prepare_payload["borrowed_capture"]["mode"]["width"],2560);
    assert_eq!(prepare_payload["borrowed_capture"]["transport_epoch"],"a".repeat(32));
    let open=auxiliary_open(&broker.broker);
    assert_eq!(open["capture"]["output"],"kwin:Virtual-7");
    assert_eq!(open["capture"]["capture_provider"],"vkms");
    assert!(open.get("display_policy_snapshot").is_none());
    assert!(open.get("virtual_display_intent").is_none());
    assert!(!broker.broker.actions().iter().any(|(service,_)|matches!(*service,"amaterasu.display/1"|"amaterasu.virtual-display/1")));
    controller.reply_delivered(10,now);
    let events=controller.take_auxiliary_events();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].event,SessionEvent::StreamStarted);
    let root=*controller.records.keys().next().unwrap();
    let initial_operation=controller.records[&root].application_operation.clone();
    assert!(initial_operation.starts_with("ale1:"));
    let context=controller.session_presentation_context(Client{id:fingerprint(peer(1).certificate()),paired:true,may_view:true,may_launch:true,may_terminate:true});
    assert_eq!(context.game.unwrap().source,Some(source));
    broker.transports.values_mut().for_each(|transport|transport["state"]=json!("closed"));
    for tick in 0..192 {
        controller.poll(now+Duration::from_millis(tick*10),&mut|command|borrowed_send(&mut broker,command));
        if controller.records[&root].state==State::Paused{break;}
    }
    assert_eq!(controller.records[&root].state,State::Paused);
    assert!(controller.take_auxiliary_events().is_empty(),"the application still owns its original source while paused");
    assert!(controller.owns_application_operation(&initial_operation,"0123456789abcdef0123456789abcdef"));
}

#[test]
fn auxiliary_unknown_route_is_rejected_before_any_stream_admission() {
    let now=Instant::now();
    let mut controller=auxiliary_controller();
    let plan=auxiliary_plan(1,CaptureSource::ExactOutput{output:"kwin:DP-1".into(),route_key:"unapproved_route".into()});
    assert_eq!(controller.launch_auxiliary(10,request(1),peer(1),plan,now).unwrap().status,503);
    assert!(controller.records.is_empty());
}

#[test]
fn auxiliary_authenticated_disconnect_is_observed_only_after_native_retirement() {
    let now=Instant::now();
    let mut controller=auxiliary_controller();
    let mut broker=Broker::default();
    controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now);
    auxiliary_ready(&mut controller,&mut broker,now);
    controller.reply_delivered(10,now);
    controller.take_auxiliary_events();
    let record=controller.records.values_mut().next().unwrap();
    record.authenticated_client_disconnected=true;
    record.state=State::Stopping;
    assert!(controller.take_auxiliary_events().is_empty());
    let record=controller.records.values_mut().next().unwrap();
    record.rtsp_requested=false;
    record.state=State::Stopped;
    let events=controller.take_auxiliary_events();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].event,SessionEvent::ClientDisconnected);
    assert!(controller.take_auxiliary_events().is_empty());
}

#[test]
fn auxiliary_scheduling_services_a_new_stream_while_another_is_due() {
    let now=Instant::now();
    let mut controller=auxiliary_controller();
    let mut broker=Broker::default();
    controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now);
    auxiliary_ready(&mut controller,&mut broker,now);
    controller.reply_delivered(10,now);
    let existing=*controller.records.keys().next().unwrap();
    let mut second=auxiliary_plan(2,CaptureSource::SyntheticBlack);
    second.session_id="second-source".into();
    controller.launch_auxiliary(20,request(2),peer(2),second,now);
    let added=*controller.records.keys().last().unwrap();
    controller.records.get_mut(&existing).unwrap().next_monitor=now;
    controller.last_scheduled_record=existing;
    controller.job=None;
    controller.schedule(now);
    assert_eq!(controller.job.as_ref().unwrap().record,added);
    assert_eq!(controller.job.as_ref().unwrap().phase,workflow::Phase::ReadRtspRuntimeEpoch);
}

#[test]
fn auxiliary_recovery_guard_keeps_live_and_uncertain_transport_operations() {
    let now=Instant::now();
    let mut controller=auxiliary_controller();
    let mut broker=Broker::default();
    controller.launch_auxiliary(10,request(1),peer(1),auxiliary_plan(1,CaptureSource::SyntheticBlack),now);
    auxiliary_ready(&mut controller,&mut broker,now);
    let record=controller.records.values_mut().next().unwrap();
    record.state=State::CleanupFailed;
    let operation=record.rtsp_operation.clone();
    assert!(controller.owns_transport_operation(&operation,"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(!controller.owns_transport_operation(&operation,"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
}

#[test]
fn auxiliary_retired_records_do_not_exhaust_future_admissions() {
    let now = Instant::now();
    let mut controller = auxiliary_controller();
    let mut broker = Broker::default();
    for key in 1..=(MAX_RECORDS as u8 + 1) {
        controller.launch_auxiliary(
            key as u64,
            request(key),
            peer(1),
            auxiliary_plan(key, CaptureSource::SyntheticBlack),
            now,
        );
        assert_eq!(auxiliary_ready(&mut controller, &mut broker, now).1.status, 200);
        controller.reply_delivered(key as u64, now);
        controller.take_auxiliary_events();
        controller.stop_auxiliary("retained-source", 7, now).unwrap();
        for _ in 0..128 {
            controller.poll(now, &mut |command| presentation_send(&mut broker, command));
            if controller.auxiliary_stopped("retained-source", 7).unwrap_or(false) { break; }
        }
        assert!(controller.auxiliary_stopped("retained-source", 7).unwrap());
        controller.take_auxiliary_events();
        assert!(controller.records.len() <= MAX_RECORDS);
    }
    assert_eq!(controller.retired_stream_keys.len(), 1);
}
