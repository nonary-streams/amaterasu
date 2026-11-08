//! Review-only preparation for an independently supplied frame processor.
use amaterasu_contracts::processing_composition::ProcessingSelection;
use amaterasu_package::{
    BootstrapConfig, ComponentManifest, PackageResult, read_bounded, read_json, sha256,
};
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::Path};

pub fn prepare(
    root: &Path,
    expected_digest: &str,
    output: &Path,
    selection_path: &Path,
    component_path: &Path,
    settings_path: &Path,
    metadata_path: Option<&Path>,
) -> PackageResult<()> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let output_parent = output
        .parent()
        .ok_or("output must have a parent")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if output_parent.starts_with(&root) {
        return Err("review output must be outside the installation root".into());
    }
    let bootstrap_path = root.join("bootstrap.json");
    let source = read_bounded(&bootstrap_path, 1024 * 1024)?;
    if sha256(&source) != expected_digest {
        return Err("bootstrap changed; retry review".into());
    }
    let mut bootstrap: BootstrapConfig =
        serde_json::from_slice(&source).map_err(|error| error.to_string())?;
    if bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("processing review requires trusted package signers".into());
    }
    let selection: ProcessingSelection = read_json(selection_path)?;
    let component: ComponentManifest = read_json(component_path)?;
    let settings: Value = read_json(settings_path)?;
    if component.id != selection.processor.principal
        || component.runtime
        || !component
            .requested
            .native_provide
            .contains(&selection.processor.service)
        || !settings.is_object()
        || serde_json::to_vec(&settings)
            .map_err(|error| error.to_string())?
            .len()
            > 8192
    {
        return Err(
            "component manifest or private settings do not match the selected processor".into(),
        );
    }
    match bootstrap.policy.principals.get(&component.id) {
        Some(existing) if !contains_grants(existing, &component.requested) => {
            return Err(
                "processor requests grants not already approved for its existing principal".into(),
            );
        }
        Some(_) => {}
        None => {
            bootstrap
                .policy
                .principals
                .insert(component.id.clone(), component.requested.clone());
        }
    }
    match bootstrap.policy.settings.get(&component.id) {
        Some(existing) if existing != &settings => {
            return Err("processor settings conflict with existing policy".into());
        }
        Some(_) => {}
        None => {
            bootstrap
                .policy
                .settings
                .insert(component.id.clone(), settings);
        }
    }
    let metadata = match metadata_path {
        Some(path) => {
            String::from_utf8(read_bounded(path, 256 * 1024)?).map_err(|error| error.to_string())?
        }
        None => String::new(),
    };
    crate::profiles::install_processing(&mut bootstrap.policy, &selection, &metadata)?;
    let report = serde_json::to_vec_pretty(&json!({"kind":"processing_bootstrap_review_v1",
        "source_bootstrap_sha256":expected_digest, "selection":selection,
        "component":component, "candidate_bootstrap":bootstrap}))
    .map_err(|error| error.to_string())?;
    if sha256(&read_bounded(&bootstrap_path, 1024 * 1024)?) != expected_digest {
        return Err("bootstrap changed during review".into());
    }
    let path = output_parent.join(output.file_name().ok_or("output needs a file name")?);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
    if let Err(error) = file.write_all(&report).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.to_string());
    }
    println!(
        "Prepared processing review {} (SHA-256 {}).",
        path.display(),
        sha256(&report)
    );
    Ok(())
}

fn contains_grants(
    existing: &amaterasu_contracts::Grants,
    required: &amaterasu_contracts::Grants,
) -> bool {
    required.native_provide.is_subset(&existing.native_provide)
        && required.native_use.is_subset(&existing.native_use)
        && required.provide.is_subset(&existing.provide)
        && required.call.is_subset(&existing.call)
        && required.publish.is_subset(&existing.publish)
        && required.subscribe.is_subset(&existing.subscribe)
        && required.hook.is_subset(&existing.hook)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::{random_identifier, read_json, write_json_atomically};

    #[test]
    fn review_is_digest_bound_and_does_not_modify_the_development_root() {
        let base = std::env::temp_dir().join(format!(
            "processing-review-{}",
            random_identifier().unwrap()
        ));
        let root = base.join("root");
        crate::development::initialize_linux_session(
            &root,
            &crate::profiles::linux_session::options::LinuxSessionOptions {
                external_extension_composition: true,
                ..Default::default()
            },
        )
        .unwrap();
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/examples/frame-processor");
        let before = std::fs::read(root.join("bootstrap.json")).unwrap();
        let digest = sha256(&before);
        let output = base.join("review.json");
        assert!(
            prepare(
                &root,
                &"0".repeat(64),
                &output,
                &example.join("selection.json"),
                &example.join("component-linux.json"),
                &example.join("settings.json"),
                None
            )
            .is_err()
        );
        prepare(
            &root,
            &digest,
            &output,
            &example.join("selection.json"),
            &example.join("component-linux.json"),
            &example.join("settings.json"),
            None,
        )
        .unwrap();
        assert_eq!(std::fs::read(root.join("bootstrap.json")).unwrap(), before);
        let report: Value = read_json(&output).unwrap();
        assert_eq!(
            report["candidate_bootstrap"]["policy"]["processing_compositions"]["amaterasu.linux-session-video"]
                ["processor"]["principal"],
            "example.frame-processor"
        );
        assert!(
            prepare(
                &root,
                &digest,
                &output,
                &example.join("selection.json"),
                &example.join("component-linux.json"),
                &example.join("settings.json"),
                None
            )
            .is_err()
        );
        let mut baseline: BootstrapConfig = serde_json::from_slice(&before).unwrap();
        baseline
            .policy
            .principals
            .insert("example.frame-processor".into(), Default::default());
        write_json_atomically(&root.join("bootstrap.json"), &baseline).unwrap();
        let new_digest = sha256(&std::fs::read(root.join("bootstrap.json")).unwrap());
        assert!(
            prepare(
                &root,
                &new_digest,
                &base.join("denied.json"),
                &example.join("selection.json"),
                &example.join("component-linux.json"),
                &example.join("settings.json"),
                None
            )
            .is_err()
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
