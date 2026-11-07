//! Administrative software qualification for the real Linux privilege boundary.
//! No capture hardware or driver ioctl is exercised by these fixtures.
#![cfg(target_os = "linux")]

use amaterasu_capture_authority::{ApprovalRecord, ApprovalStore, LaunchOptions};
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_package::{
    ActivationState, BootstrapConfig, ComponentManifest, PrivilegedHelperArtifact,
    PrivilegedOperation, PrivilegedRequest, PrivilegedResourceScope, ReleaseManifest,
    ReleaseSignature, VerificationPolicy, current_target, encode_hex, sha256, unix_timestamp,
    verify_package,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

struct TestRoot(PathBuf);
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn directory(path: &Path, mode: u32) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn json(path: &Path, value: &impl serde::Serialize) {
    write(path, &serde_json::to_vec_pretty(value).unwrap(), 0o444);
}
fn options(root: &Path, id: &str) -> LaunchOptions {
    LaunchOptions {
        approval_store: root.join("approvals"),
        approval_id: id.to_owned(),
        package_root: root.join("package"),
        worker: root.join("worker"),
        state_root: root.join("state"),
    }
}
fn until_exists(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires root to create protected /run state and launch a disposable nobody worker"]
fn real_broker_scopes_sessions_and_revokes_a_running_unprivileged_worker() {
    // SAFETY: geteuid only observes this process's credential.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this ignored test as root"
    );
    let root = TestRoot(PathBuf::from(format!(
        "/run/amaterasu-authority-test-{}-{}",
        std::process::id(),
        amaterasu_package::random_identifier().unwrap()
    )));
    directory(&root.0, 0o755);
    let fixture = root.0.join("fixture");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authority_peer.rs");
    let compilation = Command::new("rustc")
        .arg("--edition=2024")
        .arg("-C")
        .arg("opt-level=1")
        .arg(source)
        .arg("-o")
        .arg(&fixture)
        .output()
        .expect("rustc must be available for the administrative fixture");
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let executable = fs::read(&fixture).unwrap();
    write(&root.0.join("worker"), &executable, 0o555);
    directory(&root.0.join("state"), 0o700);
    let state_c =
        std::ffi::CString::new(root.0.join("state").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the terminated path is the disposable state directory created above.
    assert_eq!(unsafe { libc::chown(state_c.as_ptr(), 65534, 65534) }, 0);
    assert_eq!(fs::metadata(root.0.join("state")).unwrap().uid(), 65534);
    let package_root = root.0.join("package");
    directory(&package_root, 0o755);
    directory(&package_root.join("sets"), 0o755);
    let signing_key = SigningKey::from_bytes(&[217; 32]);
    let bootstrap = BootstrapConfig {
        development: false,
        allow_unsigned: false,
        trusted_keys: BTreeMap::from([(
            "fixture".into(),
            encode_hex(signing_key.verifying_key().as_bytes()),
        )]),
        policy: RuntimeConfig::default(),
    };
    json(&package_root.join("bootstrap.json"), &bootstrap);
    let artifact = b"not loaded: fixture worker implements only the broker protocol";
    let component = |id: &str, file: &str, runtime| ComponentManifest {
        id: id.into(),
        version: "0.1.0".into(),
        file: file.into(),
        sha256: sha256(artifact),
        size: artifact.len() as u64,
        abi_major: 1,
        runtime,
        requires: BTreeMap::new(),
        requested: Grants::default(),
    };
    let manifest = ReleaseManifest {
        schema: 1,
        host_context_schema: 1,
        project: "amaterasu".into(),
        sequence: 1,
        expires_unix: unix_timestamp() + 3600,
        target: current_target(),
        components: vec![
            component("fixture.runtime", "runtime.bin", true),
            component("fixture.capture", "capture.bin", false),
        ],
        privileged_requests: vec![PrivilegedRequest {
            schema: 1,
            component: "fixture.capture".into(),
            operation: PrivilegedOperation::VkmsFrameExportV1,
            target: current_target(),
            resource: PrivilegedResourceScope { drm_card: 0 },
            helper: PrivilegedHelperArtifact {
                file: "helper".into(),
                sha256: sha256(&executable),
                size: executable.len() as u64,
            },
        }],
    };
    let raw_manifest = serde_json::to_vec(&manifest).unwrap();
    let digest = sha256(&raw_manifest);
    let package = package_root.join("sets").join(&digest);
    directory(&package, 0o755);
    write(&package.join("manifest.json"), &raw_manifest, 0o444);
    json(
        &package.join("manifest.sig"),
        &ReleaseSignature {
            key_id: "fixture".into(),
            signature: encode_hex(&signing_key.sign(&raw_manifest).to_bytes()),
        },
    );
    write(&package.join("runtime.bin"), artifact, 0o444);
    write(&package.join("capture.bin"), artifact, 0o444);
    let helper = package.join("helper");
    write(&helper, &executable, 0o544);
    json(
        &package_root.join("selection.json"),
        &ActivationState {
            active: Some(digest.clone()),
            highest_sequence: 1,
            ..Default::default()
        },
    );
    let verified = verify_package(
        &package,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: false,
            now: unix_timestamp(),
        },
    )
    .unwrap();
    let record = ApprovalRecord {
        schema: 1,
        request: verified.privileged_requests[0].clone(),
        worker_sha256: sha256(&executable),
        uid: 65534,
        gid: 65534,
    };
    let store = ApprovalStore::create(&root.0.join("approvals")).unwrap();
    let id = store.put(&record).unwrap();
    // Reload from protected on-disk state; no in-memory caller assertion is used.
    assert_eq!(
        ApprovalStore::open(store.path()).unwrap().get(&id).unwrap(),
        record
    );
    // The helper artifact carries no capability and ordinary users cannot run it.
    let refused = Command::new(&helper)
        .arg("--approved-service")
        .uid(65534)
        .gid(65534)
        .status();
    assert!(
        refused.is_err(),
        "an unprivileged process executed the protected helper"
    );
    let run_options = options(&root.0, &id);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let supervisor = thread::spawn(move || {
        let result = amaterasu_capture_authority::run(run_options);
        done_tx.send(result).unwrap();
    });
    until_exists(&root.0.join("state/ready"));
    assert!(root.0.join("state/precheck-ready").exists());
    assert!(root.0.join("state/scope-denied").exists());
    assert!(root.0.join("state/replay-denied").exists());
    let worker_pid: i32 = fs::read_to_string(root.0.join("state/ready"))
        .unwrap()
        .parse()
        .unwrap();
    let helper_pids: Vec<i32> = fs::read_to_string(package.join("helper-pids"))
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert!(
        helper_pids.len() >= 3,
        "expected independently spawned helper sessions"
    );
    store.remove(&id).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("revocation did not stop broker");
    assert!(result.unwrap_err().contains("revoked"));
    supervisor.join().unwrap();
    // SAFETY: signal zero only observes whether the disposable worker still exists.
    assert_eq!(
        unsafe { libc::kill(worker_pid, 0) },
        -1,
        "worker survived authority withdrawal"
    );
    for pid in helper_pids {
        // SAFETY: signal zero only observes whether a disposable helper exists.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "helper survived authority withdrawal"
        );
    }
    assert!(
        amaterasu_capture_authority::run(options(&root.0, &id)).is_err(),
        "restart reused a withdrawn approval"
    );
    store.put(&record).unwrap();
    write(&helper, b"changed helper bytes", 0o544);
    assert!(
        amaterasu_capture_authority::run(options(&root.0, &id)).is_err(),
        "changed helper reused approval"
    );
    store.remove(&id).unwrap();
}
