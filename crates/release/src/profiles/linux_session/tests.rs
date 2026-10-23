use super::*;

#[test]
fn profile_uses_owner_selected_pipewire_capture_without_kms_or_metrics_modules() {
    let capture = COMPONENTS
        .iter()
        .find(|component| component.id == "amaterasu.capture-pipewire")
        .expect("PipeWire capture component");
    assert_eq!(capture.library, "amaterasu_capture_pipewire");
    assert!(
        capture
            .native_provides
            .contains(&"amaterasu.capture-pipewire/1")
    );
    assert!(capture.dependencies.contains(&RUNTIME_ID));

    let video = COMPONENTS
        .iter()
        .find(|component| component.id == VIDEO_COMPONENT_ID)
        .expect("Linux video component");
    assert!(video.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(video.native_uses.contains(&"amaterasu.capture-pipewire/1"));
    assert!(video.dependencies.contains(&"amaterasu.capture-pipewire"));

    // Nested broker requests intersect the call grants at every ancestor, but
    // only video needs direct native-use authority for frame capture.
    for component_id in [NVHTTP.id, RTSP.id, STREAM_SESSION.id] {
        let component = COMPONENTS
            .iter()
            .find(|component| component.id == component_id)
            .expect("stream ancestor component");
        assert!(component.calls.contains(&"amaterasu.capture-pipewire/1"));
        assert!(
            !component
                .native_uses
                .contains(&"amaterasu.capture-pipewire/1")
        );
    }

    let management = COMPONENTS
        .iter()
        .find(|component| component.id == MANAGEMENT.id)
        .expect("Linux management component");
    assert!(management.calls.contains(&"amaterasu.capture-pipewire/1"));
    assert!(ALLOWED_SCOPES.contains(&"capture.read"));
    assert!(ALLOWED_SCOPES.contains(&"capture.write"));
    assert_eq!(
        portal_capture_settings(),
        json!({
            "enabled": true,
            "selection_principals": ["amaterasu.management"],
            "capture_principals": [VIDEO_COMPONENT_ID],
        })
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["capture"]["capture_provider"],
        "pipewire"
    );
    assert_eq!(
        streaming_settings(&LinuxSessionOptions::default())["desktop"]["enabled"],
        true
    );
    assert_eq!(
        catalog_settings(&LinuxSessionOptions::default())["seed_desktop_entry"],
        false
    );
    assert!(!COMPONENTS.iter().any(|component| matches!(
        component.id,
        "amaterasu.capture-kms" | "amaterasu.host-metrics"
    )));
    assert!(
        !PackageProfile::Platform
            .components()
            .iter()
            .any(|component| component.id == capture.id)
    );
}
