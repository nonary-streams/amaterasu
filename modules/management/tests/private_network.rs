//! Explicit opt-in only: the Python launcher creates a private Linux network.
#![cfg(target_os = "linux")]

#[test]
#[ignore = "requires built signed-host artifacts, Python cryptography, iproute2 and user/network namespaces; opens isolated non-loopback listeners"]
fn signed_private_network_peer_admission_and_narrowing() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace directory");
    let python = std::env::var_os("AMATERASU_PYTHON").unwrap_or_else(|| "python3".into());
    let result = std::process::Command::new(python)
        .arg(workspace.join("scripts/test_management_private_network.py"))
        .args(["--private-network", "--skip-build"])
        .current_dir(workspace)
        .status()
        .expect("opt-in private-network qualification launcher");
    assert!(
        result.success(),
        "private-network peer qualification failed"
    );
}
