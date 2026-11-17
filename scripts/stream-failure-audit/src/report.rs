use crate::{
    Inventory, Observation, digest,
    graph::{self, CallPath, Graph},
};
use serde::Serialize;
use std::collections::BTreeMap;

pub const LIMITATIONS: &[&str] = &[
    "This is a conservative source inventory, not all possible stream scenarios or a proof of execution, reachability, criticality, or policy compliance.",
    "Call edges are name/annotation candidates, not compiler type resolution. Imports/aliases, trait objects, function pointers, generics, callbacks, workers, events and IPC can break or misconnect paths. Unresolved calls are retained.",
    "Call paths do not prove data flow: a callee error may be caught, retried or ignored. Try propagation, conversions, fallback closures and guards are evidence for review; no automatic demotion is performed.",
    "Only local Rust under crates/ and modules/ is scanned. Build scripts, tests, examples, fixtures, vendored third-party code and symlink targets are excluded. Native C/C++, GPU/shader, OS, firmware, remote/replacement plugins and installed binaries require separate audits.",
    "Unexpanded macros are opaque except explicit panic macros and json! ok:false token pairs. Generated Rust, include!, proc macros, extern/ABI and dynamic module routing may hide failures. API metadata is a contract inventory, not proof of the selected provider or an IPC call edge.",
    "All non-test feature/platform branches are inventoried together without evaluating cfg. cfg(test) and #[test] declarations are excluded; runtime tests or arbitrary custom cfg tests cannot always be identified.",
    "Termination names and lifecycle state assignments can describe normal teardown, local resources, or unrelated sessions. Negative wire replies may refuse one request while preserving the stream. Panic candidates may be proven safe by invariants outside this analysis.",
    "Boundary detection uses a finite vocabulary of call/state names and response forms. Custom wrappers, status constants, non-JSON wire encodings, silent thread exit, deadlocks and errors represented by other conventions can be missed. Inspect unlinked origins and unresolved calls; expand fixtures when adding new conventions.",
    "A policy reference in the same function does not prove domination, final decision authority or enforcement. Missing references are structural review candidates, never a recommendation to bypass authentication, encryption, ownership or retirement fences.",
    "Source files are read individually, not from an atomic filesystem snapshot. File SHA-256 digests, Git identity and dirty state identify the examined input; concurrent changes require a rerun.",
];

#[derive(Serialize)]
pub struct Finding {
    pub id: String,
    pub function: usize,
    pub observation: Observation,
    pub classification: String,
    pub classification_reason: String,
    pub relevance: String,
    pub call_paths: Vec<CallPath>,
    pub paths_truncated: bool,
    pub central_decision_review: String,
}

#[derive(Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub source: serde_json::Value,
    pub limitations: Vec<String>,
    pub decision_symbols: Vec<String>,
    pub unmatched_decision_symbols: Vec<String>,
    pub inventory: Inventory,
    pub graph: Graph,
    pub findings: Vec<Finding>,
    pub summary: BTreeMap<String, usize>,
}

pub fn build(
    inventory: Inventory,
    source: serde_json::Value,
    symbols: Vec<String>,
    depth: usize,
    limit: usize,
) -> Report {
    let graph = graph::build(&inventory.functions);
    let mut configured = Vec::new();
    let mut unmatched = Vec::new();
    for symbol in &symbols {
        let matches: Vec<_> = inventory
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let name = format!("{}::{}::{}", f.package, f.owner, f.name);
                (f.id == *symbol
                    || name == *symbol
                    || format!("{}::{}", f.owner, f.name) == *symbol)
                    .then_some(i)
            })
            .collect();
        if matches.is_empty() {
            unmatched.push(symbol.clone());
        }
        configured.extend(matches);
    }
    let mut findings = Vec::new();
    let mut summary = BTreeMap::new();
    for (index, function) in inventory.functions.iter().enumerate() {
        if function.observations.is_empty() {
            continue;
        }
        let (call_paths, paths_truncated) = graph::paths(&graph, index, depth, limit);
        let relevance = if graph::stream_scope(function) {
            "stream_scope"
        } else if call_paths.iter().any(|p| {
            p.functions
                .iter()
                .any(|&i| graph::stream_scope(&inventory.functions[i]))
        }) {
            "candidate_stream_caller"
        } else {
            "unlinked_review"
        };
        for observation in &function.observations {
            let (classification, reason) = classify(observation);
            let policy = if symbols.is_empty() {
                "unconfigured"
            } else if configured.contains(&index) {
                "inside_configured_symbol"
            } else if graph
                .edges
                .iter()
                .any(|edge| edge.caller == index && configured.contains(&edge.callee))
            {
                "same_function_reference_only_not_enforcement"
            } else {
                "no_resolved_policy_reference_review_required"
            };
            let mut id_input = format!(
                "{}:{}:{}:{}",
                observation.location.file,
                observation.location.line,
                observation.location.column,
                observation.kind
            );
            id_input.push_str(&observation.expression);
            findings.push(Finding {
                id: digest(id_input.as_bytes())[..16].into(),
                function: index,
                observation: observation.clone(),
                classification: classification.into(),
                classification_reason: reason.into(),
                relevance: relevance.into(),
                call_paths: call_paths.clone(),
                paths_truncated,
                central_decision_review: policy.into(),
            });
            *summary
                .entry(format!("kind:{}", observation.kind))
                .or_default() += 1;
            *summary
                .entry(format!("classification:{classification}"))
                .or_default() += 1;
            *summary.entry(format!("relevance:{relevance}")).or_default() += 1;
        }
    }
    summary.insert("source_files".into(), inventory.files.len());
    summary.insert("functions".into(), inventory.functions.len());
    summary.insert("findings".into(), findings.len());
    summary.insert("parse_or_io_errors".into(), inventory.errors.len());
    summary.insert("coverage_gaps".into(), inventory.gaps.len());
    summary.insert("call_edges_candidate".into(), graph.edges.len());
    summary.insert("calls_unresolved".into(), graph.unresolved.len());
    summary.insert(
        "functions_with_bounded_paths".into(),
        findings
            .iter()
            .filter(|f| f.paths_truncated)
            .map(|f| f.function)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    );
    Report {
        schema_version: 1,
        source,
        limitations: LIMITATIONS.iter().map(|s| (*s).into()).collect(),
        decision_symbols: symbols,
        unmatched_decision_symbols: unmatched,
        inventory,
        graph,
        findings,
        summary,
    }
}

fn classify(observation: &Observation) -> (&'static str, &'static str) {
    if observation.kind == "declared_disposition" {
        if observation.expression.contains(":: critical") {
            return (
                "critical_proposed",
                "Source declares StreamIssue::critical; validate this classification against the actual effect and contract.",
            );
        }
        return (
            "superficial_proposed",
            "Source declares an advisory or demotion; verify fallback, security contract and lifecycle reporting before accepting it.",
        );
    }
    if observation.kind == "termination_candidate"
        && observation.expression.ends_with("close (None)")
    {
        return (
            "normal_teardown_proposed",
            "close(None) supplies no failure reason; verify the initiating request and retirement outcome.",
        );
    }
    if observation.literals.iter().any(|s| {
        [
            "permission_denied",
            "authentication_failed",
            "unauthorized",
            "encryption_required",
            "ownership_unconfirmed",
            "invalid_key",
        ]
        .contains(&s.as_str())
    }) {
        return (
            "critical_proposed",
            "A literal names a security/ownership condition. This is a review hint, not an automatic policy decision.",
        );
    }
    (
        "unresolved",
        "Determine whether this affects a request, startup, an active stream, cleanup, or only a local resource; follow guards, propagation and provider contracts.",
    )
}

pub fn markdown(report: &Report, max_findings: usize) -> String {
    use std::fmt::Write;
    let mut out = String::from(
        "# Stream failure source audit\n\nAll classifications are proposed or unresolved. This report never authorizes demotion or changes runtime policy.\n\n",
    );
    writeln!(out, "Source: `{}`\n", report.source).unwrap();
    out.push_str("## Coverage\n\n");
    for (key, value) in &report.summary {
        writeln!(out, "- {key}: {value}").unwrap();
    }
    for gap in &report.inventory.gaps {
        writeln!(out, "- GAP: {gap}").unwrap();
    }
    for error in &report.inventory.errors {
        writeln!(out, "- ERROR: {error}").unwrap();
    }
    for symbol in &report.unmatched_decision_symbols {
        writeln!(out, "- UNMATCHED DECISION SYMBOL: {symbol}").unwrap();
    }
    out.push_str("\n## Limits\n\n");
    for limit in &report.limitations {
        writeln!(out, "- {limit}").unwrap();
    }
    out.push_str("\n## Review queue\n\nPrioritized stream-scope boundary sites come first. The JSON contains the full inventory, including unlinked origins, module boundaries, unresolved dispatch and source digests.\n\n");
    let mut queue: Vec<_> = report.findings.iter().collect();
    queue.sort_by_key(|f| {
        (
            f.relevance == "unlinked_review",
            !is_boundary(&f.observation.kind),
            &f.observation.location.file,
            f.observation.location.line,
            &f.id,
        )
    });
    for finding in queue.iter().take(max_findings) {
        let o = &finding.observation;
        let function = &report.inventory.functions[finding.function];
        writeln!(
            out,
            "### {} — {}\n\n`{}:{}:{}` · `{}` · **{}** · {}\n\n{}\n\nCentral decision: `{}`.\n",
            finding.id,
            o.kind,
            o.location.file,
            o.location.line,
            o.location.column,
            function.id,
            finding.classification,
            finding.relevance,
            finding.classification_reason,
            finding.central_decision_review
        )
        .unwrap();
        for guard in &o.guards {
            writeln!(out, "- Guard: `{}`", compact(guard, 800)).unwrap();
        }
        if !o.literals.is_empty() {
            writeln!(
                out,
                "\nLiteral reason/context candidates: `{}`\n",
                compact(&o.literals.join("`, `"), 1200)
            )
            .unwrap();
        }
        writeln!(out, "```rust\n{}\n```\n", o.evidence).unwrap();
        for path in &finding.call_paths {
            let names = path
                .functions
                .iter()
                .map(|&i| report.inventory.functions[i].id.as_str())
                .collect::<Vec<_>>()
                .join(" → ");
            writeln!(
                out,
                "- Candidate call path: `{names}`{} ",
                if path.bounded { " (depth bounded)" } else { "" }
            )
            .unwrap();
        }
        if finding.paths_truncated {
            out.push_str("- Additional callers/depth omitted by configured bounds; inspect JSON call edges.\n");
        }
        out.push('\n');
    }
    writeln!(
        out,
        "Shown {} of {} observations; remaining observations are in JSON.\n",
        max_findings.min(queue.len()),
        queue.len()
    )
    .unwrap();
    out
}

pub fn is_boundary(kind: &str) -> bool {
    [
        "failure_boundary",
        "termination_candidate",
        "wire_status_candidate",
        "negative_response",
        "negative_wire_structure",
        "lifecycle_transition",
        "failure_state_return_candidate",
        "error_reply_candidate",
    ]
    .contains(&kind)
}

fn compact(text: &str, limit: usize) -> String {
    if text.chars().count() > limit {
        format!(
            "{}… [truncated; full expression in JSON]",
            text.chars().take(limit).collect::<String>()
        )
    } else {
        text.into()
    }
}
