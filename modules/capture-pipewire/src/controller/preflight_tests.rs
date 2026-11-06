use super::*;
use crate::test_support::{settings, setup};
use amaterasu_module_sdk::capture::{SourceClass, SourceMode, SourcePreflightRequest};
use std::sync::atomic::Ordering;

fn request(class: SourceClass, route: Option<&str>) -> SourcePreflightRequest {
    SourcePreflightRequest {
        source_class: class,
        route: route.map(str::to_owned),
        mode: SourceMode {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: false,
            bit_depth: Some(8),
        },
    }
}

#[test]
fn managed_approval_does_not_authorize_desktop_probe_or_open() {
    let mut config = settings();
    config.allow_named_outputs = true;
    let (mut controller, driver) = setup(config);
    assert!(
        controller
            .preflight(
                "amaterasu.video",
                &request(SourceClass::ManagedOutput, None)
            )
            .is_ready()
    );
    assert!(
        !controller
            .preflight(
                "amaterasu.video",
                &request(SourceClass::DesktopOutput, Some("kwin:DP-1"))
            )
            .is_ready()
    );
    assert!(matches!(
        controller.handle(
            "amaterasu.video",
            Operation::Open {
                output: "kwin:DP-1".into(),
                operation_id: Some("desktop".into()),
                constant_rate: None
            },
            0
        ),
        Err("desktop_capture_disabled")
    ));
    assert_eq!(driver.preflights.load(Ordering::SeqCst), 1);
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
    assert!(controller.selection.is_none());
    assert!(controller.lease.is_none());
}

#[test]
fn approved_desktop_preflight_is_pure_and_checks_authority_format_and_route() {
    let mut config = settings();
    config.allow_desktop_outputs = true;
    let (controller, driver) = setup(config);
    let mut query = request(SourceClass::DesktopOutput, Some("kwin:DP-1"));
    let response = controller.preflight("amaterasu.video", &query);
    assert!(response.is_ready());
    response.validate(&query).unwrap();
    assert!(!controller.preflight("other", &query).is_ready());
    query.mode.hdr = true;
    query.mode.bit_depth = Some(10);
    assert!(!controller.preflight("amaterasu.video", &query).is_ready());
    query.mode.hdr = false;
    query.mode.bit_depth = Some(8);
    query.route = Some("portal-selected-session-source".into());
    assert!(!controller.preflight("amaterasu.video", &query).is_ready());
    query.source_class = SourceClass::ManagedOutput;
    query.route = Some("kwin:DP-1".into());
    assert!(!controller.preflight("amaterasu.video", &query).is_ready());
    assert_eq!(driver.preflights.load(Ordering::SeqCst), 1);
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
    assert_eq!(driver.state_reads.load(Ordering::SeqCst), 0);
    assert!(controller.selection.is_none());
    assert!(controller.lease.is_none());
    assert_eq!(controller.opened, 0);
}

#[test]
fn separately_approved_desktop_open_uses_exact_name_without_portal_selection() {
    let mut config = settings();
    config.allow_desktop_outputs = true;
    let (mut controller, driver) = setup(config);
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 2,
        width: 2,
        height: 2,
    };
    let result = controller
        .handle(
            "amaterasu.video",
            Operation::Open {
                output: "kwin:DP-1".into(),
                operation_id: Some("desktop".into()),
                constant_rate: None,
            },
            0,
        )
        .unwrap();
    assert_eq!(result.payload["output"], "kwin:DP-1");
    assert_eq!(driver.selections.load(Ordering::SeqCst), 1);
    assert!(controller.selection.is_none());
    assert!(controller.lease.is_none());
}
