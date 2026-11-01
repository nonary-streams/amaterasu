//! Certificate-backed, device-free feedback lifecycle qualification.
use super::*;

pub(super) struct Context<'a> {
    pub runtime: &'a Runtime,
    pub console: &'a RuntimeClient,
    pub pairing: &'a mut LoadedModule,
    pub memory: &'a mut LoadedModule,
    pub host_configuration: &'a mut Option<LoadedModule>,
    pub launch: &'a mut LoadedModule,
    pub lifecycle: &'a mut LoadedModule,
    pub rtsp: &'a mut LoadedModule,
    pub selected_session: &'a mut Option<LoadedModule>,
    pub selected_stream: &'a mut Option<LoadedModule>,
    pub selected_control: &'a mut Option<LoadedModule>,
    pub selected_media: &'a mut Option<LoadedModule>,
    pub selected_input: &'a mut Option<LoadedModule>,
    pub library: &'a mut Option<LoadedModule>,
    pub policy: &'a mut Option<LoadedModule>,
    pub generated: &'a mut Option<LoadedModule>,
    pub provider: &'a mut Option<LoadedModule>,
    pub video_fixture: &'a mut Option<fixture_video::Video>,
    pub configuration: &'a RuntimeClient,
    pub capture_fixture: &'a RuntimeClient,
    pub video_runtime: &'a RuntimeClient,
    pub session: &'a RuntimeClient,
    pub stream: &'a RuntimeClient,
    pub input: &'a RuntimeClient,
    pub application: &'a RuntimeClient,
    pub display: &'a RuntimeClient,
    pub cert_id: &'a str,
    pub cert: &'a [u8],
    pub second_cert: &'a [u8],
    pub second_cert_id: &'a str,
    pub second_pem: &'a str,
    pub first_key: &'a str,
    pub second_key: &'a str,
    pub previous: Option<&'a amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>,
}

fn load_stream(
    runtime: &Runtime,
    observed: std::sync::Arc<
        std::sync::Mutex<Option<amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>>,
    >,
) -> LoadedModule {
    let artifacts = PathBuf::from(std::env::var_os("AMATERASU_COMPOSITION_ARTIFACT_DIR").unwrap());
    LoadedModule::start_observed(
        &artifact(&artifacts, "amaterasu_stream_session"),
        runtime.endpoint("stream"),
        observed,
    )
}

pub(super) fn run(context: Context<'_>) -> amaterasu_module_sdk::nvhttp_source::BindFeedbackSource {
    let Context {
        runtime,
        console,
        pairing,
        memory,
        host_configuration,
        launch,
        lifecycle,
        rtsp,
        selected_session,
        selected_stream,
        selected_control,
        selected_media,
        selected_input,
        library,
        policy,
        generated,
        provider,
        video_fixture,
        configuration,
        capture_fixture,
        video_runtime,
        session,
        stream,
        input,
        application,
        display,
        cert_id,
        cert,
        second_cert,
        second_cert_id,
        second_pem,
        first_key,
        second_key,
        previous,
    } = context;
    let owner = runtime.client("stream");
    provision_authority(
        pairing,
        memory,
        host_configuration,
        console,
        &owner,
        cert_id,
        second_cert_id,
    );
    let artifacts = PathBuf::from(std::env::var_os("AMATERASU_COMPOSITION_ARTIFACT_DIR").unwrap());
    let mut module = LoadedModule::start(
        &artifact(&artifacts, "amaterasu_nvhttp"),
        &runtime.endpoint("amaterasu.nvhttp"),
    );
    let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
    *selected_stream = Some(load_stream(runtime, observed.clone()));
    let address = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        QUALIFICATION_PORT.load(std::sync::atomic::Ordering::Relaxed) + 5,
    ));
    let mut status = None;
    let mut status_sequence = 0u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        pump_input_configuration(configuration);
        if status.is_none() {
            status = Some(broker_request(
                &owner,
                "amaterasu.nvhttp/1",
                json!({"action":"status"}),
                &format!("normal-nvhttp-status-{status_sequence}"),
            ));
            status_sequence += 1;
        }
        if let Some(result) = take(&owner, status.unwrap()) {
            status = None;
            assert_eq!(result["https_address"], address.to_string());
            if result["tls_ready"] == true && result["feedback_source_ready"] == true {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "NVHTTP status timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let (_, host_der) = pem_rfc7468::decode_vec(HOST_CERT.as_bytes()).unwrap();
    let target =
        "/launch?appid=1&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17";
    let mut client = authenticated_tls::ModuleConnection::connect_with_key(
        address, &host_der, cert, first_key, target,
    );
    let mut backends = Hardware {
        real_input: true,
        real_stream: true,
        real_session: true,
        ..Hardware::default()
    };
    let mut response = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        client.tick();
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        launch.tick();
        lifecycle.tick();
        rtsp.tick();
        selected_session.as_mut().unwrap().tick();
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        selected_media.as_mut().unwrap().tick();
        selected_input.as_mut().unwrap().tick();
        pump_input_configuration(configuration);
        fixture_capture::pump(capture_fixture);
        video_fixture.as_mut().unwrap().pump(video_runtime);
        if let Some(provider) = &mut *library {
            provider.tick();
        }
        if let Some(provider) = &mut *policy {
            provider.tick();
        }
        if let Some(provider) = &mut *generated {
            provider.tick();
        }
        if let Some(provider) = &mut *provider {
            provider.tick();
        }
        backends.pump(session, stream, input, application, display);
        client.tick();
        client.read_plaintext(&mut response);
        if response.windows(7).any(|bytes| bytes == b"</root>") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "NVHTTP TLS launch timed out: {}",
            String::from_utf8_lossy(&response)
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let response = String::from_utf8(response).unwrap();
    let url = response
        .split_once("<sessionUrl0>")
        .and_then(|(_, tail)| tail.split_once("</sessionUrl0>"))
        .map(|(url, _)| url.to_owned())
        .unwrap_or_else(|| panic!("launch rejected: {response}"));
    let mut control_client = rtsp_client::play(&url, || {
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        launch.tick();
        lifecycle.tick();
        rtsp.tick();
        selected_session.as_mut().unwrap().tick();
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        selected_media.as_mut().unwrap().tick();
        selected_input.as_mut().unwrap().tick();
        pump_input_configuration(configuration);
        fixture_capture::pump(capture_fixture);
        video_fixture.as_mut().unwrap().pump(video_runtime);
        if let Some(provider) = &mut *library {
            provider.tick();
        }
        if let Some(provider) = &mut *policy {
            provider.tick();
        }
        if let Some(provider) = &mut *generated {
            provider.tick();
        }
        if let Some(provider) = &mut *provider {
            provider.tick();
        }
        backends.pump(session, stream, input, application, display);
    });
    let rtsp_owner = runtime.client("amaterasu.rtsp");
    let mut last = Value::Null;
    for n in 0..300 {
        for _ in 0..30 {
            module.tick();
            super::tick_authority_modules(pairing, memory, host_configuration);
            lifecycle.tick();
            selected_session.as_mut().unwrap().tick();
            selected_stream.as_mut().unwrap().tick();
            selected_control.as_mut().unwrap().tick();
            selected_media.as_mut().unwrap().tick();
            selected_input.as_mut().unwrap().tick();
            pump_input_configuration(configuration);
            fixture_capture::pump(capture_fixture);
            video_fixture.as_mut().unwrap().pump(video_runtime);
            backends.pump(session, stream, input, application, display);
        }
        let pending = broker_request(
            &rtsp_owner,
            "amaterasu.stream-session/1",
            json!({"action":"status"}),
            &format!("full-active-{n}"),
        );
        selected_stream.as_mut().unwrap().tick();
        if let Some(value) = take(&rtsp_owner, pending) {
            last = value;
        }
        if last["sessions"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["state"] == "active" && item["input_active"] == true)
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        last["sessions"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["state"] == "active" && item["input_active"] == true)),
        "full stream did not activate: {last}"
    );
    control_client.enable_haptics();
    control_client.send_controller_arrival();
    let mut received = None;
    let mut authority = Value::Null;
    let mut authority_pending = None;
    let mut observed_join = None;
    for n in 0..150 {
        if n % 5 == 0 {
            control_client.keep_alive();
        }
        for _ in 0..20 {
            module.tick();
            if let Ok(last) = observed.lock() {
                observed_join = last.clone();
            }
            super::tick_authority_modules(pairing, memory, host_configuration);
            lifecycle.tick();
            selected_session.as_mut().unwrap().tick();
            selected_stream.as_mut().unwrap().tick();
            selected_control.as_mut().unwrap().tick();
            selected_media.as_mut().unwrap().tick();
            selected_input.as_mut().unwrap().tick();
            pump_input_configuration(configuration);
            fixture_capture::pump(capture_fixture);
            video_fixture.as_mut().unwrap().pump(video_runtime);
            backends.pump(session, stream, input, application, display);
            if let Some(message) = control_client.receive_feedback()
                && message.kind == 0x5502
            {
                received = Some(message.payload);
            }
        }
        if authority_pending.is_none() {
            authority_pending = Some(broker_request(
                &owner,
                "amaterasu.pairing-effects/1",
                json!({"action":"status"}),
                &format!("full-authority-{n}"),
            ));
        }
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(&owner, authority_pending.unwrap()) {
            authority = value;
            authority_pending = None;
        }
        if received.is_some() && authority["bound_sources"] == 1 && authority["in_flight"] == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        authority["bound_sources"], 1,
        "source was not bound: {authority}"
    );
    assert_eq!(
        authority["in_flight"], 1,
        "stream lease missing: {authority}"
    );
    let expected = if std::env::var_os("AMATERASU_INPUT_QUALIFICATION_ALTERNATE").is_some() {
        vec![0, 0, 4, 5, 6]
    } else {
        vec![0, 0, 1, 2, 3]
    };
    assert_eq!(
        received,
        Some(expected),
        "selected input backend feedback was not delivered"
    );
    let observed_join = observed_join.expect("real source join was observed");
    assert_eq!(observed_join.scope.controller_slot, 0);
    assert_ne!(cert, second_cert);
    let second_target =
        "/launch?appid=1&mode=1280x720x60&rikey=10112233445566778899aabbccddeeff&rikeyid=18";
    let mut second = authenticated_tls::ModuleConnection::connect_with_key(
        address,
        &host_der,
        second_cert,
        second_key,
        second_target,
    );
    let mut second_response = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        second.tick();
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        launch.tick();
        lifecycle.tick();
        rtsp.tick();
        selected_session.as_mut().unwrap().tick();
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        selected_media.as_mut().unwrap().tick();
        selected_input.as_mut().unwrap().tick();
        pump_input_configuration(configuration);
        fixture_capture::pump(capture_fixture);
        video_fixture.as_mut().unwrap().pump(video_runtime);
        backends.pump(session, stream, input, application, display);
        if let Some(p) = &mut *library {
            p.tick();
        }
        if let Some(p) = &mut *policy {
            p.tick();
        }
        if let Some(p) = &mut *generated {
            p.tick();
        }
        if let Some(p) = &mut *provider {
            p.tick();
        }
        second.tick();
        second.read_plaintext(&mut second_response);
        if second_response.windows(7).any(|bytes| bytes == b"</root>") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "second TLS client timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let second_response = String::from_utf8(second_response).unwrap();
    assert!(
        second_response.contains("status_code=\"503\"")
            && second_response.contains("another stream is active"),
        "{second_response}"
    );
    // Negative service-boundary test: the second certificate completed TLS and
    // has the same feedback grant, but Pairing must retain source ownership.
    let protocol = runtime.client("amaterasu.nvhttp");
    let crossed = broker_request(
        &protocol,
        "amaterasu.pairing-effects/1",
        json!({"action":"bind_stream","epoch":observed_join.authority_epoch,"certificate_pem":second_pem,"source":observed_join.source,"scope":observed_join.scope}),
        "cross-certificate-source",
    );
    let mut crossed_response = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(&protocol, crossed) {
            crossed_response = Some(value);
            break;
        }
    }
    assert_eq!(
        crossed_response.as_ref().map(|v| &v["code"]),
        Some(&json!("source_owner_conflict")),
        "{crossed_response:?}"
    );
    let attacker = runtime.client("attacker");
    let denied = broker_request(
        &attacker,
        "amaterasu.nvhttp/1",
        observed_join.payload(),
        "source-foreign-owner",
    );
    let mut denied_response = None;
    for _ in 0..500 {
        module.tick();
        if let Some(value) = take(&attacker, denied) {
            denied_response = Some(value);
            break;
        }
    }
    assert_eq!(
        denied_response.as_ref().map(|v| &v["code"]),
        Some(&json!("permission_denied")),
        "{denied_response:?}"
    );
    for (name, forged) in [
        ("stale-generation", {
            let mut request = observed_join.clone();
            request.source.generation += 1;
            request.source.stream_generation += 1;
            request
        }),
        ("foreign-source", {
            let mut request = observed_join.clone();
            request.source.id = "f".repeat(32);
            request
        }),
        ("foreign-boot", {
            let mut request = observed_join.clone();
            request.input_boot_epoch = "e".repeat(32);
            request
        }),
    ] {
        let pending = broker_request(
            &owner,
            "amaterasu.nvhttp/1",
            forged.payload(),
            &format!("source-deny-{name}"),
        );
        let mut result = None;
        for _ in 0..1000 {
            module.tick();
            lifecycle.tick();
            super::tick_authority_modules(pairing, memory, host_configuration);
            if let Some(value) = take(&owner, pending) {
                result = Some(value);
                break;
            }
        }
        assert_eq!(
            result.as_ref().map(|v| &v["code"]),
            Some(&json!("source_denied")),
            "{name}: {result:?}"
        );
    }
    if let Some(previous) = previous {
        assert_eq!(
            previous.scope.controller_slot,
            observed_join.scope.controller_slot
        );
        assert_ne!(previous.source.id, observed_join.source.id);
        assert_ne!(previous.source.session_id, observed_join.source.session_id);
        let mut crossed = previous.clone();
        crossed.authority_epoch = observed_join.authority_epoch.clone();
        crossed.source.effect_owner_epoch = observed_join.source.effect_owner_epoch.clone();
        crossed.scope = observed_join.scope.clone();
        crossed.input_boot_epoch = observed_join.input_boot_epoch.clone();
        let pending = broker_request(
            &owner,
            "amaterasu.nvhttp/1",
            crossed.payload(),
            "cross-client-prior-source",
        );
        let mut denied = None;
        for _ in 0..1000 {
            module.tick();
            lifecycle.tick();
            super::tick_authority_modules(pairing, memory, host_configuration);
            if let Some(value) = take(&owner, pending) {
                denied = Some(value);
                break;
            }
        }
        assert_eq!(
            denied.as_ref().map(|v| &v["code"]),
            Some(&json!("source_denied")),
            "cross-client source was accepted: {denied:?}"
        );
    }
    let old_epoch = authority["epoch"].as_str().unwrap().to_owned();
    let old_owner = authority["owner_epoch"].as_str().unwrap().to_owned();
    let control_query = broker_request(
        stream,
        "amaterasu.control-transport/1",
        json!({"action":"status"}),
        "full-control-before-revoke",
    );
    let mut control_status = None;
    for _ in 0..500 {
        selected_control.as_mut().unwrap().tick();
        if let Some(value) = take(stream, control_query) {
            control_status = Some(value);
            break;
        }
    }
    let control_status = control_status.expect("control status");
    let control_item = control_status["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["authenticated"] == true)
        .expect("authenticated stream");
    let control_id = control_item["stream"].as_u64().unwrap();
    let control_epoch = control_item["retirement_epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let revision = authority["revisions"]["trust"].as_u64().unwrap();
    let revoke = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"update_client_permissions","certificate_id":cert_id,"expected_revision":revision,"permissions":{"list_apps":true,"view_streams":false,"launch_apps":true,"terminate_streams":true}}),
        "full-revoke-view",
    );
    let mut fence = None;
    for n in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        selected_stream.as_mut().unwrap().tick();
        selected_input.as_mut().unwrap().tick();
        lifecycle.tick();
        fixture_capture::pump(capture_fixture);
        if n % 10 == 0 {
            let query = broker_request(
                &owner,
                "amaterasu.pairing-effects/1",
                json!({"action":"status"}),
                &format!("full-fence-{n}"),
            );
            for _ in 0..30 {
                super::tick_authority_modules(pairing, memory, host_configuration);
                if let Some(value) = take(&owner, query) {
                    if value["fenced"] == true {
                        fence = Some(value);
                    }
                    break;
                }
            }
        }
        if fence.is_some() {
            break;
        }
    }
    let fence = fence.expect("pairing did not fence the source");
    assert_eq!(fence["drain_required"], true, "{fence}");
    assert!(
        matches!(
            console
                .send(Command::TakeResult { request: revoke })
                .unwrap(),
            Response::Pending
        ),
        "revocation completed before transport retirement"
    );
    let mut revoked = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        launch.tick();
        lifecycle.tick();
        rtsp.tick();
        selected_session.as_mut().unwrap().tick();
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        selected_media.as_mut().unwrap().tick();
        selected_input.as_mut().unwrap().tick();
        pump_input_configuration(configuration);
        fixture_capture::pump(capture_fixture);
        video_fixture.as_mut().unwrap().pump(video_runtime);
        backends.pump(session, stream, input, application, display);
        if let Some(value) = take(console, revoke) {
            revoked = Some(value);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        revoked.as_ref().map(|v| &v["ok"]),
        Some(&json!(true)),
        "revocation did not settle: {revoked:?}"
    );
    let retired = broker_request(
        stream,
        "amaterasu.control-transport/1",
        json!({"action":"retirement_status","stream":control_id,"operation_id":format!("feedback-retire-{control_epoch}-{control_id}"),"provider_epoch":control_epoch}),
        "full-retired-control",
    );
    let mut receipt = None;
    for _ in 0..500 {
        selected_control.as_mut().unwrap().tick();
        if let Some(value) = take(stream, retired) {
            receipt = Some(value);
            break;
        }
    }
    let receipt = receipt.expect("control retirement receipt");
    assert_eq!(receipt["state"], "retired", "{receipt}");
    assert_eq!(receipt["scope"], "whole_control_stream", "{receipt}");
    while control_client.receive_feedback().is_some() {}
    let quiet = std::time::Instant::now() + std::time::Duration::from_millis(600);
    while std::time::Instant::now() < quiet {
        selected_control.as_mut().unwrap().tick();
        let mut packet = [0u8; 4096];
        match control_client.control.recv(&mut packet) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(size) => {
                panic!("control datagram received after retirement acknowledgement: {size} bytes")
            }
            Err(error) => panic!("control receive after retirement: {error}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    drop(selected_stream.take());
    *selected_stream = Some(load_stream(runtime, observed.clone()));
    let mut restart_status = Value::Null;
    let mut pending = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        module.tick();
        super::tick_authority_modules(pairing, memory, host_configuration);
        selected_stream.as_mut().unwrap().tick();
        selected_control.as_mut().unwrap().tick();
        if pending.is_none() {
            pending = Some(broker_request(
                &owner,
                "amaterasu.pairing-effects/1",
                json!({"action":"status"}),
                "full-restart-status",
            ));
        }
        if let Some(value) = take(&owner, pending.unwrap()) {
            restart_status = value;
            pending = None;
        }
        if restart_status["epoch"] != old_epoch
            && restart_status["owner_epoch"] != old_owner
            && restart_status["fenced"] == false
            && restart_status["in_flight"] == 0
            && restart_status["bound_sources"] == 0
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_ne!(
        restart_status["epoch"], old_epoch,
        "owner restart epoch did not rotate: {restart_status}"
    );
    assert_ne!(
        restart_status["owner_epoch"], old_owner,
        "owner incarnation did not rotate: {restart_status}"
    );
    assert_eq!(restart_status["fenced"], false, "{restart_status}");
    assert_eq!(restart_status["in_flight"], 0, "{restart_status}");
    assert_eq!(restart_status["bound_sources"], 0, "{restart_status}");
    observed_join
}

/// Configure only this fresh test authority through its real administrator API.
fn provision_authority(
    pairing: &mut LoadedModule,
    memory: &mut LoadedModule,
    host_configuration: &mut Option<LoadedModule>,
    console: &RuntimeClient,
    owner: &RuntimeClient,
    cert_id: &str,
    second_cert_id: &str,
) {
    let list = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"list_clients"}),
        "full-list-clients",
    );
    let mut listed = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, list) {
            listed = Some(value);
            break;
        }
    }
    let listed = listed.expect("client list");
    assert_eq!(listed["ok"], true, "{listed}");
    let revision = listed["revision"].as_u64().expect("pairing revision");
    let grants = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"update_client_feedback_grants","certificate_id":cert_id,"expected_revision":revision,"grants":[{"schema":"org.amaterasu.feedback.rgb_led","version":1,"direction":"to_client"}]}),
        "full-grant-feedback",
    );
    let mut granted = None;
    for _ in 0..1000 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, grants) {
            granted = Some(value);
            break;
        }
    }
    assert_eq!(
        granted.as_ref().map(|v| &v["ok"]),
        Some(&json!(true)),
        "{granted:?}"
    );
    let list = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"list_clients"}),
        "full-list-second-grant",
    );
    let mut listed = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, list) {
            listed = Some(value);
            break;
        }
    }
    let listed = listed.expect("second grant revision");
    let grant = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"update_client_feedback_grants","certificate_id":second_cert_id,"expected_revision":listed["revision"],"grants":[{"schema":"org.amaterasu.feedback.rgb_led","version":1,"direction":"to_client"}]}),
        "full-grant-second-feedback",
    );
    let mut result = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, grant) {
            result = Some(value);
            break;
        }
    }
    assert_eq!(
        result.as_ref().map(|v| &v["ok"]),
        Some(&json!(true)),
        "{result:?}"
    );
    let list = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"list_clients"}),
        "full-list-after-grants",
    );
    let mut listed = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, list) {
            listed = Some(value);
            break;
        }
    }
    let listed = listed.expect("post-grant list");
    let update = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"update_client_config_overrides","certificate_id":cert_id,"expected_revision":listed["revision"],"config_overrides":{"controller":true}}),
        "full-enable-controller",
    );
    let mut updated = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, update) {
            updated = Some(value);
            break;
        }
    }
    assert_eq!(
        updated.as_ref().map(|v| &v["ok"]),
        Some(&json!(true)),
        "{updated:?}"
    );
    let list = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"list_clients"}),
        "full-list-before-mode",
    );
    let mut listed = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, list) {
            listed = Some(value);
            break;
        }
    }
    let listed = listed.expect("pre-mode list");
    let mode = broker_request(
        console,
        "amaterasu.pairing/1",
        json!({"action":"set_permission_mode","enabled":true,"expected_revision":listed["revision"]}),
        "full-enable-permission-mode",
    );
    let mut changed = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(console, mode) {
            changed = Some(value);
            break;
        }
    }
    assert_eq!(
        changed.as_ref().map(|v| &v["ok"]),
        Some(&json!(true)),
        "{changed:?}"
    );
    let query = broker_request(
        owner,
        "amaterasu.pairing-effects/1",
        json!({"action":"status"}),
        "after-provision-status",
    );
    let mut status = None;
    for _ in 0..500 {
        super::tick_authority_modules(pairing, memory, host_configuration);
        if let Some(value) = take(owner, query) {
            status = Some(value);
            break;
        }
    }
    let status = status.expect("post-provision authority status");
    assert_eq!(status["bound_sources"], 0, "{status}");
    if status["fenced"] == true {
        assert_eq!(status["drain_required"], false, "{status}");
        let request = broker_request(
            owner,
            "amaterasu.pairing-effects/1",
            json!({"action":"recover","epoch":status["epoch"],"owner_epoch":"provisioned-fixture"}),
            "after-provision-recover",
        );
        let mut result = None;
        for _ in 0..500 {
            super::tick_authority_modules(pairing, memory, host_configuration);
            if let Some(value) = take(owner, request) {
                result = Some(value);
                break;
            }
        }
        assert_eq!(
            result.as_ref().map(|v| &v["ok"]),
            Some(&json!(true)),
            "{result:?}"
        );
    }
}
