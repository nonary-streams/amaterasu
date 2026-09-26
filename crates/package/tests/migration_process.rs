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
