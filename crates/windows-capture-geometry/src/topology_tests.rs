use crate::{
    model::{CcdSnapshot, DisplaySnapshot, Topology},
    GeometryError, SelectedOutput,
};
use amaterasu_media::{CaptureGeometry, DesktopRect};

#[derive(Clone)]
struct Snapshots {
    gdi: Vec<DisplaySnapshot>,
    dxgi: Vec<DisplaySnapshot>,
    ccd: Vec<CcdSnapshot>,
}

fn rectangle(left: i32, top: i32, width: u32, height: u32) -> DesktopRect {
    DesktopRect::new(left, top, width, height).unwrap()
}

fn snapshots() -> Snapshots {
    let displays = [
        (
            r"\\.\DISPLAY1",
            10,
            rectangle(-1920, 0, 1920, 1080),
            "adapter-a:target-7",
        ),
        (
            r"\\.\DISPLAY2",
            20,
            rectangle(0, -200, 2560, 1440),
            "adapter-b:target-3",
        ),
        (
            r"\\.\DISPLAY3",
            30,
            rectangle(2560, 100, 1280, 1024),
            "adapter-b:target-8",
        ),
    ];
    let native = displays
        .iter()
        .map(
            |(source_name, monitor_handle, rectangle, _)| DisplaySnapshot {
                source_name: (*source_name).into(),
                monitor_handle: *monitor_handle,
                rectangle: *rectangle,
            },
        )
        .collect::<Vec<_>>();
    let ccd = displays
        .iter()
        .map(|(source_name, _, rectangle, target_id)| CcdSnapshot {
            source_name: (*source_name).into(),
            target_id: (*target_id).into(),
            rectangle: *rectangle,
        })
        .collect();
    Snapshots {
        gdi: native.clone(),
        dxgi: native,
        ccd,
    }
}

fn selected(snapshot: &Snapshots, index: usize) -> SelectedOutput {
    let display = &snapshot.gdi[index];
    SelectedOutput {
        source_name: display.source_name.clone(),
        monitor_handle: display.monitor_handle,
        rectangle: display.rectangle,
    }
}

fn assert_reconcile_unavailable(snapshot: Snapshots) {
    assert!(matches!(
        Topology::reconcile(snapshot.gdi, snapshot.dxgi, snapshot.ccd),
        Err(GeometryError::Unavailable(_))
    ));
}

fn assert_resolve_unavailable(topology: &Topology, selected: &SelectedOutput) {
    assert!(matches!(
        topology.resolve(selected, None),
        Err(GeometryError::Unavailable(_))
    ));
}

#[test]
fn equal_complete_sets_reconcile_independent_order_case_and_negative_origins() {
    let mut snapshot = snapshots();
    snapshot.dxgi.reverse();
    for display in &mut snapshot.dxgi {
        display.source_name.make_ascii_lowercase();
    }
    snapshot.ccd.rotate_left(1);
    for display in &mut snapshot.ccd {
        display.source_name.make_ascii_lowercase();
    }
    let selected = SelectedOutput {
        source_name: r"\\.\display1".into(),
        ..selected(&snapshot, 0)
    };

    let topology = Topology::reconcile(snapshot.gdi, snapshot.dxgi, snapshot.ccd).unwrap();
    let result = topology
        .resolve(&selected, Some("ADAPTER-A:TARGET-7"))
        .unwrap();
    assert_eq!(result.target_id, "adapter-a:target-7");
    assert_eq!(
        result.geometry,
        CaptureGeometry::new(
            rectangle(-1920, 0, 1920, 1080),
            rectangle(-1920, -200, 5760, 1440),
        )
        .unwrap()
    );
}

#[test]
fn gdi_and_dxgi_must_match_monitor_handle_source_name_and_rectangle() {
    let mut wrong_handle = snapshots();
    wrong_handle.dxgi[0].monitor_handle = 99;
    assert_reconcile_unavailable(wrong_handle);

    let mut wrong_name = snapshots();
    wrong_name.dxgi[0].source_name = r"\\.\DISPLAY9".into();
    assert_reconcile_unavailable(wrong_name);

    let mut wrong_rectangle = snapshots();
    wrong_rectangle.dxgi[0].rectangle = rectangle(-1919, 0, 1920, 1080);
    assert_reconcile_unavailable(wrong_rectangle);
}

#[test]
fn ccd_must_match_each_capture_source_and_rectangle() {
    let mut wrong_source = snapshots();
    wrong_source.ccd[0].source_name = r"\\.\DISPLAY9".into();
    assert_reconcile_unavailable(wrong_source);

    let mut wrong_rectangle = snapshots();
    wrong_rectangle.ccd[0].rectangle = rectangle(-1920, 1, 1920, 1080);
    assert_reconcile_unavailable(wrong_rectangle);
}

#[test]
fn missing_extra_and_equal_length_wrong_members_never_form_a_topology() {
    let mut missing_gdi = snapshots();
    missing_gdi.gdi.pop();
    assert_reconcile_unavailable(missing_gdi);

    let mut missing_dxgi = snapshots();
    missing_dxgi.dxgi.pop();
    assert_reconcile_unavailable(missing_dxgi);

    let mut missing_ccd = snapshots();
    missing_ccd.ccd.pop();
    assert_reconcile_unavailable(missing_ccd);

    let mut extra_dxgi = snapshots();
    extra_dxgi.dxgi.push(DisplaySnapshot {
        source_name: r"\\.\DISPLAY4".into(),
        monitor_handle: 40,
        rectangle: rectangle(3840, 0, 1024, 768),
    });
    assert_reconcile_unavailable(extra_dxgi);

    let mut wrong_member = snapshots();
    wrong_member.dxgi[2] = DisplaySnapshot {
        source_name: r"\\.\DISPLAY4".into(),
        monitor_handle: 40,
        rectangle: rectangle(3840, 0, 1024, 768),
    };
    assert_reconcile_unavailable(wrong_member);
}

#[test]
fn duplicate_enumerator_identities_and_ambiguous_clones_are_rejected() {
    let mut duplicate_gdi_handle = snapshots();
    duplicate_gdi_handle.gdi[1].monitor_handle = duplicate_gdi_handle.gdi[0].monitor_handle;
    assert_reconcile_unavailable(duplicate_gdi_handle);

    let mut duplicate_gdi_source = snapshots();
    duplicate_gdi_source.gdi[1].source_name = duplicate_gdi_source.gdi[0].source_name.clone();
    assert_reconcile_unavailable(duplicate_gdi_source);

    let mut duplicate_dxgi_handle = snapshots();
    duplicate_dxgi_handle.dxgi[1].monitor_handle = duplicate_dxgi_handle.dxgi[0].monitor_handle;
    assert_reconcile_unavailable(duplicate_dxgi_handle);

    // Two CCD paths naming the same source is an ambiguous cloned topology.
    let mut cloned_source = snapshots();
    cloned_source.ccd[1].source_name = cloned_source.ccd[0].source_name.to_ascii_lowercase();
    assert_reconcile_unavailable(cloned_source);

    let mut missing_target = snapshots();
    missing_target.ccd[0].target_id.clear();
    assert_reconcile_unavailable(missing_target);

    let mut duplicate_target = snapshots();
    duplicate_target.ccd[1].target_id = duplicate_target.ccd[0].target_id.to_ascii_uppercase();
    assert_reconcile_unavailable(duplicate_target);
}

#[test]
fn retained_physical_target_detects_identity_change_and_display_name_reuse() {
    let original = snapshots();
    let selected = selected(&original, 0);
    let old_target = original.ccd[0].target_id.clone();
    let topology = Topology::reconcile(original.gdi, original.dxgi, original.ccd).unwrap();
    assert!(topology.resolve(&selected, Some(&old_target)).is_ok());

    let mut replacement = snapshots();
    replacement.ccd[0].target_id = "adapter-c:replacement".into();
    let topology = Topology::reconcile(replacement.gdi, replacement.dxgi, replacement.ccd).unwrap();
    assert_eq!(
        topology.resolve(&selected, Some(&old_target)),
        Err(GeometryError::TargetChanged)
    );

    let mut reused_display_name = snapshots();
    reused_display_name.ccd.swap(0, 1);
    reused_display_name.ccd[0].source_name = r"\\.\DISPLAY1".into();
    reused_display_name.ccd[1].source_name = r"\\.\DISPLAY2".into();
    reused_display_name.ccd[0].rectangle = reused_display_name.gdi[0].rectangle;
    reused_display_name.ccd[1].rectangle = reused_display_name.gdi[1].rectangle;
    let topology = Topology::reconcile(
        reused_display_name.gdi,
        reused_display_name.dxgi,
        reused_display_name.ccd,
    )
    .unwrap();
    assert_eq!(
        topology.resolve(&selected, Some(&old_target)),
        Err(GeometryError::TargetChanged)
    );
}

#[test]
fn selected_monitor_must_still_match_handle_name_and_rectangle() {
    let snapshot = snapshots();
    let selected = selected(&snapshot, 0);
    let topology = Topology::reconcile(snapshot.gdi, snapshot.dxgi, snapshot.ccd).unwrap();

    assert_resolve_unavailable(
        &topology,
        &SelectedOutput {
            monitor_handle: 99,
            ..selected.clone()
        },
    );
    assert_resolve_unavailable(
        &topology,
        &SelectedOutput {
            source_name: r"\\.\DISPLAY9".into(),
            ..selected.clone()
        },
    );
    assert_resolve_unavailable(
        &topology,
        &SelectedOutput {
            rectangle: rectangle(-1919, 0, 1920, 1080),
            ..selected.clone()
        },
    );
    assert_resolve_unavailable(
        &topology,
        &SelectedOutput {
            rectangle: DesktopRect {
                left: -1920,
                top: 0,
                width: 0,
                height: 1080,
            },
            ..selected
        },
    );
}
