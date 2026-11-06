use super::*;
use crate::profiles::{PackageProfile, installer_policy, linux_session};
use amaterasu_contracts::{Command, Response};
use amaterasu_runtime::Broker;
use std::collections::{BTreeMap, BTreeSet};

struct PrivateTestRoot(std::path::PathBuf);

impl PrivateTestRoot {
    fn new() -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "remote-session-profile-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for PrivateTestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
    assert!(
        policy.settings["amaterasu.rtsp"]
            .get("retirement_status_principals")
            .is_none()
    );
    assert!(
        policy.settings["amaterasu.rtsp"]
            .get("retirement_status_owners")
            .is_none()
    );
    assert_eq!(
        policy.settings["amaterasu.launch"]["application_retirement_observers"][COMPONENT.id],
        "amaterasu.nvhttp"
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
        policy.settings["amaterasu.nvhttp"]["auxiliary_exact_capture_routes"],
        json!({"managed_output":"dxgi","desktop_output":"dxgi"})
    );
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
    assert_eq!(public_methods.len(), 8);
    for method in public_methods {
        assert_eq!(method.provider, COMPONENT.id);
        assert_eq!(
            method.callers,
            BTreeSet::from(["amaterasu.management".into()])
        );
    }
}

#[test]
fn linux_exact_desktop_route_requires_explicit_named_capture_authorization() {
    let root = PrivateTestRoot::new();
    let policy = linux_session::linux_session_policy(
        &root.0,
        &linux_session::options::LinuxSessionOptions::default(),
    )
    .unwrap();
    assert_eq!(
        policy.settings["amaterasu.nvhttp"]["auxiliary_exact_capture_routes"],
        json!({"managed_output":"vkms"})
    );

    let root = PrivateTestRoot::new();
    let mut authorized = linux_session::linux_session_policy(
        &root.0,
        &linux_session::options::LinuxSessionOptions {
            enable_virtual_display: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        authorized.settings["amaterasu.nvhttp"]["auxiliary_exact_capture_routes"],
        json!({"managed_output":"vkms","desktop_output":"pipewire"})
    );
    assert_eq!(
        authorized.settings["amaterasu.input"]["allow_injection"],
        false
    );
    assert_eq!(
        authorized.settings["amaterasu.display"]["allow_mutation"],
        false
    );
    authorized.principals.remove("amaterasu.capture-pipewire");
    configure_capture_routes(&mut authorized);
    assert!(
        authorized.settings["amaterasu.nvhttp"]["auxiliary_exact_capture_routes"]
            .get("desktop_output")
            .is_none()
    );
}

#[test]
fn monitor_profiles_use_the_same_private_catalog_owner_as_the_protocol() {
    for owner in [
        linux_session::options::CatalogOwner::Management,
        linux_session::options::CatalogOwner::Console,
    ] {
        let root = PrivateTestRoot::new();
        let mut policy = linux_session::linux_session_policy(
            &root.0,
            &linux_session::options::LinuxSessionOptions {
                catalog_owner: owner,
                ..Default::default()
            },
        )
        .unwrap();
        let catalog = &policy.settings["amaterasu.catalog"];
        assert_eq!(
            catalog["profile_principals"][COMPONENT.id],
            catalog["export_principals"]["amaterasu.nvhttp"]
        );
        policy.settings.get_mut("amaterasu.catalog").unwrap()["profile_principals"][COMPONENT.id] =
            json!("unrelated.owner");
        let before = serde_json::to_value(&policy).unwrap();
        assert!(install(&mut policy).is_err());
        assert_eq!(serde_json::to_value(&policy).unwrap(), before);
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
fn transport_recovery_observer_is_bound_to_an_already_approved_stream_owner() {
    let root = PrivateTestRoot::new();
    let policy = linux_session::linux_session_policy(
        &root.0,
        &linux_session::options::LinuxSessionOptions::default(),
    )
    .unwrap();
    let rtsp = &policy.settings["amaterasu.rtsp"];
    assert_eq!(rtsp["allowed_principals"], json!(["amaterasu.nvhttp"]));
    assert_eq!(rtsp["retirement_status_principals"], json!([COMPONENT.id]));
    assert_eq!(
        rtsp["retirement_status_owners"][COMPONENT.id],
        "amaterasu.nvhttp"
    );
    let mut conflict = rtsp.clone();
    conflict["retirement_status_owners"][COMPONENT.id] = json!("foreign.owner");
    assert!(configure_transport_retirement_observer(&mut conflict).is_err());
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
