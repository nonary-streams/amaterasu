use super::super::options::CatalogOwner;
use super::super::*;
use crate::profiles::{
    CAPTURE_DXGI, CAPTURE_WGC, CAPTURE_X11, DIAGNOSTICS, HOST_CONFIGURATION, INPUT_PROBE, PLAYNITE,
    PackageProfile, SOFTWARE_AUDIO_SOURCE, SYSTEM_TRAY, UPDATER, development_policy,
    installer_policy,
};
use amaterasu_contracts::{Command, Response};
use amaterasu_runtime::Broker;
use std::collections::BTreeSet;

const NVHTTP_ID: &str = "amaterasu.nvhttp";
const RTSP_ID: &str = "amaterasu.rtsp";
const STREAM_SESSION_ID: &str = "amaterasu.stream-session";

pub(super) struct TemporaryRoot(std::path::PathBuf);

impl TemporaryRoot {
    pub(super) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-linux-session-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn default_policy(name: &str) -> RuntimeConfig {
    let root = TemporaryRoot::new(name);
    linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap()
}

pub(super) fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn component<'a>(id: &str) -> &'a ComponentDefinition {
    COMPONENTS
        .iter()
        .find(|component| component.id == id)
        .unwrap_or_else(|| panic!("{id} is not in the Linux session profile"))
}

fn providers() -> BTreeMap<&'static str, &'static str> {
    let mut providers = BTreeMap::new();
    for component in COMPONENTS {
        for service in component.provides {
            providers.insert(*service, component.id);
        }
    }
    providers
}

#[test]
fn profile_contains_unique_providers_and_all_dependencies() {
    let ids: BTreeSet<_> = COMPONENTS.iter().map(|component| component.id).collect();
    assert_eq!(ids.len(), COMPONENTS.len());
    let mut control = BTreeSet::new();
    let mut native = BTreeSet::new();
    for component in COMPONENTS {
        assert!(
            component
                .dependencies
                .iter()
                .all(|dependency| ids.contains(dependency)),
            "{} dependency missing",
            component.id
        );
        for service in component.provides {
            assert!(control.insert(*service), "duplicate {service}");
        }
        for service in component.native_provides {
            assert!(native.insert(*service), "duplicate native {service}");
        }
    }
    for component in COMPONENTS {
        assert!(
            component
                .native_uses
                .iter()
                .all(|service| native.contains(service)),
            "{} native provider missing",
            component.id
        );
    }
    let selected: Vec<_> = PackageProfile::LinuxSession
        .components()
        .iter()
        .map(|component| component.id)
        .collect();
    let composed: Vec<_> = COMPONENTS.iter().map(|component| component.id).collect();
    assert_eq!(selected, composed);
}

#[test]
fn profile_loads_in_dependency_order_without_cycles() {
    let mut remaining: Vec<_> = COMPONENTS.iter().collect();
    let mut loaded = BTreeSet::new();
    while !remaining.is_empty() {
        let position = remaining
            .iter()
            .position(|component| {
                component
                    .dependencies
                    .iter()
                    .all(|dependency| loaded.contains(dependency))
            })
            .expect("dependency cycle or absent dependency");
        loaded.insert(remaining.remove(position).id);
    }
}

#[test]
fn every_call_targets_a_provider_the_caller_depends_on() {
    let providers = providers();
    for caller in COMPONENTS {
        let closure = dependency_closure(caller.id);
        for service in caller.calls {
            // This service is supplied by the host, not by a loadable component.
            if *service == "amaterasu.host-logs/1" {
                assert_eq!(caller.id, "amaterasu.management");
                continue;
            }
            let provider = providers
                .get(service)
                .unwrap_or_else(|| panic!("{} calls absent service {service}", caller.id));
            assert!(
                *provider == caller.id || closure.contains(provider),
                "{} calls {service} without depending on {provider}",
                caller.id
            );
        }
    }
}

fn dependency_closure(id: &str) -> BTreeSet<&'static str> {
    let mut closure = BTreeSet::new();
    let mut pending = vec![component(id)];
    while let Some(next) = pending.pop() {
        for dependency in next.dependencies {
            if closure.insert(*dependency) {
                pending.push(component(dependency));
            }
        }
    }
    closure
}

#[test]
fn profile_uses_real_libraries_and_none_of_the_fixture_or_foreign_platform_components() {
    for (definition, library) in [
        (INPUT, "amaterasu_input"),
        (AUDIO, "amaterasu_audio"),
        (CAPTURE_KMS, "amaterasu_capture_kms"),
        (VIRTUAL_DISPLAY, "amaterasu_virtual_display"),
        (ENCODE, "amaterasu_encode"),
        (AUDIO_ENCODE, "amaterasu_audio_encode"),
    ] {
        let included = component(definition.id);
        assert_eq!(included.library, library);
        assert_eq!(included.library, definition.library);
    }
    let video = component(VIDEO_COMPONENT_ID);
    assert_eq!(video.library, VIDEO.library);
    assert_eq!(video.provides, VIDEO.provides);
    assert_eq!(video.native_provides, VIDEO.native_provides);
    assert_eq!(
        video.native_uses,
        &[
            "amaterasu.capture-kms/1",
            "amaterasu.capture-pipewire/1",
            "amaterasu.capture-vkms/1",
            "amaterasu.encode/1"
        ]
    );
    assert!(
        COMPONENTS.iter().all(|included| included.id != VIDEO.id),
        "the generic video id belongs to Platform and the probes"
    );

    let excluded = [
        INPUT_PROBE.id,
        SOFTWARE_AUDIO_SOURCE.id,
        CAPTURE_DXGI.id,
        CAPTURE_WGC.id,
        CAPTURE_X11.id,
        // Disabled by omission: no bootstrap switch keeps the updater from
        // checking its release feed, and none of these has a Linux job here.
        UPDATER.id,
        SYSTEM_TRAY.id,
        PLAYNITE.id,
        HOST_CONFIGURATION.id,
        DIAGNOSTICS.id,
        "amaterasu.linux-probe-video",
        "amaterasu.stream-source",
        "amaterasu.capture-source",
        "amaterasu.capture-vibeshine-drm",
        "amaterasu.upnp",
    ];
    for id in excluded {
        assert!(
            COMPONENTS.iter().all(|included| included.id != id),
            "{id} must not be part of the Linux session profile"
        );
    }
    for service in [
        "amaterasu.capture-dxgi/1",
        "amaterasu.capture-wgc/1",
        "amaterasu.capture-x11/1",
        "amaterasu.updater/1",
        "amaterasu.system-tray/1",
        "amaterasu.playnite/1",
        "amaterasu.host-configuration/1",
        "amaterasu.diagnostics/1",
        "amaterasu.host-lifecycle/1",
    ] {
        for included in COMPONENTS {
            assert!(
                !included.calls.contains(&service)
                    && !included.native_uses.contains(&service)
                    && !included.provides.contains(&service),
                "{} must not mention {service}",
                included.id
            );
        }
    }
}

#[test]
fn other_profiles_and_policies_do_not_gain_the_linux_session_composition() {
    for profile in [
        PackageProfile::Example,
        PackageProfile::Platform,
        PackageProfile::MediaProbe,
        PackageProfile::StreamProbe,
        PackageProfile::SoftwareProbe,
        PackageProfile::LinuxProbe,
    ] {
        assert!(
            profile
                .components()
                .iter()
                .all(|component| component.id != VIDEO_COMPONENT_ID)
        );
    }
    let root = TemporaryRoot::new("development-unchanged");
    let development = development_policy(root.path()).unwrap();
    assert!(!development.principals.contains_key(VIDEO_COMPONENT_ID));
    assert!(!development.settings.contains_key(VIDEO_COMPONENT_ID));
    let installer = installer_policy("__WEB__", BTreeMap::new());
    assert!(!installer.principals.contains_key(VIDEO_COMPONENT_ID));
    assert!(!installer.settings.contains_key(VIDEO_COMPONENT_ID));
}

#[test]
fn policy_grants_each_component_exactly_what_its_manifest_requests() {
    let policy = default_policy("grants");
    for component in COMPONENTS {
        if component.id == RUNTIME_ID {
            assert!(!policy.principals.contains_key(RUNTIME_ID));
            continue;
        }
        let granted = &policy.principals[component.id];
        let requested = component.grants();
        assert_eq!(granted.call, requested.call, "{}", component.id);
        assert_eq!(granted.provide, requested.provide, "{}", component.id);
        assert_eq!(granted.native_use, requested.native_use, "{}", component.id);
        assert_eq!(
            granted.native_provide, requested.native_provide,
            "{}",
            component.id
        );
    }
    let console = &policy.principals[CONSOLE_ID];
    assert!(console.provide.is_empty() && console.native_use.is_empty());
    assert_eq!(console.call, names(CONSOLE_CALLS));
    assert_eq!(policy.principals.len(), COMPONENTS.len());
}

// Each entry is one delegated path that starts with a request from the first
// service's caller, then continues with requests that name the previous one as
// parent. The first caller of every path is NVHTTP.
const STREAM_PREFIX: [&str; 2] = ["amaterasu.rtsp/1", "amaterasu.stream-session/1"];

fn delegated_paths() -> Vec<Vec<&'static str>> {
    let mut paths: Vec<Vec<&'static str>> = Vec::new();
    for tail in [
        &["amaterasu.video/1", "amaterasu.encode/1"][..],
        &["amaterasu.video/1", "amaterasu.capture-kms/1"],
        &["amaterasu.media-transport/1"],
        &["amaterasu.control-transport/1"],
        &["amaterasu.session/1"],
        &["amaterasu.display/1"],
        &["amaterasu.display/1", "amaterasu.state/1"],
        &["amaterasu.virtual-display/1"],
        &["amaterasu.input/1"],
        &["amaterasu.audio/1"],
        &["amaterasu.audio-encode/1"],
    ] {
        let mut path = STREAM_PREFIX.to_vec();
        path.extend_from_slice(tail);
        paths.push(path);
    }
    paths.push(vec!["amaterasu.rtsp/1", "amaterasu.launch/1"]);
    for tail in [
        "amaterasu.catalog/1",
        "amaterasu.session/1",
        "amaterasu.application/1",
        "amaterasu.lutris/1",
    ] {
        paths.push(vec!["amaterasu.launch/1", tail]);
    }
    paths.push(vec![
        "amaterasu.launch/1",
        "amaterasu.catalog/1",
        "amaterasu.state/1",
    ]);
    for direct in [
        "amaterasu.catalog/1",
        "amaterasu.application/1",
        "amaterasu.session/1",
        "amaterasu.input/1",
        "amaterasu.pairing/1",
        "amaterasu.encode/1",
    ] {
        paths.push(vec![direct]);
    }
    for service in [
        "amaterasu.catalog/1",
        "amaterasu.steam/1",
        "amaterasu.lutris/1",
    ] {
        paths.push(vec!["amaterasu.library/1", service]);
    }
    paths.push(vec![
        "amaterasu.library/1",
        "amaterasu.catalog/1",
        "amaterasu.state/1",
    ]);
    paths.push(vec!["amaterasu.pairing/1", "amaterasu.state/1"]);
    paths.push(vec!["amaterasu.catalog/1", "amaterasu.state/1"]);
    paths
}

/// The caller of the first request is NVHTTP; each later caller is the provider
/// of the service its parent request named.
fn path_callers(path: &[&str]) -> Vec<&'static str> {
    let providers = providers();
    let mut callers = vec![NVHTTP_ID];
    for service in &path[..path.len() - 1] {
        callers.push(providers[service]);
    }
    callers
}

fn delegates(policy: &RuntimeConfig, path: &[&str]) -> bool {
    let mut allowed_services = policy.principals[NVHTTP_ID].call.clone();
    for (caller, service) in path_callers(path).into_iter().zip(path) {
        let grants = &policy.principals[caller];
        if !allowed_services.contains(*service) || !grants.call.contains(*service) {
            return false;
        }
        allowed_services = allowed_services
            .intersection(&grants.call)
            .cloned()
            .collect();
    }
    true
}

#[test]
fn linux_session_call_intersections_cover_the_whole_stream_and_launch_chain() {
    let policy = default_policy("intersections");
    for path in delegated_paths() {
        assert!(delegates(&policy, &path), "chain {path:?} is not delegated");
    }

    // Removing a grant from any ancestor must cut every path that needs it.
    for path in delegated_paths() {
        let callers = path_callers(&path);
        for (index, ancestor) in callers.iter().enumerate() {
            for service in &path[index..] {
                let mut narrowed = policy.clone();
                narrowed
                    .principals
                    .get_mut(*ancestor)
                    .unwrap()
                    .call
                    .remove(*service);
                assert!(
                    !delegates(&narrowed, &path),
                    "{ancestor} losing {service} must cut {path:?}"
                );
            }
        }
    }
}

#[test]
fn only_the_stream_chain_can_reach_capture_and_virtual_display() {
    let policy = default_policy("chain-only");
    let capture = "amaterasu.capture-kms/1";
    let virtual_display = "amaterasu.virtual-display/1";
    let capture_callers: BTreeSet<_> = policy
        .principals
        .iter()
        .filter(|(_, grants)| grants.call.contains(capture))
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        capture_callers,
        BTreeSet::from([
            NVHTTP_ID,
            RTSP_ID,
            STREAM_SESSION_ID,
            VIDEO_COMPONENT_ID,
            // Read-only output enumeration for the operator.
            "amaterasu.management",
        ])
    );
    let display_callers: BTreeSet<_> = policy
        .principals
        .iter()
        .filter(|(_, grants)| grants.call.contains(virtual_display))
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        display_callers,
        BTreeSet::from([NVHTTP_ID, RTSP_ID, STREAM_SESSION_ID])
    );
    for outsider in [
        "amaterasu.pairing",
        "amaterasu.catalog",
        "amaterasu.steam",
        "amaterasu.lutris",
        "amaterasu.launch",
        "amaterasu.application",
        "amaterasu.input",
        "amaterasu.display",
        CONSOLE_ID,
    ] {
        assert!(!policy.principals[outsider].call.contains(capture));
        assert!(!policy.principals[outsider].call.contains(virtual_display));
    }
    let native_users: BTreeSet<_> = policy
        .principals
        .iter()
        .filter(|(_, grants)| grants.native_use.contains(capture))
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(native_users, BTreeSet::from([VIDEO_COMPONENT_ID]));
}

#[test]
fn real_broker_delivers_every_delegated_request_and_enforces_each_ancestor() {
    fn submit(
        broker: &mut Broker,
        caller: &str,
        service: &str,
        operation_id: &str,
        parent: Option<u64>,
    ) -> Response {
        broker.dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload: json!({"action": "status"}),
                timeout_ms: 30_000,
                operation_id: operation_id.into(),
                parent,
            },
        )
    }

    /// Returns the failing step's error code, or None when every request was
    /// accepted and delivered to its provider with NVHTTP as the initiator.
    fn run(broker: &mut Broker, path: &[&str], label: &str) -> Option<String> {
        let providers = providers();
        let callers = path_callers(path);
        let mut parent = None;
        for (step, (caller, service)) in callers.into_iter().zip(path).enumerate() {
            let operation = format!("{label}-{step}");
            let id = match submit(broker, caller, service, &operation, parent) {
                Response::Accepted { id } => id,
                Response::Error { code, .. } => return Some(code),
                other => panic!("unexpected response {other:?}"),
            };
            let provider = providers[service];
            match broker.dispatch(provider, Command::Poll {}) {
                Response::Request {
                    id: delivered,
                    initiator,
                    ..
                } => {
                    assert_eq!(delivered, id, "{provider} received another request");
                    assert_eq!(initiator, NVHTTP_ID);
                }
                other => panic!("{provider} was not delivered {service}: {other:?}"),
            }
            parent = Some(id);
        }
        None
    }

    fn registered_broker(policy: RuntimeConfig) -> Broker {
        let mut broker = Broker::new(policy).unwrap();
        for component in COMPONENTS {
            for service in component.provides {
                assert!(
                    matches!(
                        broker.dispatch(
                            component.id,
                            Command::Register {
                                service: (*service).into()
                            }
                        ),
                        Response::Ok
                    ),
                    "{} could not register {service}",
                    component.id
                );
            }
        }
        broker
    }

    let policy = default_policy("broker");
    let mut broker = registered_broker(policy.clone());
    for (index, path) in delegated_paths().iter().enumerate() {
        assert_eq!(run(&mut broker, path, &format!("path{index}")), None);
    }

    // Remove the capture grant from each ancestor in turn: the broker itself
    // must reject the active capture enumeration.
    let capture_path = [
        "amaterasu.rtsp/1",
        "amaterasu.stream-session/1",
        "amaterasu.video/1",
        "amaterasu.capture-kms/1",
    ];
    for ancestor in [NVHTTP_ID, RTSP_ID, STREAM_SESSION_ID, VIDEO_COMPONENT_ID] {
        let mut narrowed = policy.clone();
        narrowed
            .principals
            .get_mut(ancestor)
            .unwrap()
            .call
            .remove("amaterasu.capture-kms/1");
        let mut broker = registered_broker(narrowed);
        assert_eq!(
            run(&mut broker, &capture_path, "narrowed").as_deref(),
            Some("permission_denied"),
            "{ancestor} without the grant must be denied"
        );
    }

    // Both the initiating NVHTTP and the immediate library caller need each
    // provider grant for parented artwork requests.
    for service in ["amaterasu.steam/1", "amaterasu.lutris/1"] {
        for ancestor in [NVHTTP_ID, LIBRARY.id] {
            let mut narrowed = policy.clone();
            narrowed
                .principals
                .get_mut(ancestor)
                .unwrap()
                .call
                .remove(service);
            let mut broker = registered_broker(narrowed);
            assert_eq!(
                run(
                    &mut broker,
                    &["amaterasu.library/1", service],
                    "artwork-narrowed"
                )
                .as_deref(),
                Some("permission_denied"),
                "{ancestor} without {service} must be denied"
            );
        }
    }

    for outsider in ["amaterasu.pairing", "amaterasu.catalog", CONSOLE_ID] {
        for service in ["amaterasu.capture-kms/1", "amaterasu.virtual-display/1"] {
            assert!(
                matches!(
                    submit(&mut broker, outsider, service, "outsider", None),
                    Response::Error { code, .. } if code == "permission_denied"
                ),
                "{outsider} reached {service}"
            );
        }
    }
}

#[test]
fn nvhttp_chain_grants_match_the_agreed_services_exactly() {
    let expected = |services: &[&str]| -> BTreeSet<String> {
        services
            .iter()
            .map(|service| (*service).to_owned())
            .collect()
    };
    assert_eq!(
        component(NVHTTP_ID).grants().call,
        expected(&[
            "amaterasu.configuration/1",
            "amaterasu.input/1",
            "amaterasu.pairing/1",
            "amaterasu.state/1",
            "amaterasu.catalog/1",
            "amaterasu.library/1",
            "amaterasu.steam/1",
            "amaterasu.launch/1",
            "amaterasu.display/1",
            "amaterasu.virtual-display/1",
            "amaterasu.lutris/1",
            "amaterasu.application/1",
            "amaterasu.rtsp/1",
            "amaterasu.stream-session/1",
            "amaterasu.session/1",
            "amaterasu.video/1",
            "amaterasu.audio/1",
            "amaterasu.audio-encode/1",
            "amaterasu.media-transport/1",
            "amaterasu.control-transport/1",
            "amaterasu.capture-kms/1",
            "amaterasu.capture-pipewire/1",
            "amaterasu.capture-vkms/1",
            "amaterasu.encode/1",
        ])
    );
    assert_eq!(
        component(RTSP_ID).grants().call,
        expected(&[
            "amaterasu.input/1",
            "amaterasu.stream-session/1",
            "amaterasu.session/1",
            "amaterasu.display/1",
            "amaterasu.virtual-display/1",
            "amaterasu.state/1",
            "amaterasu.launch/1",
            "amaterasu.video/1",
            "amaterasu.audio/1",
            "amaterasu.audio-encode/1",
            "amaterasu.media-transport/1",
            "amaterasu.control-transport/1",
            "amaterasu.capture-kms/1",
            "amaterasu.capture-pipewire/1",
            "amaterasu.capture-vkms/1",
            "amaterasu.encode/1",
        ])
    );
    assert_eq!(
        component(STREAM_SESSION_ID).grants().call,
        expected(&[
            "amaterasu.input/1",
            "amaterasu.session/1",
            "amaterasu.display/1",
            "amaterasu.virtual-display/1",
            "amaterasu.state/1",
            "amaterasu.video/1",
            "amaterasu.audio/1",
            "amaterasu.audio-encode/1",
            "amaterasu.media-transport/1",
            "amaterasu.control-transport/1",
            "amaterasu.capture-kms/1",
            "amaterasu.capture-pipewire/1",
            "amaterasu.capture-vkms/1",
            "amaterasu.encode/1",
            "amaterasu.session-history/1",
            "amaterasu.host-metrics/1",
            "amaterasu.frame-limiter/1",
        ])
    );
    assert_eq!(
        component(STREAM_SESSION_ID).grants().native_use,
        expected(&[
            "amaterasu.video/1",
            "amaterasu.audio/1",
            "amaterasu.audio-encode/1",
            "amaterasu.media-transport/1",
            "amaterasu.control-transport/1",
        ])
    );
    assert_eq!(
        component(VIDEO_COMPONENT_ID).grants().call,
        expected(&[
            "amaterasu.capture-kms/1",
            "amaterasu.capture-pipewire/1",
            "amaterasu.capture-vkms/1",
            "amaterasu.encode/1"
        ])
    );
    assert_eq!(
        component(NVHTTP_ID).grants().native_use,
        expected(&["amaterasu.identity/1", "amaterasu.network/1"])
    );
}

#[test]
fn effect_bearing_settings_default_to_disabled() {
    let policy = default_policy("defaults");
    let settings = &policy.settings;
    assert_eq!(settings[VIDEO_COMPONENT_ID]["enabled"], false);
    assert_eq!(
        settings[VIDEO_COMPONENT_ID]["capture_providers"],
        json!(["kms", "pipewire", "vkms"])
    );
    assert_eq!(settings["amaterasu.capture-kms"]["enabled"], false);
    assert_eq!(settings["amaterasu.audio"]["enabled"], false);
    assert_eq!(settings["amaterasu.virtual-display"]["enabled"], false);
    assert_eq!(settings["amaterasu.input"]["allow_injection"], false);
    assert_eq!(settings["amaterasu.display"]["allow_mutation"], false);
    assert_eq!(settings["amaterasu.stream-session"]["enable_input"], false);
    // Compute-only components are on so a client can read the codec snapshot.
    assert_eq!(settings["amaterasu.encode"]["enabled"], true);
    assert_eq!(settings["amaterasu.audio-encode"]["enabled"], true);
    // The management listener is configured when the root is initialised.
    assert!(!settings.contains_key("amaterasu.management"));
    assert!(!settings.contains_key("amaterasu.upnp"));
    assert!(!settings.contains_key("amaterasu.updater"));
}

#[test]
fn each_effect_option_enables_only_its_own_setting() {
    let enabled = |flag: &str| {
        let options = LinuxSessionOptions::from_arguments(&arguments(&[flag])).unwrap();
        let root = TemporaryRoot::new("effect-option");
        linux_session_policy(root.path(), &options)
            .unwrap()
            .settings
    };
    let baseline = default_policy("effect-baseline").settings;
    for (flag, changed) in [
        (
            "--enable-capture",
            vec!["amaterasu.capture-kms", VIDEO_COMPONENT_ID],
        ),
        ("--enable-audio", vec!["amaterasu.audio"]),
        (
            "--enable-virtual-display",
            vec![
                "amaterasu.capture-pipewire",
                "amaterasu.nvhttp",
                "amaterasu.virtual-display",
            ],
        ),
        (
            "--enable-input",
            vec!["amaterasu.input", "amaterasu.stream-session"],
        ),
        ("--enable-display-mutation", vec!["amaterasu.display"]),
    ] {
        let settings = enabled(flag);
        // State locations follow the temporary root, which differs per call.
        let differing: Vec<_> = baseline
            .keys()
            .filter(|key| !["amaterasu.state", "amaterasu.session-history"].contains(&key.as_str()))
            .filter(|key| settings[*key] != baseline[*key])
            .map(String::as_str)
            .collect();
        assert_eq!(differing, changed, "{flag}");
    }
    let settings = enabled("--enable-input");
    assert_eq!(settings["amaterasu.input"]["allow_injection"], true);
    assert_eq!(settings["amaterasu.stream-session"]["enable_input"], true);
}

#[test]
fn stream_wiring_names_only_the_next_service_in_the_chain() {
    let settings = default_policy("wiring").settings;
    assert_eq!(
        settings["amaterasu.rtsp"]["allowed_principals"],
        json!(["amaterasu.nvhttp"])
    );
    assert_eq!(
        settings["amaterasu.stream-session"]["allowed_principals"],
        json!(["amaterasu.rtsp"])
    );
    for transport in ["amaterasu.media-transport", "amaterasu.control-transport"] {
        assert_eq!(
            settings[transport]["allowed_principals"],
            json!(["amaterasu.stream-session"])
        );
    }
    assert_eq!(
        settings["amaterasu.pairing"]["transport_principals"],
        json!(["amaterasu.nvhttp"])
    );
    assert_eq!(
        settings["amaterasu.catalog"]["export_principals"],
        json!({"amaterasu.nvhttp": "amaterasu.management", "amaterasu.management": "amaterasu.management"})
    );
    assert_eq!(
        settings["amaterasu.catalog"]["launch_principals"],
        json!(["amaterasu.launch"])
    );
    assert!(
        settings["amaterasu.catalog"]["import_principals"]
            .as_object()
            .unwrap()
            .keys()
            .all(|importer| ["amaterasu.steam", "amaterasu.lutris"].contains(&importer.as_str()))
    );
    let console =
        LinuxSessionOptions::from_arguments(&arguments(&["--catalog-owner", "console"])).unwrap();
    assert_eq!(console.catalog_owner, CatalogOwner::Console);
    let root = TemporaryRoot::new("console-owner");
    let settings = linux_session_policy(root.path(), &console)
        .unwrap()
        .settings;
    assert_eq!(
        settings["amaterasu.catalog"]["export_principals"],
        json!({"amaterasu.nvhttp": "amaterasu.console", "amaterasu.management": "amaterasu.console"})
    );
}

#[test]
fn streaming_profile_defaults_to_portal_and_supports_hardware_modes() {
    let settings = default_policy("streaming").settings;
    let streaming = &settings["amaterasu.nvhttp"]["streaming"];
    assert_eq!(streaming["capture"]["capture_provider"], "pipewire");
    assert_eq!(
        streaming["capture"]["output"],
        "portal-selected-session-source"
    );
    assert_eq!(streaming["capture"]["encoder"], "auto");
    assert_eq!(streaming["profile"]["max_width"], 3840);
    assert_eq!(streaming["profile"]["max_height"], 2160);
    assert_eq!(streaming["profile"]["max_fps"], 120);
    assert_eq!(streaming["profile"]["encrypt_video"], true);
    assert_eq!(streaming["profile"]["encrypt_audio"], true);
}

/// The two fields that decide who may mint a management session. Edit them and
/// `AUTHORIZATION_POLICY_GENERATION` together, then update this pin.
#[test]
fn authorization_generation_is_bumped_with_principals_and_scopes() {
    let pinned_principals = ["amaterasu.console"];
    let pinned_scopes = [
        "account.self",
        "catalog.read",
        "catalog.write",
        "configuration.read",
        "configuration.write",
        "session.read",
        "session.write",
        "application.read",
        "application.launch",
        "application.terminate",
        "steam.read",
        "steam.refresh",
        "lutris.read",
        "host-logs.read",
        "tokens.manage",
        "pairing.admin",
        "display.read",
        "display.write",
        "capture.read",
        "capture.write",
        "media.read",
    ];
    let settings = default_policy("generation").settings;
    let authentication = &settings["amaterasu.authentication"];
    assert_eq!(
        authentication["administrator_principals"],
        json!(pinned_principals)
    );
    assert_eq!(
        authentication["legacy_verifier_principals"],
        json!(["amaterasu.management"])
    );
    assert_eq!(
        settings["amaterasu.pairing"]["administrator_principals"],
        json!(["amaterasu.console", "amaterasu.management"])
    );
    assert_eq!(authentication["allowed_scopes"], json!(pinned_scopes));
    assert_eq!(authentication["authorization_policy_generation"], 15);

    // Generations are profile-specific and never reused.
    let development_root = TemporaryRoot::new("generation-development");
    let development = development_policy(development_root.path()).unwrap();
    let installer = installer_policy("__WEB__", BTreeMap::new());
    let generation = |policy: &RuntimeConfig| {
        policy.settings["amaterasu.authentication"]["authorization_policy_generation"]
            .as_u64()
            .unwrap()
    };
    let generations = [
        generation(&development),
        generation(&installer),
        AUTHORIZATION_POLICY_GENERATION,
    ];
    assert_eq!(generations.iter().collect::<BTreeSet<_>>().len(), 3);
    assert!(generations.iter().all(|generation| *generation > 0));

    // TTL fields are separate from the generation rule.
    assert_eq!(authentication["session_token_ttl_seconds"], 7200);
    assert_eq!(
        authentication["remember_me_refresh_token_ttl_seconds"],
        604800
    );
}

#[cfg(unix)]
#[test]
fn persistent_state_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = TemporaryRoot::new("private");
    let policy = linux_session_policy(root.path(), &LinuxSessionOptions::default()).unwrap();
    let state = Path::new(
        policy.settings["amaterasu.state"]["data_directory"]
            .as_str()
            .unwrap(),
    );
    let history_database = Path::new(
        policy.settings["amaterasu.session-history"]["database_path"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        state,
        root.path().join("data/state").canonicalize().unwrap()
    );
    let history = history_database.parent().unwrap();
    assert_eq!(history, state.join("session-history"));
    for directory in [
        root.path().join("data"),
        state.to_path_buf(),
        history.to_path_buf(),
    ] {
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", directory.display());
    }
}

#[test]
fn initialised_root_carries_the_policy_and_enables_management_only_on_request() {
    let root = TemporaryRoot::new("initialise");
    let default_options = LinuxSessionOptions::default();
    crate::development::initialize_linux_session(root.path(), &default_options).unwrap();
    let bootstrap: amaterasu_package::BootstrapConfig =
        amaterasu_package::read_json(&root.path().join("bootstrap.json")).unwrap();
    assert!(bootstrap.development && !bootstrap.allow_unsigned);
    let management = &bootstrap.policy.settings["amaterasu.management"];
    assert_eq!(management["enabled"], false);
    assert!(management["web"]["directory"].is_string());
    assert!(management.get("bind").is_none());
    assert!(bootstrap.policy.principals.contains_key(VIDEO_COMPONENT_ID));
    assert!(
        !bootstrap
            .policy
            .principals
            .contains_key("amaterasu.input-probe")
    );
    assert!(
        crate::development::initialize_linux_session(root.path(), &default_options)
            .unwrap_err()
            .contains("already initialized")
    );

    let managed = TemporaryRoot::new("initialise-management");
    let options =
        LinuxSessionOptions::from_arguments(&arguments(&["--management-port", "47990"])).unwrap();
    crate::development::initialize_linux_session(managed.path(), &options).unwrap();
    let bootstrap: amaterasu_package::BootstrapConfig =
        amaterasu_package::read_json(&managed.path().join("bootstrap.json")).unwrap();
    let management = &bootstrap.policy.settings["amaterasu.management"];
    assert_eq!(management["enabled"], true);
    assert_eq!(management["bind"], "127.0.0.1:47990");
}

#[test]
fn the_policy_round_trips_through_bootstrap_serialisation() {
    let policy = default_policy("round-trip");
    let reloaded: RuntimeConfig =
        serde_json::from_value(serde_json::to_value(&policy).unwrap()).unwrap();
    assert_eq!(reloaded.principals.len(), policy.principals.len());
    assert_eq!(reloaded.settings, policy.settings);
}
