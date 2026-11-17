use amaterasu_stream_failure_audit::{Inventory, graph, report, scan};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    process::Command,
};

fn fixture() -> Inventory {
    Inventory {
        functions: scan::parse_source(
            "modules/rtsp/src/fixture.rs",
            "amaterasu_rtsp",
            include_str!("fixtures/scenarios.rs"),
        )
        .unwrap(),
        ..Default::default()
    }
}

#[test]
fn labeled_recall_and_false_positive_controls() {
    let inventory = fixture();
    // Gold labels record constructs that must be available to a human reviewer.
    // They do not claim that every observed construct actually kills a stream.
    let positives = [
        ("explicit_security_error", "error_origin"),
        ("propagated_startup", "error_propagation"),
        ("converted_error", "error_conversion"),
        ("absent_required_provider", "error_conversion"),
        ("wire_auth", "wire_status_candidate"),
        ("wire_expired", "wire_status_candidate"),
        ("wire_sequence", "wire_status_candidate"),
        ("wire_busy", "wire_status_candidate"),
        ("wire_dynamic", "wire_status_candidate"),
        ("wire_with_failure_report", "wire_status_candidate"),
        ("timeout", "failure_boundary"),
        ("failed_state", "lifecycle_transition"),
        ("stopping_state", "lifecycle_transition"),
        ("negative_json", "negative_response"),
        ("nested_negative_json", "negative_response"),
        ("broker_dispatch", "module_boundary"),
        ("ipc_literal", "module_boundary"),
        ("ipc_dynamic", "module_boundary"),
        ("uncertain_cleanup", "termination_candidate"),
        ("normal_teardown", "termination_candidate"),
        ("revoke", "termination_candidate"),
        ("transport_disconnected", "termination_candidate"),
        ("process_abort", "termination_candidate"),
        ("panic_on_startup", "panic_candidate"),
        ("unwrap_failure", "panic_candidate"),
        ("expect_failure", "panic_candidate"),
        ("declared_critical", "declared_disposition"),
        ("declared_demoted", "declared_disposition"),
        ("declared_advisory", "declared_disposition"),
        ("sentinel_none", "sentinel_return_candidate"),
        ("sentinel_false", "sentinel_return_candidate"),
        ("tail_none", "sentinel_return_candidate"),
        ("typed_error_reply", "error_reply_candidate"),
        ("pointer_dispatch", "indirect_dispatch"),
        ("guarded_failure", "failure_boundary"),
        ("platform_failure", "error_origin"),
        ("mixed_cfg_production", "error_origin"),
        ("let_else_provider", "failure_boundary"),
        ("private_wire_error", "error_reply_candidate"),
        ("reply_struct", "negative_wire_structure"),
        ("conditional_failed_state", "lifecycle_transition"),
        ("closing_state", "lifecycle_transition"),
        ("failed_state_return", "failure_state_return_candidate"),
        ("opaque_macro", "opaque_macro_boundary"),
    ];
    let negatives = [
        "comment_only",
        "string_only",
        "optional_fallback",
        "optional_caught",
        "healthy_wire",
        "healthy_wire_with_report",
        "healthy_json",
        "healthy_json_string",
        "format_message",
        "log_only",
        "warning_only",
        "identifier_only",
        "error_text_variable",
        "harmless_name",
        "nested_declaration",
    ];
    let by_name: BTreeMap<_, _> = inventory
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let missed: Vec<_> = positives
        .iter()
        .filter(|(name, kind)| {
            !by_name
                .get(name)
                .is_some_and(|f| f.observations.iter().any(|o| o.kind == *kind))
        })
        .collect();
    let false_positive: Vec<_> = negatives
        .iter()
        .filter(|name| {
            by_name
                .get(**name)
                .is_some_and(|f| !f.observations.is_empty())
        })
        .collect();
    eprintln!(
        "Gold fixture: TP={}/{}, FN={}, FP={}/{}, recall={:.3}, negative false-positive rate={:.3}",
        positives.len() - missed.len(),
        positives.len(),
        missed.len(),
        false_positive.len(),
        negatives.len(),
        (positives.len() - missed.len()) as f64 / positives.len() as f64,
        false_positive.len() as f64 / negatives.len() as f64
    );
    assert!(missed.is_empty(), "missed: {missed:?}");
    assert!(
        false_positive.is_empty(),
        "false positives: {false_positive:?}"
    );
    assert!(!by_name.contains_key("test_function"));
    assert!(!by_name.contains_key("fake_failure"));
    let wire = &by_name["wire_with_failure_report"].observations[0];
    assert_eq!(wire.literal_status, Some(455));
    assert_eq!(wire.arguments.len(), 4);
    assert_eq!(wire.arguments[2], "455u16");
}

#[test]
fn indirect_paths_guards_and_receiver_disambiguation() {
    let inventory = fixture();
    let graph = graph::build(&inventory.functions);
    let index = |name: &str, owner: &str| {
        inventory
            .functions
            .iter()
            .position(|f| f.name == name && f.owner == owner)
            .unwrap()
    };
    let (_, truncated) = graph::paths(&graph, index("fail", "Worker"), 5, 3);
    assert!(!truncated);
    let (paths, _) = graph::paths(&graph, index("fail", "Worker"), 5, 3);
    assert!(paths.iter().any(|p| {
        p.functions
            .iter()
            .map(|&i| inventory.functions[i].name.as_str())
            .collect::<Vec<_>>()
            == ["start", "prepare", "fail"]
    }));
    for (caller, owner) in [("file_close", "File"), ("stream_close", "Stream")] {
        assert!(
            graph
                .edges
                .iter()
                .any(|e| e.caller == index(caller, "") && e.callee == index("close", owner))
        );
    }
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.caller == index("unknown_close", ""))
    );
    assert!(
        graph
            .unresolved
            .iter()
            .any(|c| c.caller == index("unknown_close", ""))
    );
    let guard = &inventory.functions[index("guarded_failure", "")].observations[0].guards;
    assert!(guard.iter().any(|g| g.contains("! ready")));
    assert!(guard.iter().any(|g| g.contains("Disconnected")));
    assert!(
        inventory.functions[index("let_else_provider", "")]
            .observations
            .iter()
            .any(|o| o.guards.iter().any(|g| g.starts_with("let-else")))
    );
}

#[test]
fn classifications_and_policy_references_never_claim_proofs() {
    let result = report::build(
        fixture(),
        serde_json::json!({}),
        vec!["Worker::fail".into()],
        5,
        3,
    );
    let classification = |name: &str| {
        result
            .findings
            .iter()
            .find(|f| result.inventory.functions[f.function].name == name)
            .unwrap()
    };
    assert_eq!(
        classification("declared_demoted").classification,
        "superficial_proposed"
    );
    assert_eq!(
        classification("explicit_security_error").classification,
        "critical_proposed"
    );
    assert_eq!(
        classification("normal_teardown").classification,
        "normal_teardown_proposed"
    );
    assert_eq!(classification("caught_origin").classification, "unresolved");
    assert_eq!(
        classification("prepare").central_decision_review,
        "same_function_reference_only_not_enforcement"
    );
    assert_eq!(
        classification("wire_auth").central_decision_review,
        "no_resolved_policy_reference_review_required"
    );
    assert!(result.unmatched_decision_symbols.is_empty());
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert!(report::markdown(&result, 2).contains("remaining observations are in JSON"));
    let ids: BTreeSet<_> = result.findings.iter().map(|f| &f.id).collect();
    assert_eq!(ids.len(), result.findings.len());
}

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "stream-failure-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(path.join("crates/core/src")).unwrap();
        fs::create_dir_all(path.join("modules/rtsp/src")).unwrap();
        Self(path)
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn cli_reports_parse_errors_missing_modules_and_deterministic_output() {
    let root = TempRoot::new();
    fs::write(
        root.0.join("modules/rtsp/Cargo.toml"),
        "[package]\nname = \"amaterasu-rtsp\"\n",
    )
    .unwrap();
    fs::write(
        root.0.join("modules/rtsp/src/lib.rs"),
        "fn start() -> Result<(), E> { Err(\"native_failed\".into()) }",
    )
    .unwrap();
    fs::write(
        root.0.join(".gitmodules"),
        "[submodule \"missing\"]\npath = modules/encoder\n",
    )
    .unwrap();
    fs::write(
        root.0.join("modules/rtsp/src/tests.rs"),
        "this is intentionally not Rust",
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_amaterasu-stream-failure-audit"))
            .arg("--root")
            .arg(&root.0)
            .arg("--json")
            .arg(root.0.join("out.json"))
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let first = fs::read(root.0.join("out.json")).unwrap();
    assert!(run().status.success());
    assert_eq!(first, fs::read(root.0.join("out.json")).unwrap());
    let report: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        report["inventory"]["gaps"][0],
        "unavailable module source: modules/encoder"
    );
    fs::write(root.0.join("modules/rtsp/src/broken.rs"), "fn bad(").unwrap();
    assert_eq!(run().status.code(), Some(2));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(root.0.join("out.json")).unwrap()).unwrap();
    assert_eq!(report["inventory"]["errors"].as_array().unwrap().len(), 1);
}

#[test]
fn unknown_policy_and_invalid_options_fail_visibly() {
    let root = TempRoot::new();
    for args in [
        vec!["--decision-symbol", "Missing::decide"],
        vec!["--max-depth", "0"],
        vec!["--max-paths", "0"],
        vec!["--bogus", "x"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_amaterasu-stream-failure-audit"))
            .arg("--root")
            .arg(&root.0)
            .arg("--json")
            .arg(root.0.join("out.json"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn imported_central_policy_is_a_candidate_reference_across_crates() {
    let mut inventory = Inventory::default();
    inventory.functions.extend(
        scan::parse_source(
            "crates/module-sdk/src/termination.rs",
            "amaterasu_module_sdk",
            "struct Handler; impl Handler { fn decide() {} }",
        )
        .unwrap(),
    );
    inventory.functions.extend(scan::parse_source("modules/rtsp/src/network.rs", "amaterasu_rtsp", "use amaterasu_module_sdk::stream_lifecycle::{Handler as Policy}; fn reject_request(c: &mut Controller) { Policy::decide(); c.reject(); }").unwrap());
    let result = report::build(
        inventory,
        serde_json::json!({}),
        vec!["amaterasu_module_sdk::Handler::decide".into()],
        5,
        3,
    );
    let boundary = result
        .findings
        .iter()
        .find(|f| f.observation.kind == "failure_boundary")
        .unwrap();
    assert_eq!(
        boundary.central_decision_review,
        "same_function_reference_only_not_enforcement"
    );
}

#[test]
fn outputs_cannot_overwrite_source_inputs() {
    let root = TempRoot::new();
    let source = root.0.join("modules/rtsp/src/lib.rs");
    let original = "fn healthy() {}";
    fs::write(&source, original).unwrap();
    for target in [
        source.clone(),
        root.0.join("modules/rtsp/api.json"),
        root.0.join("Cargo.toml"),
    ] {
        let status = Command::new(env!("CARGO_BIN_EXE_amaterasu-stream-failure-audit"))
            .arg("--root")
            .arg(&root.0)
            .arg("--json")
            .arg(target)
            .output()
            .unwrap();
        assert_eq!(status.status.code(), Some(2));
    }
    assert_eq!(fs::read_to_string(source).unwrap(), original);
}

#[test]
fn catches_do_not_automatically_demote_and_path_limits_are_visible() {
    let result = report::build(fixture(), serde_json::json!({}), vec![], 1, 1);
    let origin = result
        .findings
        .iter()
        .find(|f| result.inventory.functions[f.function].name == "caught_origin")
        .unwrap();
    assert_eq!(origin.classification, "unresolved");
    let worker = result
        .findings
        .iter()
        .find(|f| result.inventory.functions[f.function].name == "fail")
        .unwrap();
    assert!(worker.paths_truncated);
    assert!(worker.call_paths.iter().any(|p| p.bounded));
    let file = result
        .findings
        .iter()
        .find(|f| result.inventory.functions[f.function].name == "file_close")
        .unwrap();
    assert_eq!(file.classification, "unresolved");
}
