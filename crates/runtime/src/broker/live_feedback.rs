//! Startup checks for the opt-in authenticated feedback composition.
use amaterasu_contracts::{LiveFeedbackProfile, RuntimeConfig};
use serde_json::Value;
use std::collections::BTreeSet;

const MARKER: &str = "live_feedback_authorized";
const PROTOCOL: &str = "amaterasu.nvhttp/1";
const PAIRING: &str = "amaterasu.pairing-effects/1";
const PAIRING_ADMIN: &str = "amaterasu.pairing/1";
const IDENTITY: &str = "amaterasu.identity/1";
const STATE: &str = "amaterasu.state/1";
const STREAM: &str = "amaterasu.stream-session/1";
const INPUT: &str = "amaterasu.input/1";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const CONTROL: &str = "amaterasu.control-transport/1";
const HOST_CONFIG: &str = "amaterasu.host-configuration/1";

pub(super) fn validate(config: &RuntimeConfig) -> Result<(), String> {
    validate_topology(config)?;
    if let Some(profile) = &config.live_feedback {
        let proof = config
            .host_context
            .live_feedback_bootstrap
            .as_ref()
            .ok_or("live feedback requires verified host bootstrap evidence")?;
        let state = config
            .service_selections
            .broker
            .get(STATE)
            .and_then(Option::as_ref)
            .ok_or("selected State missing")?;
        let valid_id = |value: &str| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if !config.host_lifecycle_control
            || proof.version != 1
            || proof.profile != *profile
            || proof.state_owner != state.provider
            || !valid_id(&proof.installation_id)
            || proof
                .first_boot_claim
                .as_deref()
                .is_some_and(|claim| !valid_id(claim))
        {
            return Err("invalid trusted live feedback bootstrap evidence".into());
        }
    } else if config.host_context.live_feedback_bootstrap.is_some() {
        return Err("feedback bootstrap evidence has no selected profile".into());
    }
    Ok(())
}

fn validate_topology(config: &RuntimeConfig) -> Result<(), String> {
    if config
        .settings
        .values()
        .any(|s| s.get(MARKER).is_some() || s.get("live_feedback_bootstrap_claim").is_some())
    {
        return Err("live feedback marker is broker-owned".into());
    }
    let Some(p) = &config.live_feedback else {
        return Ok(());
    };
    if config.effect_fence.is_some() {
        return Err("live feedback cannot select an uncoordinated generic effect authority".into());
    }
    let roles = [
        &p.protocol_owner,
        &p.pairing_owner,
        &p.stream_owner,
        &p.input_owner,
        &p.lifecycle_owner,
        &p.control_owner,
    ];
    if roles
        .iter()
        .any(|role| !amaterasu_contracts::valid_name(role))
        || roles.iter().collect::<BTreeSet<_>>().len() != roles.len()
    {
        return Err("invalid live feedback principals".into());
    }
    let stream = settings(config, &p.stream_owner)?;
    let selected_control = &stream["providers"]["control_transport"];
    let control_service = if selected_control.is_null() {
        CONTROL
    } else {
        selected_control
            .as_str()
            .filter(|name| {
                amaterasu_contracts::valid_name(name)
                    && name
                        .strip_suffix("/1")
                        .is_some_and(|base| !base.is_empty() && !base.contains('/'))
            })
            .ok_or("invalid selected live feedback control service")?
    };
    for (service, owner) in [
        (PROTOCOL, &p.protocol_owner),
        (PAIRING, &p.pairing_owner),
        (PAIRING_ADMIN, &p.pairing_owner),
        (STREAM, &p.stream_owner),
        (INPUT, &p.input_owner),
        (LIFECYCLE, &p.lifecycle_owner),
        (control_service, &p.control_owner),
    ] {
        let selection = config
            .service_selections
            .broker
            .get(service)
            .and_then(Option::as_ref)
            .ok_or("live feedback service is not selected")?;
        let contract = if service == control_service {
            CONTROL
        } else {
            service
        };
        if selection.provider != *owner || selection.contract != contract {
            return Err("live feedback selection mismatch".into());
        }
    }
    let identity = config
        .service_selections
        .native
        .get(IDENTITY)
        .and_then(Option::as_ref)
        .ok_or("live feedback identity is not selected")?;
    if identity.provider != p.pairing_owner
        || identity.contract != "amaterasu.identity.native/1"
        || !config
            .principals
            .get(&p.protocol_owner)
            .is_some_and(|g| g.native_use.contains(IDENTITY))
        || !config
            .principals
            .get(&p.pairing_owner)
            .is_some_and(|g| g.native_provide.contains(IDENTITY))
    {
        return Err("live feedback identity mismatch".into());
    }
    let state = config
        .service_selections
        .broker
        .get(STATE)
        .and_then(Option::as_ref)
        .ok_or("live feedback pairing State is not selected")?;
    if state.contract != STATE
        || !config
            .principals
            .get(&p.pairing_owner)
            .is_some_and(|g| g.call.contains(STATE))
    {
        return Err("live feedback pairing State mismatch".into());
    }
    let native_control = config
        .service_selections
        .native
        .get(control_service)
        .and_then(Option::as_ref)
        .ok_or("live feedback native control is not selected")?;
    if native_control.provider != p.control_owner
        || native_control.contract != "amaterasu.control-transport.native/1"
        || !config
            .principals
            .get(&p.stream_owner)
            .is_some_and(|g| g.native_use.contains(control_service))
    {
        return Err("live feedback native control mismatch".into());
    }
    for (owner, service) in [
        (&p.protocol_owner, PAIRING),
        (&p.protocol_owner, LIFECYCLE),
        (&p.stream_owner, PROTOCOL),
        (&p.stream_owner, PAIRING),
        (&p.stream_owner, INPUT),
        (&p.stream_owner, LIFECYCLE),
        (&p.stream_owner, control_service),
        (&p.input_owner, STREAM),
        (&p.input_owner, LIFECYCLE),
    ] {
        if !config
            .principals
            .get(owner)
            .is_some_and(|g| g.call.contains(service))
        {
            return Err("live feedback call grant missing".into());
        }
    }
    let protocol = settings(config, &p.protocol_owner)?;
    let pairing = settings(config, &p.pairing_owner)?;
    let input = settings(config, &p.input_owner)?;
    let lifecycle = settings(config, &p.lifecycle_owner)?;
    let control = settings(config, &p.control_owner)?;
    if protocol["enabled"] != true
        || stream["enabled"] != true
        || protocol["session_lifecycle_required"] != true
        || protocol["effect_source_join"]["effect_owner"] != p.stream_owner
        || protocol["effect_source_join"]["input_consumer"] != p.input_owner
        || pairing["effect_authority"]["protocol_owner"] != p.protocol_owner
        || pairing["effect_authority"]["effect_owner"] != p.stream_owner
        || !contains(&pairing["transport_principals"], &p.protocol_owner)
        || pairing["effect_authority"]["feedback_policies"]
            .as_array()
            .is_none_or(Vec::is_empty)
        || !contains(&stream["allowed_principals"], &p.protocol_owner)
        || stream["enable_input"] != true
        || stream["observe_feedback_sources"] != true
        || stream["feedback_retirement_input_principal"] != p.input_owner
        || stream["feedback_source_protocol_service"] != PROTOCOL
        || stream["feedback_owner_restart_recovery"] != true
        || input["feedback_owner"] != p.stream_owner
        || input["feedback_retirement_service"] != STREAM
        || control["enabled"] != true
        || !contains(&control["allowed_principals"], &p.stream_owner)
        || !contains(&lifecycle["trusted_callers"], &p.protocol_owner)
        || !contains(&lifecycle["trusted_callers"], &p.stream_owner)
        || !contains(&lifecycle["trusted_callers"], &p.input_owner)
        || lifecycle["input_provider"] != p.input_owner
        || !contains(&lifecycle["consumers"]["input"], &p.input_owner)
        || !contains(&lifecycle["consumers"]["input"], &p.stream_owner)
        || !contains(&lifecycle["consumers"]["capture"], &p.stream_owner)
        || !contains(&lifecycle["session_delegates"], &p.stream_owner)
        || [
            "prepare",
            "lookup",
            "snapshot",
            "bind_application",
            "end_session",
            "application_retired",
            "resolve_effect_sources",
        ]
        .iter()
        .any(|operation| !contains(&lifecycle["operation_grants"][&p.protocol_owner], operation))
        || !contains(
            &lifecycle["operation_grants"][&p.input_owner],
            "validate_input_binding",
        )
        || [
            "validate_candidate",
            "ack_binding",
            "start_stream",
            "pause_stream",
            "retire_stream",
            "binding_validate",
            "capture_invalidated",
            "issue_input_binding",
            "lookup",
        ]
        .iter()
        .any(|operation| !contains(&lifecycle["operation_grants"][&p.stream_owner], operation))
    {
        return Err("live feedback settings mismatch".into());
    }
    match &p.host_configuration_owner {
        Some(owner) => {
            let selected = config
                .service_selections
                .broker
                .get(HOST_CONFIG)
                .and_then(Option::as_ref)
                .ok_or("live feedback host configuration not selected")?;
            if roles.contains(&owner)
                || selected.provider != *owner
                || selected.contract != HOST_CONFIG
                || pairing["host_configuration"] != true
                || pairing["effect_authority"]["configuration_owner"] != *owner
                || settings(config, owner)?["pairing_effect_authority"]["principal"]
                    != p.pairing_owner
                || !config
                    .principals
                    .get(owner)
                    .is_some_and(|g| g.call.contains(PAIRING))
                || !config
                    .principals
                    .get(&p.pairing_owner)
                    .is_some_and(|g| g.call.contains(HOST_CONFIG))
            {
                return Err("live feedback host configuration mismatch".into());
            }
        }
        None => {
            if pairing["host_configuration"] == true
                || !pairing["effect_authority"]["configuration_owner"].is_null()
                || config
                    .principals
                    .values()
                    .any(|g| g.provide.contains(HOST_CONFIG))
            {
                return Err("live feedback host configuration is not fenced".into());
            }
        }
    }
    Ok(())
}
fn settings<'a>(config: &'a RuntimeConfig, owner: &str) -> Result<&'a Value, String> {
    config
        .settings
        .get(owner)
        .filter(|value| value.is_object())
        .ok_or_else(|| "live feedback settings missing".into())
}
fn contains(value: &Value, needle: &str) -> bool {
    value
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == needle))
}
pub(super) fn authorized(profile: &LiveFeedbackProfile, caller: &str) -> bool {
    [
        &profile.protocol_owner,
        &profile.pairing_owner,
        &profile.stream_owner,
    ]
    .iter()
    .any(|owner| owner.as_str() == caller)
}
pub(super) fn inject(settings: &mut Value) {
    if let Some(object) = settings.as_object_mut() {
        object.insert(MARKER.into(), Value::Bool(true));
    }
}

pub(super) fn claim_payload(config: &RuntimeConfig) -> Option<Value> {
    let proof = config.host_context.live_feedback_bootstrap.as_ref()?;
    let claim = proof.first_boot_claim.as_ref()?;
    Some(
        serde_json::json!({"version":1,"installation_id":proof.installation_id,
        "claim":claim,"pairing_owner":proof.profile.pairing_owner,"effect_owner":proof.profile.stream_owner}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Broker;
    use amaterasu_contracts::{Command, Grants, HOST_LIFECYCLE_CONTROL_PRINCIPAL, Response};
    use serde_json::json;

    fn profile() -> RuntimeConfig {
        let mut c: RuntimeConfig = serde_json::from_value(json!({
            "principals": {
                "protocol":{"provide":[PROTOCOL],"call":[PAIRING,LIFECYCLE],"native_use":[IDENTITY]},
                "pairing":{"provide":[PAIRING,PAIRING_ADMIN],"call":[STATE],"native_provide":[IDENTITY]},
                "state":{"provide":[STATE]},
                "stream":{"provide":[STREAM],"call":[PROTOCOL,PAIRING,INPUT,LIFECYCLE,CONTROL],"native_use":[CONTROL]},
                "input":{"provide":[INPUT],"call":[STREAM,LIFECYCLE]},
                "lifecycle":{"provide":[LIFECYCLE]},
                "control":{"provide":[CONTROL],"native_provide":[CONTROL]}
            },
            "settings":{
                "protocol":{"enabled":true,"session_lifecycle_required":true,"effect_source_join":{"effect_owner":"stream","input_consumer":"input"}},
                "pairing":{"transport_principals":["protocol"],"effect_authority":{"protocol_owner":"protocol","effect_owner":"stream","feedback_policies":[{}]}},
                "stream":{"enabled":true,"allowed_principals":["protocol"],"enable_input":true,"observe_feedback_sources":true,"feedback_retirement_input_principal":"input","feedback_source_protocol_service":PROTOCOL,"feedback_owner_restart_recovery":true},
                "input":{"feedback_owner":"stream","feedback_retirement_service":STREAM},
                "lifecycle":{"trusted_callers":["protocol","stream","input"],"session_delegates":["stream"],"input_provider":"input","consumers":{"capture":["stream"],"input":["stream","input"]},"operation_grants":{"protocol":["prepare","lookup","snapshot","bind_application","end_session","application_retired","resolve_effect_sources"],"input":["validate_input_binding"],"stream":["validate_candidate","ack_binding","start_stream","pause_stream","retire_stream","binding_validate","capture_invalidated","issue_input_binding","lookup"]}},
                "control":{"enabled":true,"allowed_principals":["stream"]}
            },
            "live_feedback":{"protocol_owner":"protocol","pairing_owner":"pairing","stream_owner":"stream","input_owner":"input","lifecycle_owner":"lifecycle","control_owner":"control"}
        })).unwrap();
        for (service, owner) in [
            (PROTOCOL, "protocol"),
            (PAIRING, "pairing"),
            (PAIRING_ADMIN, "pairing"),
            (STATE, "state"),
            (STREAM, "stream"),
            (INPUT, "input"),
            (LIFECYCLE, "lifecycle"),
            (CONTROL, "control"),
        ] {
            c.service_selections.broker.insert(
                service.into(),
                Some(amaterasu_contracts::SelectedService {
                    provider: owner.into(),
                    contract: service.into(),
                }),
            );
        }
        c.service_selections.native.insert(
            IDENTITY.into(),
            Some(amaterasu_contracts::SelectedService {
                provider: "pairing".into(),
                contract: "amaterasu.identity.native/1".into(),
            }),
        );
        c.service_selections.native.insert(
            CONTROL.into(),
            Some(amaterasu_contracts::SelectedService {
                provider: "control".into(),
                contract: "amaterasu.control-transport.native/1".into(),
            }),
        );
        c
    }
    #[test]
    fn selected_profile_stays_closed_without_host_bootstrap_proof() {
        let selected = profile();
        assert!(validate_topology(&selected).is_ok());
        assert!(Broker::new(selected).is_err());
        let mut unselected = profile();
        unselected.live_feedback = None;
        let mut broker = Broker::new(unselected).unwrap();
        assert!(
            matches!(broker.dispatch("protocol", Command::GetSettings {}), Response::Result {payload} if payload.get(MARKER).is_none())
        );
    }

    fn host_bootstrap_profile(claim: Option<&str>) -> RuntimeConfig {
        let mut configuration = profile();
        let selected_profile = configuration.live_feedback.clone().unwrap();
        configuration.host_lifecycle_control = true;
        configuration
            .principals
            .insert(HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(), Grants::default());
        configuration.host_context = amaterasu_contracts::RuntimeHostContext {
            schema: 1,
            live_feedback_bootstrap: Some(amaterasu_contracts::LiveFeedbackHostProof {
                version: 1,
                installation_id: "a".repeat(32),
                profile: selected_profile,
                state_owner: "state".into(),
                first_boot_claim: claim.map(str::to_owned),
            }),
            ..Default::default()
        };
        configuration
    }

    fn test_state_directory() -> std::path::PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "amaterasu-feedback-broker-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn assert_error_code(response: Response, expected: &str) {
        assert!(
            matches!(&response, Response::Error { code, .. } if code == expected),
            "expected {expected}, got {response:?}"
        );
    }

    fn register_state_provider(broker: &mut Broker) {
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Register {
                    service: STATE.into(),
                }
            ),
            Response::Ok
        ));
    }

    fn complete_real_state_bootstrap(
        broker: &mut Broker,
        store: &amaterasu_state::test_support::TestStateStore,
    ) -> u64 {
        let Response::Accepted { id } = broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::InitializeLiveFeedback {},
        ) else {
            panic!("trusted host bootstrap was not accepted");
        };
        let Response::Request {
            id: state_request,
            caller,
            service,
            payload,
            ..
        } = broker.dispatch("state", Command::Poll {})
        else {
            panic!("selected State provider did not receive initialization");
        };
        assert_eq!(service, STATE);
        let result = store.handle(&caller, payload);
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: state_request,
                    payload: result,
                }
            ),
            Response::Ok
        ));
        id
    }

    #[test]
    fn host_bootstrap_opens_state_only_after_real_claim_persistence() {
        let directory = test_state_directory();
        let store = amaterasu_state::test_support::TestStateStore::open(directory.clone()).unwrap();
        let mut broker = Broker::new(host_bootstrap_profile(Some(&"b".repeat(32)))).unwrap();
        register_state_provider(&mut broker);

        assert_error_code(
            broker.dispatch(
                "pairing",
                Command::Request {
                    service: STATE.into(),
                    payload: json!({"action":"get","key":"unused"}),
                    timeout_ms: 1000,
                    operation_id: "before-host-state-init".into(),
                    parent: None,
                },
            ),
            "bootstrap_pending",
        );

        let request = complete_real_state_bootstrap(&mut broker, &store);
        assert!(matches!(
            broker.dispatch(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                Command::FinishLiveFeedbackInitialization { request }
            ),
            Response::Result { payload } if payload["ok"] == true
                && payload["revision"] == 1
                && payload["claim"]["installation_id"] == "a".repeat(32)
                && payload["claim"]["claim"] == "b".repeat(32)
        ));
        assert_eq!(
            store.handle(
                "pairing",
                json!({"action":"get","key":"host/feedback-bootstrap/v1"})
            )["value"]["phase"],
            "prepared"
        );
        assert!(matches!(
            broker.dispatch(
                "pairing",
                Command::Request {
                    service: STATE.into(),
                    payload: json!({"action":"get","key":"unused"}),
                    timeout_ms: 1000,
                    operation_id: "after-host-state-init".into(),
                    parent: None,
                },
            ),
            Response::Accepted { .. }
        ));

        drop(store);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn mismatched_provider_result_fails_closed_and_keeps_state_gated() {
        let mut broker = Broker::new(host_bootstrap_profile(Some(&"b".repeat(32)))).unwrap();
        register_state_provider(&mut broker);
        let Response::Accepted { id: request } = broker.dispatch(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL,
            Command::InitializeLiveFeedback {},
        ) else {
            panic!("trusted host bootstrap was not accepted");
        };
        let Response::Request { id, .. } = broker.dispatch("state", Command::Poll {}) else {
            panic!("selected State provider did not receive initialization");
        };
        assert!(matches!(
            broker.dispatch(
                "state",
                Command::Complete {
                    request: id,
                    payload: json!({
                        "ok":true,
                        "revision":1,
                        "claim":{
                            "version":1,
                            "installation_id":"wrong-installation",
                            "claim":"b".repeat(32),
                            "pairing_owner":"pairing",
                            "effect_owner":"stream"
                        }
                    }),
                }
            ),
            Response::Ok
        ));
        assert_error_code(
            broker.dispatch(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                Command::FinishLiveFeedbackInitialization { request },
            ),
            "bootstrap_claim_mismatch",
        );
        assert_error_code(
            broker.dispatch(
                "pairing",
                Command::Request {
                    service: STATE.into(),
                    payload: json!({"action":"get","key":"unused"}),
                    timeout_ms: 1000,
                    operation_id: "after-bad-host-state-init".into(),
                    parent: None,
                },
            ),
            "bootstrap_pending",
        );
    }

    #[test]
    fn consumed_claim_is_not_reissued_after_lost_reply_or_restart() {
        let directory = test_state_directory();
        let store = amaterasu_state::test_support::TestStateStore::open(directory.clone()).unwrap();
        let mut first = Broker::new(host_bootstrap_profile(Some(&"b".repeat(32)))).unwrap();
        register_state_provider(&mut first);
        let _request = complete_real_state_bootstrap(&mut first, &store);
        // Model the host exiting before it receives the terminal State response.
        drop(first);

        let mut restarted = Broker::new(host_bootstrap_profile(None)).unwrap();
        register_state_provider(&mut restarted);
        assert_error_code(
            restarted.dispatch(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                Command::InitializeLiveFeedback {},
            ),
            "permission_denied",
        );
        assert_eq!(
            store.handle(
                "pairing",
                json!({"action":"get","key":"host/feedback-bootstrap/v1"})
            )["value"]["phase"],
            "prepared"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn consumed_claim_is_not_reissued_after_state_history_is_lost() {
        let directory = test_state_directory();
        let store = amaterasu_state::test_support::TestStateStore::open(directory.clone()).unwrap();
        let mut first = Broker::new(host_bootstrap_profile(Some(&"b".repeat(32)))).unwrap();
        register_state_provider(&mut first);
        let _request = complete_real_state_bootstrap(&mut first, &store);
        assert_eq!(
            store.handle(
                "pairing",
                json!({"action":"get","key":"host/feedback-bootstrap/v1"})
            )["value"]["phase"],
            "prepared"
        );
        drop(first);
        drop(store);

        // The host's consumed installation witness survives this complete loss
        // of State namespaces. A restart therefore has no claim to offer the
        // recreated empty provider and cannot reopen initialization.
        std::fs::remove_dir_all(directory.join("namespaces")).unwrap();
        let store = amaterasu_state::test_support::TestStateStore::open(directory.clone()).unwrap();
        let mut restarted = Broker::new(host_bootstrap_profile(None)).unwrap();
        register_state_provider(&mut restarted);
        assert_error_code(
            restarted.dispatch(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                Command::InitializeLiveFeedback {},
            ),
            "permission_denied",
        );

        let Response::Accepted { id } = restarted.dispatch(
            "pairing",
            Command::Request {
                service: STATE.into(),
                payload: json!({"action":"get","key":"host/feedback-bootstrap/v1"}),
                timeout_ms: 1000,
                operation_id: "lost-state-history-read".into(),
                parent: None,
            },
        ) else {
            panic!("pairing State read was not admitted after startup");
        };
        let Response::Request {
            id: state_request,
            caller,
            payload,
            ..
        } = restarted.dispatch("state", Command::Poll {})
        else {
            panic!("selected State provider did not receive the restart read");
        };
        let state_result = store.handle(&caller, payload);
        assert_eq!(state_result["revision"], 0);
        assert_eq!(state_result["found"], false);
        assert!(matches!(
            restarted.dispatch(
                "state",
                Command::Complete {
                    request: state_request,
                    payload: state_result,
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            restarted.dispatch("pairing", Command::TakeResult { request: id }),
            Response::Result { payload } if payload["revision"] == 0 && payload["found"] == false
        ));

        drop(store);
        let _ = std::fs::remove_dir_all(directory);
    }
    #[test]
    fn replacement_control_service_uses_the_same_contract_and_owner() {
        let mut c = profile();
        let replacement = "example.control/1";
        c.settings.get_mut("stream").unwrap()["providers"] =
            json!({"control_transport":replacement});
        let stream = c.principals.get_mut("stream").unwrap();
        stream.call.remove(CONTROL);
        stream.call.insert(replacement.into());
        stream.native_use.remove(CONTROL);
        stream.native_use.insert(replacement.into());
        let control = c.principals.get_mut("control").unwrap();
        control.provide.remove(CONTROL);
        control.provide.insert(replacement.into());
        control.native_provide.remove(CONTROL);
        control.native_provide.insert(replacement.into());
        c.service_selections.broker.remove(CONTROL);
        c.service_selections.broker.insert(
            replacement.into(),
            Some(amaterasu_contracts::SelectedService {
                provider: "control".into(),
                contract: CONTROL.into(),
            }),
        );
        c.service_selections.native.remove(CONTROL);
        c.service_selections.native.insert(
            replacement.into(),
            Some(amaterasu_contracts::SelectedService {
                provider: "control".into(),
                contract: "amaterasu.control-transport.native/1".into(),
            }),
        );
        assert!(validate_topology(&c).is_ok());
    }
    #[test]
    fn every_live_lifecycle_operation_is_required() {
        for (owner, operation) in [
            ("protocol", "prepare"),
            ("protocol", "lookup"),
            ("protocol", "snapshot"),
            ("protocol", "bind_application"),
            ("protocol", "end_session"),
            ("protocol", "application_retired"),
            ("protocol", "resolve_effect_sources"),
            ("input", "validate_input_binding"),
            ("stream", "validate_candidate"),
            ("stream", "ack_binding"),
            ("stream", "start_stream"),
            ("stream", "pause_stream"),
            ("stream", "retire_stream"),
            ("stream", "binding_validate"),
            ("stream", "capture_invalidated"),
            ("stream", "issue_input_binding"),
            ("stream", "lookup"),
        ] {
            let mut c = profile();
            c.settings.get_mut("lifecycle").unwrap()["operation_grants"][owner]
                .as_array_mut()
                .unwrap()
                .retain(|entry| entry != operation);
            assert!(
                validate_topology(&c).is_err(),
                "missing {owner}:{operation}"
            );
        }
    }
    #[test]
    fn profile_requires_the_selected_host_configuration_precommit_authority() {
        let mut c = profile();
        c.principals.insert(
            "configuration".into(),
            serde_json::from_value(json!({"provide":[HOST_CONFIG],"call":[PAIRING]})).unwrap(),
        );
        c.principals
            .get_mut("pairing")
            .unwrap()
            .call
            .insert(HOST_CONFIG.into());
        c.service_selections.broker.insert(
            HOST_CONFIG.into(),
            Some(amaterasu_contracts::SelectedService {
                provider: "configuration".into(),
                contract: HOST_CONFIG.into(),
            }),
        );
        c.settings.insert(
            "configuration".into(),
            json!({"pairing_effect_authority":{"principal":"pairing"}}),
        );
        c.settings.get_mut("pairing").unwrap()["host_configuration"] = json!(true);
        c.settings.get_mut("pairing").unwrap()["effect_authority"]["configuration_owner"] =
            json!("configuration");
        c.live_feedback.as_mut().unwrap().host_configuration_owner = Some("configuration".into());
        assert!(validate_topology(&c).is_ok());
        c.principals
            .get_mut("configuration")
            .unwrap()
            .call
            .remove(PAIRING);
        assert!(validate_topology(&c).is_err());
    }
    #[test]
    fn profile_rejects_missing_grants_owner_mismatch_and_unfenced_host_configuration() {
        let mut c = profile();
        c.principals.get_mut("stream").unwrap().call.remove(CONTROL);
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.settings.get_mut("input").unwrap()["feedback_owner"] = json!("other");
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.service_selections
            .native
            .get_mut(CONTROL)
            .unwrap()
            .as_mut()
            .unwrap()
            .provider = "stream".into();
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.service_selections.broker.remove(LIFECYCLE);
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.effect_fence = Some(
            serde_json::from_value(json!({"authority":"pairing","issuer":"stream","methods":[]}))
                .unwrap(),
        );
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.settings.get_mut("stream").unwrap()["feedback_owner_restart_recovery"] = json!(false);
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.settings.get_mut("pairing").unwrap()["host_configuration"] = json!(true);
        assert!(validate_topology(&c).is_err());
        let mut c = profile();
        c.live_feedback = None;
        c.settings.get_mut("pairing").unwrap()[MARKER] = json!(true);
        assert!(validate_topology(&c).is_err());
    }
}
