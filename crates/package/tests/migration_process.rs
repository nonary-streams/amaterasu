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
#[ignore = "requires separately built authentication and pairing import helpers"]
fn default_primary_named_as_auxiliary_still_seeds_login() {
    let authentication_helper = PathBuf::from(
        std::env::var_os("AMATERASU_AUTHENTICATION_IMPORT_EXE")
            .expect("set AMATERASU_AUTHENTICATION_IMPORT_EXE"),
    );
    let pairing_helper = PathBuf::from(
        std::env::var_os("AMATERASU_PAIRING_IMPORT_EXE").expect("set AMATERASU_PAIRING_IMPORT_EXE"),
    );
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
        "vibeshine_file_state = sunshine_state.json\nfps = 120\n",
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
    let verifier = "0123456789abcdef".repeat(4);
    fs::write(
        source.join("sunshine_state.json"),
        serde_json::to_vec(&serde_json::json!({
            "username": "admin",
            "password": verifier,
            "salt": "1234567890abcdef",
            "root": {"uniqueid": "12345678-1234-4234-8234-123456789abc", "named_devices": []}
        }))
        .unwrap(),
    )
    .unwrap();

    let result = import(&migration, &source, &destination, &runtime);
    assert!(result.status.success(), "{}", output_text(&result));
    let state: Value =
        serde_json::from_slice(&fs::read(authentication_namespace(&runtime)).unwrap()).unwrap();
    assert_eq!(
        state["values"]["authentication/v1"]["credential"]["password_hash"],
        verifier
    );
    assert!(runtime.join("installer-import-validated.json").is_file());
    fs::remove_dir_all(root).unwrap();
}

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

    write_credential(&credential, &"0123456789abcdef".repeat(4));
    let second_source = root.join("second-predecessor");
    let second_credential = second_source.join("credentials/login.json");
    fs::create_dir_all(second_credential.parent().unwrap()).unwrap();
    fs::write(
        second_source.join("vibeshine.conf"),
        "credentials_file = credentials/login.json\n",
    )
    .unwrap();
    write_credential(&second_credential, &"fedcba9876543210".repeat(4));
    let ambiguous_destination = root.join("ambiguous-data");
    let ambiguous_runtime = root.join("ambiguous-runtime");
    fs::create_dir_all(&ambiguous_runtime).unwrap();
    let ambiguous = Command::new(&migration)
        .args(["import", "--destination"])
        .arg(&ambiguous_destination)
        .arg("--runtime")
        .arg(&ambiguous_runtime)
        .arg("--source")
        .arg(format!("sunshine={}", source.display()))
        .arg("--source")
        .arg(format!("vibeshine={}", second_source.display()))
        .output()
        .unwrap();
    assert!(!ambiguous.status.success(), "two credentials were accepted");
    assert!(!ambiguous_destination.exists());
    assert!(!authentication_namespace(&ambiguous_runtime).exists());
    assert!(
        !ambiguous_runtime
            .join("installer-import-validated.json")
            .exists()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires a separately built authentication import helper"]
fn valid_credential_swap_after_helper_approval_preserves_previous_seed() {
    let helper = PathBuf::from(
        std::env::var_os("AMATERASU_AUTHENTICATION_IMPORT_EXE")
            .expect("set AMATERASU_AUTHENTICATION_IMPORT_EXE"),
    );
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
    fs::copy(&helper, &staged_helper).unwrap();
    fs::write(
        source.join("sunshine.conf"),
        "credentials_file = credentials/login.json\n",
    )
    .unwrap();
    let credential = source.join("credentials/login.json");
    let approved_hash = "0123456789abcdef".repeat(4);
    let replacement_hash = "fedcba9876543210".repeat(4);
    write_credential(&credential, &approved_hash);
    let first = import(&migration, &source, &destination, &runtime);
    assert!(first.status.success(), "{}", output_text(&first));
    let seeded = authentication_namespace(&runtime);
    let previous_seed = fs::read(&seeded).unwrap();

    // This process delegates to the real validator, then swaps in another
    // valid verifier before migration resumes copying the predecessor files.
    let wrapper_source = binaries.join("credential-swap-helper.rs");
    fs::write(
        &wrapper_source,
        r#"use std::{env, fs, process::{self, Command}};
fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let status = Command::new(env::var_os("REAL_AUTH_HELPER").unwrap())
        .args(&args).status().unwrap();
    if status.success() && args.first().is_some_and(|arg| arg == &env::var_os("SWAP_ON").unwrap()) {
        let source = args.get(2).unwrap();
        if args.first().is_some_and(|arg| arg == "seed")
            || source == &env::var_os("SWAP_SOURCE").unwrap() {
            fs::write(env::var_os("SWAP_CREDENTIAL").unwrap(),
                env::var_os("SWAP_CONTENT").unwrap().to_string_lossy().as_bytes()).unwrap();
        }
    }
    process::exit(status.code().unwrap_or(1));
}"#,
    )
    .unwrap();
    let compile = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&wrapper_source)
        .arg("-o")
        .arg(&staged_helper)
        .output()
        .unwrap();
    assert!(compile.status.success(), "{}", output_text(&compile));
    let replacement = serde_json::json!({
        "username": "admin", "password": replacement_hash, "salt": "1234567890abcdef"
    })
    .to_string();
    let retry = Command::new(&migration)
        .args(["import", "--destination"])
        .arg(&destination)
        .arg("--runtime")
        .arg(&runtime)
        .arg("--source")
        .arg(format!("sunshine={}", source.display()))
        .env("REAL_AUTH_HELPER", &helper)
        .env("SWAP_SOURCE", &source)
        .env("SWAP_CREDENTIAL", &credential)
        .env("SWAP_CONTENT", &replacement)
        .env("SWAP_ON", "validate")
        .output()
        .unwrap();
    assert!(!retry.status.success(), "swapped credential was accepted");
    assert_eq!(fs::read(&seeded).unwrap(), previous_seed);
    assert_eq!(
        fs::read(destination.join("legacy-imports/sunshine/credentials/login.json")).unwrap(),
        serde_json::to_vec(&serde_json::json!({
            "username": "admin", "password": approved_hash, "salt": "1234567890abcdef"
        }))
        .unwrap()
    );
    assert!(!runtime.join("installer-import-validated.json").exists());

    // A fresh destination cannot reject the changed credential through an
    // existing archive conflict. The approval binding itself must stop it.
    write_credential(&credential, &approved_hash);
    let fresh_destination = root.join("fresh-data");
    let fresh_runtime = root.join("fresh-runtime");
    fs::create_dir_all(&fresh_runtime).unwrap();
    let fresh_attempt = Command::new(&migration)
        .args(["import", "--destination"])
        .arg(&fresh_destination)
        .arg("--runtime")
        .arg(&fresh_runtime)
        .arg("--source")
        .arg(format!("sunshine={}", source.display()))
        .env("REAL_AUTH_HELPER", &helper)
        .env("SWAP_SOURCE", &source)
        .env("SWAP_CREDENTIAL", &credential)
        .env("SWAP_CONTENT", &replacement)
        .env("SWAP_ON", "validate")
        .output()
        .unwrap();
    assert!(!fresh_attempt.status.success());
    assert!(
        output_text(&fresh_attempt)
            .contains("predecessor sensitive state changed during validation"),
        "{}",
        output_text(&fresh_attempt)
    );
    assert!(!authentication_namespace(&fresh_runtime).exists());
    assert!(
        !fresh_runtime
            .join("installer-import-validated.json")
            .exists()
    );
    assert!(!fresh_destination.exists());

    // A source edit after the helper has durably seeded A must also prevent
    // handoff approval, even though the old authentication state is retained.
    write_credential(&credential, &approved_hash);
    let seeded_destination = root.join("seeded-data");
    let seeded_runtime = root.join("seeded-runtime");
    fs::create_dir_all(&seeded_runtime).unwrap();
    let after_seed = Command::new(&migration)
        .args(["import", "--destination"])
        .arg(&seeded_destination)
        .arg("--runtime")
        .arg(&seeded_runtime)
        .arg("--source")
        .arg(format!("sunshine={}", source.display()))
        .env("REAL_AUTH_HELPER", &helper)
        .env(
            "SWAP_SOURCE",
            seeded_destination.join("legacy-imports/sunshine"),
        )
        .env("SWAP_CREDENTIAL", &credential)
        .env("SWAP_CONTENT", &replacement)
        .env("SWAP_ON", "seed")
        .output()
        .unwrap();
    assert!(
        !after_seed.status.success(),
        "{}; source: {}",
        output_text(&after_seed),
        String::from_utf8_lossy(&fs::read(&credential).unwrap())
    );
    assert!(
        output_text(&after_seed)
            .contains("predecessor configuration changed after sensitive seeding"),
        "{}",
        output_text(&after_seed)
    );
    assert!(
        !seeded_runtime
            .join("installer-import-validated.json")
            .exists()
    );
    let seeded_state: Value =
        serde_json::from_slice(&fs::read(authentication_namespace(&seeded_runtime)).unwrap())
            .unwrap();
    assert_eq!(
        seeded_state["values"]["authentication/v1"]["credential"]["password_hash"],
        approved_hash
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

    // Even a valid login must not be seeded when the auxiliary state contains
    // an unsupported token. Preflight must leave both import and runtime clean.
    fs::write(
        source.join("private/auxiliary.json"),
        br#"{"root":{"api_tokens":[{"token":"unsupported"}]}}"#,
    )
    .unwrap();
    let unsupported = import(&migration, &source, &destination, &runtime);
    assert!(
        !unsupported.status.success(),
        "unsupported token was accepted"
    );
    assert!(
        !destination.exists(),
        "failed preflight copied predecessor files"
    );
    assert!(!authentication_namespace(&runtime).exists());
    assert!(!runtime.join("installer-import-validated.json").exists());
    fs::write(source.join("private/auxiliary.json"), auxiliary_bytes).unwrap();

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
