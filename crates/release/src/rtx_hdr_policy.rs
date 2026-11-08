//! Review-only preparation of an RTX HDR bootstrap policy candidate.
//!
//! The report is not an activation record. An installed-service transaction
//! must recheck the source digest while holding its own authority and coordinate
//! publishing the policy with selecting the matching signed native package.
use amaterasu_package::{BootstrapConfig, PackageResult, read_bounded, sha256};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub fn prepare(
    root: &Path,
    expected_digest: &str,
    output: &Path,
    selection: &crate::profiles::linux_session::ProcessingSelection,
) -> PackageResult<()> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let output_parent = output
        .parent()
        .ok_or("output must have a parent directory")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if output_parent.starts_with(&root) {
        return Err("review output must be outside the installation root".into());
    }
    let bootstrap_path = root.join("bootstrap.json");
    let source = read_bounded(&bootstrap_path, 1024 * 1024)?;
    if sha256(&source) != expected_digest {
        return Err("bootstrap changed; inspect its digest and retry".into());
    }
    let mut bootstrap: BootstrapConfig =
        serde_json::from_slice(&source).map_err(|error| error.to_string())?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("RTX HDR preparation requires a signed production bootstrap".into());
    }
    let configured_parent = bootstrap
        .policy
        .settings
        .get("amaterasu.management")
        .and_then(|value| value.get("rtx_hdr_runtime_parent"))
        .and_then(|value| value.as_str())
        .ok_or("trusted RTX HDR runtime parent is unavailable")?;
    let expected_parent = root.join("data/state/rtx-hdr");
    if Path::new(configured_parent) != expected_parent {
        return Err(
            "trusted RTX HDR runtime parent differs from the installation state directory".into(),
        );
    }
    crate::profiles::linux_session::enable_rtx_hdr_with_selection(
        &mut bootstrap.policy,
        &expected_parent.join("runtime"),
        selection,
    )?;
    let report = serde_json::to_vec_pretty(&json!({
        "kind": "rtx_hdr_bootstrap_review_v1",
        "source_bootstrap_sha256": expected_digest,
        "candidate_bootstrap": bootstrap,
    }))
    .map_err(|error| error.to_string())?;
    if sha256(&read_bounded(&bootstrap_path, 1024 * 1024)?) != expected_digest {
        return Err("bootstrap changed during preparation; retry".into());
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let path = output_parent.join(output.file_name().ok_or("output needs a file name")?);
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    if let Err(error) = file.write_all(&report).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error.to_string());
    }
    println!(
        "Prepared review report at {}. It does not activate RTX HDR.",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::{random_identifier, write_json_atomically};

    #[test]
    fn report_is_compare_checked_and_does_not_change_the_installation() {
        let base = std::env::temp_dir().join(format!(
            "amaterasu-rtx-review-{}",
            random_identifier().unwrap()
        ));
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut policy = crate::profiles::linux_session::linux_session_policy(
            &root,
            &crate::profiles::linux_session::options::LinuxSessionOptions {
                frame_helper: Some("/usr/libexec/amaterasu-frame-helper".into()),
                ..Default::default()
            },
        )
        .unwrap();
        policy
            .settings
            .insert("example.other".into(), json!({"preserved":true}));
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: [("publisher".into(), "trusted".into())].into(),
            policy,
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        let before = fs::read(root.join("bootstrap.json")).unwrap();
        let digest = sha256(&before);
        let capabilities = crate::profiles::linux_session::default_rtx_hdr_selection();
        assert!(
            prepare(
                &root,
                &"0".repeat(64),
                &base.join("stale.json"),
                &capabilities
            )
            .is_err()
        );
        assert!(!base.join("stale.json").exists());
        assert!(prepare(&root, &digest, &root.join("forbidden.json"), &capabilities).is_err());
        let output = base.join("candidate.json");
        prepare(&root, &digest, &output, &capabilities).unwrap();
        assert!(prepare(&root, &digest, &output, &capabilities).is_err());
        assert_eq!(fs::read(root.join("bootstrap.json")).unwrap(), before);
        let report: serde_json::Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
        assert_eq!(report["source_bootstrap_sha256"], digest);
        assert_eq!(
            report["candidate_bootstrap"]["policy"]["settings"]["example.other"]["preserved"],
            true
        );
        assert_eq!(
            report["candidate_bootstrap"]["policy"]["processing_compositions"]["amaterasu.linux-session-video"]
                ["processor"]["service"],
            "amaterasu.rtx-hdr/1"
        );
        fs::remove_dir_all(base).unwrap();
    }
}
