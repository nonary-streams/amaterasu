//! Conversion of installed policies that predate processing compositions.
//!
//! Earlier packages stored RTX HDR wiring inside provider settings. Current
//! providers read `processing_compositions` and reject those fields at startup,
//! so such a host needs its policy converted before it can load a current set.
//! This runs only inside the reviewed activation flow. It removes exactly the
//! fields the selected composition recreates; capture, frame-lease, runtime and
//! every other host-specific setting is carried over untouched.
use super::*;

const LEGACY_PROCESSOR_SERVICE: &str = "amaterasu.rtx-hdr/1";
const STREAM_SESSION: &str = "amaterasu.stream-session";
const VIDEO_WIRING: [&str; 2] = ["gpu_processor_service", "gpu_processor_opt_in"];
const STREAM_SESSION_WIRING: [&str; 1] = ["processing_plan"];
const LIFECYCLE_WIRING: [&str; 1] = ["processing_policy"];
const RTX_HDR_WIRING: [&str; 5] = [
    "display_settings_service",
    "extension_registry_principal",
    "foreground_context_service",
    "lifecycle_coordinator",
    "processing_consumer_principals",
];

/// Build the policy a host should run with RTX HDR selected through a
/// composition, converting older wiring first when the policy still holds it.
/// Returns whether a conversion happened, so a review can record that fact.
/// Errors leave `policy` unchanged.
pub fn compose_rtx_hdr_destination(
    policy: &mut RuntimeConfig,
    runtime_directory: &Path,
    selection: &ProcessingSelection,
) -> PackageResult<bool> {
    let mut candidate = policy.clone();
    let converted = remove_legacy_rtx_wiring(&mut candidate)?;
    enable_rtx_hdr_with_selection(&mut candidate, runtime_directory, selection)?;
    *policy = candidate;
    Ok(converted)
}

fn remove_legacy_rtx_wiring(policy: &mut RuntimeConfig) -> PackageResult<bool> {
    if !holds_legacy_rtx_wiring(policy) {
        return Ok(false);
    }
    reject_other_processor(policy)?;
    remove_fields(policy, VIDEO_COMPONENT_ID, &VIDEO_WIRING);
    remove_fields(policy, STREAM_SESSION, &STREAM_SESSION_WIRING);
    remove_fields(policy, SESSION_LIFECYCLE.id, &LIFECYCLE_WIRING);
    remove_fields(policy, RTX_HDR_COMPONENT.id, &RTX_HDR_WIRING);
    remove_provider_entries(policy, SESSION_LIFECYCLE.id, "participants", "/provider");
    remove_provider_entries(
        policy,
        EXTENSION_REGISTRY.id,
        "descriptors",
        "/descriptor/provider",
    );
    policy
        .api_methods
        .retain(|approval| approval.provider != RTX_HDR_COMPONENT.id);
    Ok(true)
}

fn holds_legacy_rtx_wiring(policy: &RuntimeConfig) -> bool {
    [
        (VIDEO_COMPONENT_ID, VIDEO_WIRING.as_slice()),
        (STREAM_SESSION, STREAM_SESSION_WIRING.as_slice()),
        (RTX_HDR_COMPONENT.id, RTX_HDR_WIRING.as_slice()),
    ]
    .into_iter()
    .any(|(principal, fields)| {
        policy
            .settings
            .get(principal)
            .is_some_and(|settings| fields.iter().any(|field| settings.get(field).is_some()))
    })
}

/// Only RTX HDR's own wiring is converted. A policy that selected some other
/// processor needs its own reviewed composition, not a silent replacement.
fn reject_other_processor(policy: &RuntimeConfig) -> PackageResult<()> {
    let expected_selections = [
        (
            VIDEO_COMPONENT_ID,
            "/gpu_processor_service",
            LEGACY_PROCESSOR_SERVICE,
        ),
        (
            STREAM_SESSION,
            "/processing_plan/provider",
            RTX_HDR_COMPONENT.id,
        ),
        (
            STREAM_SESSION,
            "/processing_plan/processor_service",
            LEGACY_PROCESSOR_SERVICE,
        ),
        (
            SESSION_LIFECYCLE.id,
            "/processing_policy/provider",
            RTX_HDR_COMPONENT.id,
        ),
        (
            SESSION_LIFECYCLE.id,
            "/processing_policy/processor_service",
            LEGACY_PROCESSOR_SERVICE,
        ),
    ];
    for (principal, pointer, expected) in expected_selections {
        let selected = policy
            .settings
            .get(principal)
            .and_then(|settings| settings.pointer(pointer));
        if selected.is_some_and(|value| value.as_str() != Some(expected)) {
            return Err("installed policy selects a different processor".into());
        }
    }
    Ok(())
}

fn remove_fields(policy: &mut RuntimeConfig, principal: &str, fields: &[&str]) {
    if let Some(settings) = policy
        .settings
        .get_mut(principal)
        .and_then(Value::as_object_mut)
    {
        for field in fields {
            settings.remove(*field);
        }
    }
}

fn remove_provider_entries(
    policy: &mut RuntimeConfig,
    principal: &str,
    field: &str,
    provider_pointer: &str,
) {
    if let Some(entries) = policy
        .settings
        .get_mut(principal)
        .and_then(|settings| settings.get_mut(field))
        .and_then(Value::as_array_mut)
    {
        entries.retain(|entry| {
            entry.pointer(provider_pointer).and_then(Value::as_str) != Some(RTX_HDR_COMPONENT.id)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::random_identifier;

    struct Fixture {
        base: std::path::PathBuf,
        runtime_directory: std::path::PathBuf,
        current: RuntimeConfig,
    }

    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "amaterasu-legacy-rtx-{}",
                random_identifier().unwrap()
            ));
            std::fs::create_dir_all(&base).unwrap();
            let base = base.canonicalize().unwrap();
            let runtime_directory = base.join("data/state/rtx-hdr/runtime");
            let current = linux_session_policy(
                &base,
                &LinuxSessionOptions {
                    frame_helper: Some("/usr/libexec/amaterasu-frame-helper".into()),
                    rtx_hdr_runtime_directory: Some(runtime_directory.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            Self {
                base,
                runtime_directory,
                current,
            }
        }

        /// The current policy, rewritten into the form installed packages used.
        fn legacy(&self) -> RuntimeConfig {
            let mut legacy = self.current.clone();
            legacy.processing_compositions.clear();
            let rtx = RTX_HDR_COMPONENT.id;
            let mut set = |principal: &str, field: &str, value: Value| {
                legacy
                    .settings
                    .entry(principal.into())
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .unwrap()
                    .insert(field.into(), value);
            };
            set(
                VIDEO_COMPONENT_ID,
                "gpu_processor_service",
                json!(LEGACY_PROCESSOR_SERVICE),
            );
            set(VIDEO_COMPONENT_ID, "gpu_processor_opt_in", json!(true));
            set(VIDEO_COMPONENT_ID, "require_frame_lease", json!(true));
            set(
                STREAM_SESSION,
                "processing_plan",
                json!({"provider": rtx, "processor_service": LEGACY_PROCESSOR_SERVICE}),
            );
            set(
                SESSION_LIFECYCLE.id,
                "participants",
                json!([{"provider": rtx}, {"provider": "amaterasu.frame-limiter"}]),
            );
            set(
                SESSION_LIFECYCLE.id,
                "processing_policy",
                json!({"provider": rtx, "processor_service": LEGACY_PROCESSOR_SERVICE}),
            );
            set(
                rtx,
                "foreground_context_service",
                json!("amaterasu.foreground-context/1"),
            );
            set(
                rtx,
                "lifecycle_coordinator",
                json!({"principal": "x", "service": "y"}),
            );
            set(
                rtx,
                "processing_consumer_principals",
                json!(["amaterasu.stream-session"]),
            );
            legacy
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn conversion_replaces_settings_wiring_with_the_composition() {
        let fixture = Fixture::new();
        let mut policy = fixture.legacy();
        let selection = default_rtx_hdr_selection();
        let converted =
            compose_rtx_hdr_destination(&mut policy, &fixture.runtime_directory, &selection)
                .unwrap();
        assert!(converted);
        let rtx = &policy.settings[RTX_HDR_COMPONENT.id];
        for field in RTX_HDR_WIRING {
            assert!(rtx.get(field).is_none(), "{field} survived");
        }
        let video = &policy.settings[VIDEO_COMPONENT_ID];
        for field in VIDEO_WIRING {
            assert!(video.get(field).is_none(), "{field} survived");
        }
        assert_eq!(video["require_frame_lease"], true);
        assert!(
            policy.settings[STREAM_SESSION]
                .get("processing_plan")
                .is_none()
        );
        let lifecycle = &policy.settings[SESSION_LIFECYCLE.id];
        assert_eq!(
            lifecycle["participants"],
            json!([{"provider": "amaterasu.frame-limiter"}])
        );
        assert!(lifecycle.get("processing_policy").is_none());
        assert_eq!(
            policy.processing_compositions[VIDEO_COMPONENT_ID]
                .processor
                .as_ref()
                .unwrap()
                .service,
            LEGACY_PROCESSOR_SERVICE
        );
    }

    #[test]
    fn a_current_policy_is_not_reported_as_converted() {
        let fixture = Fixture::new();
        let mut policy = fixture.current.clone();
        let converted = compose_rtx_hdr_destination(
            &mut policy,
            &fixture.runtime_directory,
            &default_rtx_hdr_selection(),
        )
        .unwrap();
        assert!(!converted);
    }

    #[test]
    fn a_different_selected_processor_is_refused_without_changes() {
        let fixture = Fixture::new();
        let mut policy = fixture.legacy();
        policy.settings.get_mut(VIDEO_COMPONENT_ID).unwrap()["gpu_processor_service"] =
            json!("example.other/1");
        let before = serde_json::to_value(&policy).unwrap();
        assert!(
            compose_rtx_hdr_destination(
                &mut policy,
                &fixture.runtime_directory,
                &default_rtx_hdr_selection(),
            )
            .is_err()
        );
        assert_eq!(serde_json::to_value(&policy).unwrap(), before);
    }

    #[test]
    fn default_selection_binds_the_foreground_provider_only_when_approved() {
        let fixture = Fixture::new();
        let mut policy = fixture.legacy();
        policy.principals.remove(FOREGROUND_CONTEXT.id);
        assert!(default_rtx_hdr_selection_for(&policy).foreground.is_none());
        policy
            .principals
            .insert(FOREGROUND_CONTEXT.id.into(), FOREGROUND_CONTEXT.grants());
        let selection = default_rtx_hdr_selection_for(&policy);
        assert_eq!(
            selection.foreground.as_ref().unwrap().principal,
            FOREGROUND_CONTEXT.id
        );
        compose_rtx_hdr_destination(&mut policy, &fixture.runtime_directory, &selection).unwrap();
        let view = &policy.processing_compositions[RTX_HDR_COMPONENT.id];
        assert_eq!(
            view.foreground.as_ref().unwrap().service,
            amaterasu_module_sdk::foreground_context::SERVICE
        );
    }

    #[test]
    fn default_selection_omits_consumers_the_policy_does_not_approve() {
        let fixture = Fixture::new();
        let mut policy = fixture.current.clone();
        policy.principals.remove("amaterasu.remote-session");
        assert!(
            default_rtx_hdr_selection_for(&policy)
                .source_preflight_consumers
                .is_empty()
        );
    }
}
