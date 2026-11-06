use super::*;
use crate::profiles::{PackageProfile, installer_policy, linux_session};
use amaterasu_contracts::{Command, Response};
use amaterasu_runtime::Broker;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn product_graphs_select_the_plugin_and_synthetic_capture_without_dependency_cycles() {
    for profile in [
        PackageProfile::Platform,
        PackageProfile::ExtensionExample,
        PackageProfile::LinuxSession,
        PackageProfile::LinuxSessionRtxHdr,
        PackageProfile::LinuxSessionExtensions,
    ] {
        let components = profile.components();
        let protocol = components
            .iter()
            .find(|component| component.id == "amaterasu.nvhttp")
            .unwrap();
        assert!(protocol.calls.contains(&session_presentation::SERVICE));
        assert!(protocol.dependencies.contains(&COMPONENT.id));
        let mut remaining: Vec<_> = components.iter().collect();
        let mut loaded = BTreeSet::new();
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .position(|component| {
                    component
                        .dependencies
                        .iter()
                        .all(|dependency| loaded.contains(dependency))
                })
                .expect("complete acyclic startup graph");
            assert!(loaded.insert(remaining.remove(next).id));
        }
        assert!(loaded.contains(COMPONENT.id));
        assert!(loaded.contains(BLACK_CAPTURE.id));
    }
}

#[test]
fn installer_selects_exact_contracts_and_keeps_hardware_disabled() {
    let policy = installer_policy("web", BTreeMap::new());
    assert_eq!(
        policy.settings["amaterasu.nvhttp"]["session_presentation_provider"],
        session_presentation::SERVICE
    );
    assert_eq!(policy.settings["amaterasu.input"]["allow_injection"], false);
    assert_eq!(
        policy.settings["amaterasu.display"]["allow_mutation"],
        false
    );
    assert_eq!(
        policy.settings["amaterasu.virtual-display"]["enabled"],
        false
    );
    assert_eq!(
        policy.settings["amaterasu.virtual-display"]["target_verifiers"],
        json!(["amaterasu.display"])
    );
    assert_eq!(
        policy.settings["amaterasu.rtsp"]["retirement_status_principals"],
        json!([COMPONENT.id])
    );
    assert!(
        !policy.settings["amaterasu.rtsp"]["allowed_principals"]
            .as_array()
            .unwrap()
            .contains(&json!(COMPONENT.id))
    );
    assert_eq!(policy.settings["amaterasu.video"]["enabled"], false);
    assert_eq!(policy.settings["amaterasu.nvhttp"]["allow_remote"], false);
    assert_eq!(policy.settings[BLACK_CAPTURE.id], json!({"enabled":true}));
    assert_eq!(
        policy.settings["amaterasu.video"]["external_capture_providers"]["black"],
        json!({"service":"amaterasu.capture-black/1","frame_transport":"generic_cpu"})
    );
    for method in session_presentation::methods() {
        let selected = policy
            .api_methods
            .iter()
            .find(|entry| entry.method == method)
            .unwrap();
        assert_eq!(selected.provider, COMPONENT.id);
        assert_eq!(
            selected.callers,
            BTreeSet::from(["amaterasu.nvhttp".into()])
        );
    }
    let public_methods: Vec<_> = policy
        .api_methods
        .iter()
        .filter(|entry| entry.method.service == "nonary.remote-session/1")
        .collect();
    assert_eq!(public_methods.len(), 7);
    for method in public_methods {
        assert_eq!(method.provider, COMPONENT.id);
        assert_eq!(
            method.callers,
            BTreeSet::from(["amaterasu.management".into()])
        );
    }
}

#[test]
fn installed_policy_registers_exact_sdk_methods_and_rejects_changed_schemas() {
    let policy = installer_policy("web", BTreeMap::new());
    let mut broker = Broker::new(policy).unwrap();
    for method in session_presentation::methods() {
        let mut changed = method.clone();
        changed.request.allow_extra = !changed.request.allow_extra;
        assert!(matches!(
            broker.dispatch(COMPONENT.id, Command::RegisterApi { method: changed }),
            Response::Error { .. }
        ));
        assert!(matches!(
            broker.dispatch(COMPONENT.id, Command::RegisterApi { method }),
            Response::Ok
        ));
    }
}

#[test]
fn linux_capture_call_chain_reaches_only_the_selected_native_provider() {
    let components = PackageProfile::LinuxSession.components();
    for caller in [
        "amaterasu.nvhttp",
        "amaterasu.rtsp",
        "amaterasu.stream-session",
        linux_session::VIDEO_COMPONENT_ID,
    ] {
        let component = components.iter().find(|entry| entry.id == caller).unwrap();
        assert!(component.calls.contains(&"amaterasu.capture-black/1"));
        assert_eq!(
            component.native_uses.contains(&"amaterasu.capture-black/1"),
            caller == linux_session::VIDEO_COMPONENT_ID
        );
    }
}

#[test]
fn installing_contracts_is_idempotent_and_preserves_operator_preferences() {
    let mut policy = installer_policy("web", BTreeMap::new());
    policy.settings.get_mut(COMPONENT.id).unwrap()["remote_monitor_mute_audio"] = json!(true);
    let count = policy.api_methods.len();
    install(&mut policy).unwrap();
    assert_eq!(policy.api_methods.len(), count);
    assert_eq!(
        policy.settings[COMPONENT.id]["remote_monitor_mute_audio"],
        true
    );
    policy
        .api_methods
        .iter_mut()
        .find(|entry| entry.method.service == session_presentation::SERVICE)
        .unwrap()
        .callers
        .insert("amaterasu.management".into());
    let before = serde_json::to_value(&policy).unwrap();
    assert!(install(&mut policy).is_err());
    assert_eq!(serde_json::to_value(&policy).unwrap(), before);
}

#[test]
fn a_package_without_the_plugin_does_not_gain_a_default_provider() {
    let mut policy = RuntimeConfig::default();
    policy
        .settings
        .insert("amaterasu.nvhttp".into(), json!({"enabled":true}));
    install(&mut policy).unwrap();
    assert!(
        policy.settings["amaterasu.nvhttp"]
            .get("session_presentation_provider")
            .is_none()
    );
    assert!(!policy.settings.contains_key(COMPONENT.id));
    assert!(policy.api_methods.is_empty());
}
