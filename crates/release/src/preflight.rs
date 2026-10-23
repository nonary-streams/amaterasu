//! Read-only diagnostics for the development desktop-stream smoke profile.
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, package_directory,
    read_json, unix_timestamp, verify_package,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use crate::profiles::{PackageProfile, RUNTIME_ID};

const OUTPUT_PLACEHOLDER: &str = "SELECT_OUTPUT_ID";
const INPUT_POLICY_FILE: &str = "input-companion-policy.json";
const VHF_PACKAGE_FILES: [&str; 5] = [
    "manifest.json",
    "driver/AmaterasuVhfInput.inf",
    "driver/AmaterasuVhfInput.dll",
    "driver/AmaterasuVhfInput.cat",
    "amaterasu-input-companion.exe",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttemptStatus {
    CanAttempt,
    ReadyToAttempt,
    NotReady,
    Unavailable,
}

struct PreflightReport {
    pairing: AttemptStatus,
    media: AttemptStatus,
    input: AttemptStatus,
    notes: Vec<String>,
}

struct StaticFacts<'a> {
    active_set: bool,
    verified_set: bool,
    components: &'a BTreeSet<String>,
    settings: &'a Value,
    input_policy_present: bool,
    vhf_package_missing: Option<Vec<&'static str>>,
}

/// Inspect static profile state. This never opens listeners or starts services.
pub fn run(root: &Path, vhf_install_root: Option<&Path>) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    let selection = match root.join("selection.json").try_exists() {
        Ok(true) => read_json::<ActivationState>(&root.join("selection.json"))?,
        Ok(false) => ActivationState::default(),
        Err(error) => return Err(error.to_string()),
    };

    let active_digest = selection.active.as_deref();
    let pending_digest = selection.pending.as_deref();
    let selected_digest = active_digest.or(pending_digest);
    let mut component_ids = BTreeSet::new();
    let mut package_verified = false;
    let mut package_error = None;

    if let Some(digest) = selected_digest {
        let package_result = package_directory(root, digest).and_then(|package_path| {
            verify_package(
                &package_path,
                &bootstrap,
                VerificationPolicy {
                    minimum_sequence: 0,
                    allow_historical: true,
                    now: unix_timestamp(),
                },
            )
        });
        match package_result {
            Ok(verified) => {
                package_verified = true;
                component_ids.extend(
                    verified
                        .manifest
                        .components
                        .into_iter()
                        .map(|component| component.id),
                );
                println!(
                    "Selected package: verified {} ({})",
                    verified.digest,
                    if active_digest.is_some() {
                        "active"
                    } else {
                        "pending, not active"
                    }
                );
            }
            Err(error) => package_error = Some(error),
        }
    } else {
        println!("Selected package: none (no active or pending set)");
    }

    let input_policy_present = match root.join(INPUT_POLICY_FILE).try_exists() {
        Ok(present) => present,
        Err(error) => {
            println!("VHF policy file: cannot inspect: {error}");
            false
        }
    };
    let vhf_package_missing = vhf_install_root.map(inspect_vhf_package_files);
    let policy_settings =
        serde_json::to_value(&bootstrap.policy.settings).map_err(|error| error.to_string())?;
    let facts = StaticFacts {
        active_set: active_digest.is_some(),
        verified_set: package_verified,
        components: &component_ids,
        settings: &policy_settings,
        input_policy_present,
        vhf_package_missing,
    };
    let report = assess(&facts);

    if let Some(error) = package_error {
        println!("Selected package verification: FAILED ({error})");
    }
    println!("Pairing: {}", label(report.pairing));
    println!("Media: {}", label(report.media));
    println!("Input: {}", label(report.input));
    for note in report.notes {
        println!("- {note}");
    }
    println!(
        "- Live DXGI output/provider availability: NOT VERIFIED (no service was started or queried)."
    );
    println!(
        "- Listener reachability and OS port conflicts: NOT VERIFIED (no socket bind probe was performed)."
    );
    println!("- Installed VHF driver/service and effective input grants: NOT VERIFIED.");
    println!(
        "- NVHTTP runtime acceptance of the desktop setting is NOT VERIFIED by the release CLI."
    );
    Ok(())
}

fn assess(facts: &StaticFacts<'_>) -> PreflightReport {
    let profile_components = PackageProfile::DesktopStreamSmoke.components();
    let missing: Vec<_> = profile_components
        .iter()
        .filter(|component| !facts.components.contains(component.id))
        .map(|component| component.id.to_owned())
        .collect();
    let media_missing: Vec<_> = profile_components
        .iter()
        .filter(|component| component.id != "amaterasu.input")
        .filter(|component| !facts.components.contains(component.id))
        .map(|component| component.id.to_owned())
        .collect();
    let mut notes = Vec::new();

    if !facts.verified_set {
        notes.push(
            "No selected component set was verified; provider availability cannot be established."
                .into(),
        );
    } else if missing.is_empty() {
        notes.push(
            "All DesktopStreamSmoke component artifacts are present in the verified set.".into(),
        );
    } else {
        notes.push(format!(
            "Required components absent from the verified set: {}.",
            missing.join(", ")
        ));
    }

    let nvhttp = setting(facts.settings, "amaterasu.nvhttp");
    let pairing_providers_present = [
        RUNTIME_ID,
        "amaterasu.configuration",
        "amaterasu.pairing",
        "amaterasu.state",
        "amaterasu.nvhttp",
    ]
    .iter()
    .all(|id| facts.components.contains(*id));
    let pairing_configured = nvhttp.get("enabled").and_then(Value::as_bool) == Some(true);
    let pairing = if facts.active_set
        && facts.verified_set
        && pairing_providers_present
        && pairing_configured
    {
        notes.push("Pairing prerequisites are static only; existing host identity, TLS listener, and client authorization were not queried.".into());
        AttemptStatus::CanAttempt
    } else {
        notes.push("Pairing cannot be confirmed: an active verified NVHTTP/pairing set and enabled NVHTTP setting are required.".into());
        AttemptStatus::NotReady
    };

    let output = nvhttp
        .pointer("/streaming/capture/output")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|output| !output.is_empty() && *output != OUTPUT_PLACEHOLDER);
    if output.is_none() {
        notes.push("DXGI output selection is missing or still SELECT_OUTPUT_ID.".into());
    } else {
        notes.push(format!(
            "DXGI output selection is configured as {}; current attachment was not checked.",
            output.unwrap_or_default()
        ));
    }

    let required_settings = [("/enabled", true), ("/streaming/desktop/enabled", true)];
    let desktop_configured = required_settings
        .iter()
        .all(|(path, expected)| nvhttp.pointer(path).and_then(Value::as_bool) == Some(*expected))
        && nvhttp
            .pointer("/streaming/capture/capture_provider")
            .and_then(Value::as_str)
            == Some("dxgi");
    let media_settings_present = [
        ("amaterasu.capture-dxgi", "/enabled"),
        ("amaterasu.encode", "/enabled"),
        ("amaterasu.video", "/enabled"),
        ("amaterasu.audio", "/enabled"),
        ("amaterasu.audio-encode", "/enabled"),
        ("amaterasu.media-transport", "/enabled"),
        ("amaterasu.control-transport", "/enabled"),
        ("amaterasu.stream-session", "/enabled"),
        ("amaterasu.rtsp", "/enabled"),
    ]
    .iter()
    .all(|(component, path)| {
        setting(facts.settings, component)
            .pointer(path)
            .and_then(Value::as_bool)
            == Some(true)
    }) && setting(facts.settings, "amaterasu.video")
        .get("capture_providers")
        .and_then(Value::as_array)
        .is_some_and(|providers| providers.iter().any(|provider| provider == "dxgi"));
    let media = if facts.active_set
        && facts.verified_set
        && media_missing.is_empty()
        && desktop_configured
        && media_settings_present
        && output.is_some()
    {
        notes.push("Static media profile is ready to attempt; hardware, listener, encoder, and runtime schema behavior remain unverified.".into());
        AttemptStatus::ReadyToAttempt
    } else {
        if !media_missing.is_empty() {
            notes.push(format!(
                "Media providers absent from the verified set: {}.",
                media_missing.join(", ")
            ));
        }
        if !desktop_configured || !media_settings_present {
            notes.push("One or more required desktop, DXGI/H.264, audio, transport, or session settings are missing or disabled.".into());
        }
        AttemptStatus::NotReady
    };

    let input_settings = setting(facts.settings, "amaterasu.input");
    let input_backend = input_settings
        .get("windows_backend")
        .and_then(Value::as_str)
        == Some("vhf_companion")
        && input_settings
            .get("allow_injection")
            .and_then(Value::as_bool)
            == Some(true);
    let mut input_prerequisites = Vec::new();
    if !facts.components.contains("amaterasu.input") {
        input_prerequisites.push("input provider is absent from the verified set".to_owned());
    }
    if !input_backend {
        input_prerequisites
            .push("vhf_companion backend is not selected/enabled in static policy".to_owned());
    }
    if let Some(missing_files) = &facts.vhf_package_missing {
        if !missing_files.is_empty() {
            input_prerequisites.push(format!(
                "VHF package payload files are absent: {}",
                missing_files.join(", ")
            ));
        } else {
            notes.push("VHF package payload files are present; signatures and package identity were not verified.".into());
        }
    } else {
        input_prerequisites.push(
            "VHF install root was not supplied; package payload presence is unknown".to_owned(),
        );
    }
    if !facts.input_policy_present {
        input_prerequisites
            .push("machine-owned input-companion-policy.json is absent or unreadable".to_owned());
    } else {
        notes.push("VHF input policy file is present; its selected-set binding and effective grants were not verified.".into());
    }
    notes.push(format!(
        "Input prerequisites: {}.",
        input_prerequisites.join("; ")
    ));

    // Static files cannot establish that the VHF service/device is installed,
    // that the policy is effective, or that the provider can deliver events.
    let input = AttemptStatus::Unavailable;
    let listener_notes = static_listener_conflicts(facts.settings);
    notes.extend(listener_notes);
    PreflightReport {
        pairing,
        media,
        input,
        notes,
    }
}

fn setting<'a>(settings: &'a Value, component: &str) -> &'a Value {
    settings.get(component).unwrap_or(&Value::Null)
}

fn inspect_vhf_package_files(root: &Path) -> Vec<&'static str> {
    VHF_PACKAGE_FILES
        .iter()
        .copied()
        .filter(|relative| {
            let path = root.join(relative);
            !fs::symlink_metadata(path)
                .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                .unwrap_or(false)
        })
        .collect()
}

fn static_listener_conflicts(settings: &Value) -> Vec<String> {
    let configured = [
        (
            "NVHTTP HTTP",
            settings
                .get("amaterasu.nvhttp")
                .and_then(|value| value.get("http_bind")),
        ),
        (
            "NVHTTP HTTPS",
            settings
                .get("amaterasu.nvhttp")
                .and_then(|value| value.get("https_bind")),
        ),
        (
            "RTSP",
            settings
                .get("amaterasu.rtsp")
                .and_then(|value| value.get("bind")),
        ),
    ];
    let mut endpoints = Vec::<(&str, SocketAddr)>::new();
    for (name, value) in configured {
        let Some(address) = value
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok())
        else {
            continue;
        };
        endpoints.push((name, address));
    }
    let mut notes = Vec::new();
    for left in 0..endpoints.len() {
        for right in left + 1..endpoints.len() {
            if endpoints[left].1 == endpoints[right].1 {
                notes.push(format!(
                    "Static listener conflict: {} and {} both specify {}.",
                    endpoints[left].0, endpoints[right].0, endpoints[left].1
                ));
            }
        }
    }
    if notes.is_empty() {
        notes.push("No duplicate explicit listener socket addresses were found; omitted defaults and live port occupancy were not checked.".into());
    }
    notes
}

fn label(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::CanAttempt => "CAN ATTEMPT (static prerequisites only)",
        AttemptStatus::ReadyToAttempt => "READY TO ATTEMPT (static media prerequisites only)",
        AttemptStatus::NotReady => "NOT READY TO ATTEMPT",
        AttemptStatus::Unavailable => "UNAVAILABLE / NOT VERIFIED",
    }
}

#[cfg(test)]
mod tests;
