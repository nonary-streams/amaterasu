use super::*;
use serde_json::json;

fn smoke_settings(output: &str) -> Value {
    json!({
        "amaterasu.nvhttp": {
            "enabled": true,
            "streaming": {
                "desktop": {"enabled": true},
                "capture": {"capture_provider": "dxgi", "output": output}
            }
        },
        "amaterasu.rtsp": {"enabled": true},
        "amaterasu.stream-session": {"enabled": true},
        "amaterasu.video": {"enabled": true, "capture_providers": ["dxgi"]},
        "amaterasu.capture-dxgi": {"enabled": true},
        "amaterasu.encode": {"enabled": true},
        "amaterasu.audio": {"enabled": true},
        "amaterasu.audio-encode": {"enabled": true},
        "amaterasu.media-transport": {"enabled": true},
        "amaterasu.control-transport": {"enabled": true},
        "amaterasu.input": {"allow_injection": true, "windows_backend": "vhf_companion"}
    })
}

fn all_component_ids() -> BTreeSet<String> {
    PackageProfile::DesktopStreamSmoke
        .components()
        .iter()
        .map(|component| component.id.to_owned())
        .collect()
}

fn facts<'a>(settings: &'a Value, components: &'a BTreeSet<String>) -> StaticFacts<'a> {
    StaticFacts {
        active_set: true,
        verified_set: true,
        components,
        settings,
        input_policy_present: false,
        vhf_package_missing: None,
    }
}

#[test]
fn configured_desktop_profile_can_attempt_pairing_and_media_but_never_claims_input_ready() {
    let settings = smoke_settings("\\\\?\\DISPLAY#CURRENT");
    let components = all_component_ids();
    let report = assess(&facts(&settings, &components));

    assert_eq!(report.pairing, AttemptStatus::CanAttempt);
    assert_eq!(report.media, AttemptStatus::ReadyToAttempt);
    assert_eq!(report.input, AttemptStatus::Unavailable);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("current attachment was not checked"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("machine-owned input-companion-policy.json is absent"))
    );
}

#[test]
fn placeholder_output_blocks_media_without_preventing_a_pairing_attempt() {
    let settings = smoke_settings(OUTPUT_PLACEHOLDER);
    let components = all_component_ids();
    let report = assess(&facts(&settings, &components));

    assert_eq!(report.pairing, AttemptStatus::CanAttempt);
    assert_eq!(report.media, AttemptStatus::NotReady);
    assert_eq!(report.input, AttemptStatus::Unavailable);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("selection is missing or still SELECT_OUTPUT_ID"))
    );
}

#[test]
fn missing_media_provider_is_reported_from_component_inventory() {
    let settings = smoke_settings("display-id");
    let mut components = all_component_ids();
    components.remove("amaterasu.encode");
    let report = assess(&facts(&settings, &components));

    assert_eq!(report.pairing, AttemptStatus::CanAttempt);
    assert_eq!(report.media, AttemptStatus::NotReady);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("amaterasu.encode"))
    );
}

#[test]
fn reports_identical_explicit_listener_addresses_without_probing_live_sockets() {
    let settings = json!({
        "amaterasu.nvhttp": {
            "http_bind": "127.0.0.1:47989",
            "https_bind": "127.0.0.1:47989"
        },
        "amaterasu.rtsp": {"bind": "127.0.0.1:48010"}
    });
    let notes = static_listener_conflicts(&settings);

    assert!(
        notes
            .iter()
            .any(|note| note.contains("Static listener conflict"))
    );
}

#[test]
fn no_active_set_does_not_claim_pairing_or_media_attempt_readiness() {
    let settings = smoke_settings("display-id");
    let components = all_component_ids();
    let mut facts = facts(&settings, &components);
    facts.active_set = false;
    let report = assess(&facts);

    assert_eq!(report.pairing, AttemptStatus::NotReady);
    assert_eq!(report.media, AttemptStatus::NotReady);
    assert_eq!(report.input, AttemptStatus::Unavailable);
}

#[test]
fn names_missing_vhf_payload_files_as_static_input_prerequisites() {
    let settings = smoke_settings("display-id");
    let components = all_component_ids();
    let mut facts = facts(&settings, &components);
    facts.vhf_package_missing = Some(vec!["driver/AmaterasuVhfInput.cat"]);
    let report = assess(&facts);

    assert_eq!(report.input, AttemptStatus::Unavailable);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("driver/AmaterasuVhfInput.cat"))
    );
}
