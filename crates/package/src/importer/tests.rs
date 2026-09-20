use super::*;
use crate::migration::{ImportedApplication, LegacyProduct};
use crate::{create_private_directory, random_identifier, write_json_atomically};

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-importer-test-{}",
            random_identifier().unwrap()
        ));
        create_private_directory(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runtime_root(fixture: &TemporaryDirectory) -> PathBuf {
    let runtime = fixture.path().join("runtime");
    let state = runtime.join("data/state");
    create_private_directory(&state).unwrap();
    write_json_atomically(
        &runtime.join("bootstrap.json"),
        &json!({"policy":{"settings":{"amaterasu.state":{"data_directory":state}}}}),
    )
    .unwrap();
    runtime
}

fn plan() -> ImportPlan {
    ImportPlan {
        schema: 1,
        applications: vec![ImportedApplication {
            product: LegacyProduct::Sunshine,
            name: "Example".into(),
            executable: r"C:\Games\Example.exe".into(),
            arguments: vec![],
            working_directory: Some(r"C:\Games".into()),
        }],
        settings: BTreeMap::from([
            ("stream.default_fps".into(), "120".into()),
            ("input.keyboard_enabled".into(), "enabled".into()),
            ("video.encoder_preference".into(), "NVENC".into()),
            ("stream.default_resolution".into(), "2560x1440".into()),
            ("audio.sink".into(), "Speakers".into()),
        ]),
        unsupported_items: vec![],
        critical_items: vec![],
    }
}

#[test]
fn materializes_exact_apps_and_safe_settings_in_runtime_state() {
    let fixture = TemporaryDirectory::new();
    let runtime = runtime_root(&fixture);
    let receipt = materialize_import_plan(&runtime, &plan()).unwrap();
    assert_eq!(receipt.applications, 1);
    assert_eq!(receipt.settings, 5);

    let manifest: Value = crate::read_json(&receipt.state_directory.join("namespaces/616d617465726173752e636174616c6f67/collections/7072696e636970616c2f616d617465726173752e6d616e6167656d656e74/manifest.json")).unwrap();
    assert_eq!(manifest["metadata"]["next_id"], 2);
    let entries: Value = crate::read_json(&receipt.state_directory.join("namespaces/616d617465726173752e636174616c6f67/collections/7072696e636970616c2f616d617465726173752e6d616e6167656d656e74/shard-0000000000000001-00000000.json")).unwrap();
    assert_eq!(
        entries["app-0000000000000001"]["launch"]["program"],
        r"C:\Games\Example.exe"
    );
    assert_eq!(
        entries["app-0000000000000001"]["launch"]["working_directory"],
        r"C:\Games"
    );

    let configuration: Value = crate::read_json(
        &receipt
            .state_directory
            .join("namespaces/616d617465726173752e636f6e66696775726174696f6e/state.json"),
    )
    .unwrap();
    let values = &configuration["values"][CONFIGURATION_KEY]["values"];
    assert_eq!(values["stream.default_fps"], 120);
    assert_eq!(values["input.keyboard_enabled"], true);
    assert_eq!(values["video.encoder_preference"], "nvenc");
}

#[test]
fn refuses_existing_state_before_writing_any_imported_record() {
    let fixture = TemporaryDirectory::new();
    let runtime = runtime_root(&fixture);
    let state = runtime.join("data/state");
    let configuration = namespace_path(&state, CONFIGURATION_CALLER);
    create_private_directory(configuration.parent().unwrap()).unwrap();
    write_json_atomically(
        &configuration,
        &json!({"format_version":1,"revision":4,"values":{}}),
    )
    .unwrap();

    let error = materialize_import_plan(&runtime, &plan()).unwrap_err();
    assert!(error.contains("configuration already"));
    assert!(!collection_directory(&state, CATALOG_CALLER, CATALOG_COLLECTION).exists());
}

#[test]
fn excludes_invalid_safe_setting_values_without_reparsing_legacy_files() {
    let fixture = TemporaryDirectory::new();
    let runtime = runtime_root(&fixture);
    let mut plan = plan();
    plan.applications.clear();
    plan.settings
        .insert("stream.default_fps".into(), "unbounded".into());
    plan.settings
        .insert("input.keyboard_enabled".into(), "sometimes".into());
    let receipt = materialize_import_plan(&runtime, &plan).unwrap();
    assert_eq!(receipt.settings, 3);
    let configuration: Value = crate::read_json(
        &receipt
            .state_directory
            .join("namespaces/616d617465726173752e636f6e66696775726174696f6e/state.json"),
    )
    .unwrap();
    let values = &configuration["values"][CONFIGURATION_KEY]["values"];
    assert!(values.get("stream.default_fps").is_none());
    assert!(values.get("input.keyboard_enabled").is_none());
}

#[test]
fn catalog_failure_rolls_back_configuration_and_leaves_no_retry_marker() {
    let fixture = TemporaryDirectory::new();
    let runtime = runtime_root(&fixture);
    let error = materialize_import_plan_inner(&runtime, &plan(), true).unwrap_err();
    assert!(error.contains("injected catalog"));
    let state = runtime_state_directory(&runtime).unwrap();
    assert!(!namespace_path(&state, CONFIGURATION_CALLER).exists());
    assert!(!collection_directory(&state, CATALOG_CALLER, CATALOG_COLLECTION).exists());
    assert!(!state.join(IMPORT_TRANSACTION_FILE).exists());
    materialize_import_plan(&runtime, &plan()).unwrap();
}
