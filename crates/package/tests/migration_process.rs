//! External-process acceptance for the installer credential migration path.
//!
//! Build `amaterasu-authentication-import` separately, then set
//! `AMATERASU_AUTHENTICATION_IMPORT_EXE` to its executable when running the
//! ignored test. No service, MSI registration, or installed data is touched.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires a separately built authentication import helper"]
fn credential_import_seeds_runtime_and_failed_retry_revokes_handoff_approval() {
    let helper = PathBuf::from(
        std::env::var_os("AMATERASU_AUTHENTICATION_IMPORT_EXE")
            .expect("set AMATERASU_AUTHENTICATION_IMPORT_EXE to the built helper"),
    );
    assert!(helper.is_file(), "authentication import helper must exist");
    let root = unique_fixture_root();
    let binaries = root.join("bin");
    let source = root.join("predecessor");
    let destination = root.join("data");
    let runtime = root.join("runtime");
    fs::create_dir_all(&binaries).unwrap();
    fs::create_dir_all(source.join("credentials")).unwrap();
    fs::create_dir_all(&runtime).unwrap();

    let migration = binaries.join(format!(
        "amaterasu-migration{}",
        std::env::consts::EXE_SUFFIX
    ));
    let staged_helper = binaries.join(format!(
        "amaterasu-authentication-import{}",
        std::env::consts::EXE_SUFFIX
    ));
    fs::copy(env!("CARGO_BIN_EXE_amaterasu-migration"), &migration).unwrap();
    fs::copy(helper, staged_helper).unwrap();

    fs::write(
        source.join("sunshine.conf"),
        "credentials_file = credentials/login.json\n",
    )
    .unwrap();
    let credential = source.join("credentials/login.json");
    fs::write(
        &credential,
        r#"{"username":"admin","password":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","salt":"1234567890abcdef"}"#,
    )
    .unwrap();

    let first = import(&migration, &source, &destination, &runtime);
    assert!(
        first.status.success(),
        "first import failed: {}",
        output_text(&first)
    );

    let marker = runtime.join("installer-import-validated.json");
    let approval: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    assert_eq!(approval["validated"], true);
    assert_eq!(approval["families"], serde_json::json!(["Sunshine"]));

    let seeded = authentication_namespace(&runtime);
    let state: Value = serde_json::from_slice(&fs::read(&seeded).unwrap()).unwrap();
    assert_eq!(
        state["values"]["authentication/v1"]["credential"]["username"],
        "admin"
    );
    assert_eq!(
        state["values"]["authentication/v1"]["credential"]["password_hash"],
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    );

    // The next attempt fails helper validation before it can issue fresh approval.
    fs::write(&credential, b"invalid credential json").unwrap();
    let retry = import(&migration, &source, &destination, &runtime);
    assert!(
        !retry.status.success(),
        "invalid retry unexpectedly succeeded"
    );
    assert!(
        !marker.exists(),
        "stale handoff approval survived failed retry"
    );
    assert!(
        seeded.exists(),
        "failed retry removed the durable auth seed"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires separately built authentication and pairing import helpers"]
fn failed_process_import_rolls_back_all_configured_files_and_retry_seeds_credential() {
    let authentication_helper = PathBuf::from(
        std::env::var_os("AMATERASU_AUTHENTICATION_IMPORT_EXE")
            .expect("set AMATERASU_AUTHENTICATION_IMPORT_EXE to the built helper"),
    );
    let pairing_helper = PathBuf::from(
        std::env::var_os("AMATERASU_PAIRING_IMPORT_EXE")
            .expect("set AMATERASU_PAIRING_IMPORT_EXE to the built helper"),
    );
    let root = unique_fixture_root();
    let binaries = root.join("bin");
    let source = root.join("predecessor");
    let destination = root.join("data");
    let blocked_runtime = root.join("blocked-runtime");
    let runtime = root.join("runtime");
    fs::create_dir_all(&binaries).unwrap();
    fs::create_dir_all(source.join("credentials")).unwrap();
    fs::create_dir_all(source.join("private")).unwrap();
    fs::create_dir_all(&blocked_runtime).unwrap();
    fs::create_dir_all(&runtime).unwrap();

    let migration = binaries.join(format!(
        "amaterasu-migration{}",
        std::env::consts::EXE_SUFFIX
    ));
    fs::copy(env!("CARGO_BIN_EXE_amaterasu-migration"), &migration).unwrap();
    fs::copy(
        authentication_helper,
        binaries.join(format!(
            "amaterasu-authentication-import{}",
            std::env::consts::EXE_SUFFIX
        )),
    )
    .unwrap();
    fs::copy(
        pairing_helper,
        binaries.join(format!(
            "amaterasu-pairing-import{}",
            std::env::consts::EXE_SUFFIX
        )),
    )
    .unwrap();

    fs::write(
        source.join("sunshine.conf"),
        "credentials_file = credentials/login.json\nfile_state = private/host.json\nvibeshine_file_state = private/auxiliary.json\nfps = 120\n",
    )
    .unwrap();
    fs::write(
        source.join("credentials/cakey.pem"),
        include_str!("../../../modules/pairing/tests/fixtures/host-key.pem"),
    )
    .unwrap();
    fs::write(
        source.join("credentials/cacert.pem"),
        include_str!("../../../modules/pairing/tests/fixtures/host-cert.pem"),
    )
    .unwrap();
    let state = serde_json::json!({
        "root": {"uniqueid": "12345678-1234-4234-8234-123456789abc", "named_devices": []}
    });
    let state_bytes = serde_json::to_vec(&state).unwrap();
    fs::write(source.join("private/host.json"), &state_bytes).unwrap();
    let auxiliary_bytes = br#"{"root":{}}"#;
    fs::write(source.join("private/auxiliary.json"), auxiliary_bytes).unwrap();
    let credential = source.join("credentials/login.json");
    let first_hash = "0123456789abcdef".repeat(4);
    let corrected_hash = "fedcba9876543210".repeat(4);
    write_credential(&credential, &first_hash);

    // Another Amaterasu configuration occupies the target. The failed process
    // must remove its new archive files before the installer can retry.
    let existing_source = root.join("existing-source");
    fs::create_dir_all(&existing_source).unwrap();
    fs::write(existing_source.join("sunshine.conf"), "fps = 60\n").unwrap();
    let occupied = import(
        &migration,
        &existing_source,
        &root.join("existing-data"),
        &blocked_runtime,
    );
    assert!(occupied.status.success(), "{}", output_text(&occupied));
    let failed = import(&migration, &source, &destination, &blocked_runtime);
    assert!(!failed.status.success(), "occupied runtime accepted import");
    let archived = destination.join("legacy-imports/sunshine");
    for relative in [
        "credentials/login.json",
        "private/host.json",
        "private/auxiliary.json",
        "sunshine.conf",
    ] {
        assert!(
            !archived.join(relative).exists(),
            "{relative} survived rollback"
        );
    }
    assert!(
        !blocked_runtime
            .join("installer-import-validated.json")
            .exists()
    );

    write_credential(&credential, &corrected_hash);
    let retry = import(&migration, &source, &destination, &runtime);
    assert!(retry.status.success(), "{}", output_text(&retry));
    assert_eq!(
        fs::read(archived.join("private/host.json")).unwrap(),
        state_bytes
    );
    assert_eq!(
        fs::read(archived.join("private/auxiliary.json")).unwrap(),
        auxiliary_bytes
    );
    assert_eq!(
        fs::read(archived.join("credentials/login.json")).unwrap(),
        fs::read(&credential).unwrap()
    );
    let persisted: Value =
        serde_json::from_slice(&fs::read(authentication_namespace(&runtime)).unwrap()).unwrap();
    assert_eq!(
        persisted["values"]["authentication/v1"]["credential"]["password_hash"],
        corrected_hash
    );
    assert!(runtime.join("installer-import-validated.json").is_file());

    // The installer may restart after import succeeded but before predecessor
    // handoff. Repeating the same snapshot must restore its approval without
    // replacing the already seeded credential or published settings.
    let published_configuration = runtime
        .join("data/state/namespaces")
        .join(
            b"amaterasu.configuration"
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
        .join("state.json");
    let configuration_bytes = fs::read(&published_configuration).unwrap();
    let retry = import(&migration, &source, &destination, &runtime);
    assert!(retry.status.success(), "{}", output_text(&retry));
    assert_eq!(
        fs::read(&published_configuration).unwrap(),
        configuration_bytes
    );
    assert!(runtime.join("installer-import-validated.json").is_file());

    // A predecessor changed after the approved copy is no longer that import.
    // It must not regain permission to retire the predecessor on a retry.
    fs::write(source.join("sunshine.conf"), "fps = 90\n").unwrap();
    let changed = import(&migration, &source, &destination, &runtime);
    assert!(!changed.status.success(), "changed source was accepted");
    assert!(!runtime.join("installer-import-validated.json").exists());

    fs::write(
        source.join("sunshine.conf"),
        "credentials_file = credentials/login.json\nfile_state = private/host.json\nvibeshine_file_state = private/auxiliary.json\nfps = 120\n",
    )
    .unwrap();
    let edited_configuration = serde_json::json!({
        "format_version": 1,
        "revision": 2,
        "values": {}
    });
    fs::write(
        &published_configuration,
        serde_json::to_vec(&edited_configuration).unwrap(),
    )
    .unwrap();
    let edited_retry = import(&migration, &source, &destination, &runtime);
    assert!(
        !edited_retry.status.success(),
        "edited runtime was accepted"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&published_configuration).unwrap()).unwrap(),
        edited_configuration
    );
    assert!(!runtime.join("installer-import-validated.json").exists());
    fs::remove_dir_all(root).unwrap();
}

fn write_credential(path: &Path, password_hash: &str) {
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "username": "admin",
            "password": password_hash,
            "salt": "1234567890abcdef"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn import(migration: &Path, source: &Path, destination: &Path, runtime: &Path) -> Output {
    Command::new(migration)
        .arg("import")
        .arg("--destination")
        .arg(destination)
        .arg("--runtime")
        .arg(runtime)
        .arg("--source")
        .arg(format!("sunshine={}", source.display()))
        .output()
        .unwrap()
}

fn authentication_namespace(runtime: &Path) -> PathBuf {
    let encoded: String = b"amaterasu.authentication"
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut path = runtime.join("data/state/namespaces");
    for part in encoded.as_bytes().chunks(64) {
        path.push(std::str::from_utf8(part).unwrap());
    }
    path.join("state.json")
}

fn unique_fixture_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "amaterasu-migration-process-{}-{nonce}",
        std::process::id()
    ))
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
