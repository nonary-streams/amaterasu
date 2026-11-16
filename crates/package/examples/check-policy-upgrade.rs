//! Read-only check of a locally prepared policy against the host's real guard.
//! Package signatures and activation remain the installed host's responsibility.
use amaterasu_package::policy_approval::{
    PolicyAllowlist, PolicyCandidateContext, candidate_from_allowlist,
};
use serde_json::Value;
use std::{fs, path::PathBuf};

fn check() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err("usage: check-policy-upgrade ROOT PREPARED_CANDIDATE".into());
    }
    let root = PathBuf::from(&arguments[0]);
    let candidate = PathBuf::from(&arguments[1]);
    let read = |path| fs::read(path).map_err(|error| error.to_string());
    let metadata: Value = serde_json::from_slice(&read(candidate.join("metadata.json"))?)
        .map_err(|error| error.to_string())?;
    let allowlist: PolicyAllowlist =
        serde_json::from_slice(&read(candidate.join("package-activation-policy.json"))?)
            .map_err(|error| error.to_string())?;
    let context = PolicyCandidateContext {
        selection_revision: metadata["selection_revision"]
            .as_str()
            .ok_or("selection revision")?,
        publisher_fingerprint: &allowlist.publisher_fingerprint,
        manifest_sha256: metadata["digest"].as_str().ok_or("manifest digest")?,
    };
    let current = read(root.join("bootstrap.json"))?;
    let expected: Value =
        serde_json::from_slice(&read(candidate.join("candidate-bootstrap.json"))?)
            .map_err(|error| error.to_string())?;
    let (actual, approval) = candidate_from_allowlist(&allowlist, &context, &current)?;
    let actual_value: Value = serde_json::from_slice(&actual).map_err(|error| error.to_string())?;
    if actual_value != expected {
        return Err("host guard produced a different policy".into());
    }
    let (_, repeat) = candidate_from_allowlist(&allowlist, &context, &actual)?;
    if !repeat.changes.is_empty() {
        return Err("the approved destination is not stable on a later component update".into());
    }
    println!(
        "Verified {} exact policy changes and a stable destination",
        approval.changes.len()
    );
    Ok(())
}

fn main() {
    if let Err(error) = check() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
