use super::*;

fn requested_package() -> TestPackage {
    let mut package = TestPackage::new();
    package.add_feature("example.capture", &[]);
    let helper_bytes = b"privileged helper test bytes; not an executable";
    fs::write(package.directory.join("capture-helper"), helper_bytes).unwrap();
    package
        .manifest
        .privileged_requests
        .push(PrivilegedRequest {
            schema: PRIVILEGED_REQUEST_SCHEMA,
            component: "example.capture".into(),
            operation: PrivilegedOperation::VkmsFrameExportV1,
            target: current_target(),
            resource: PrivilegedResourceScope { drm_card: 1 },
            helper: PrivilegedHelperArtifact {
                file: "capture-helper".into(),
                sha256: sha256(helper_bytes),
                size: helper_bytes.len() as u64,
            },
        });
    package.write_signed_manifest();
    package
}

#[test]
fn legacy_manifest_keeps_no_privileged_request_or_signer_ambiguity() {
    let package = TestPackage::new();
    let bytes = fs::read(package.directory.join("manifest.json")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("privileged_requests"));
    let verified = package.verify().unwrap();
    assert!(verified.privileged_requests.is_empty());
    assert_eq!(
        verified.signer_fingerprint,
        Some(sha256(package.signing_key.verifying_key().as_bytes()))
    );
}

#[test]
#[cfg(target_os = "linux")]
fn signed_request_binds_every_verified_identity_and_preserves_helper() {
    let package = requested_package();
    let verified = package.verify().unwrap();
    assert_eq!(verified.privileged_requests.len(), 1);
    let request = &verified.privileged_requests[0];
    assert_eq!(request.schema, 1);
    assert_eq!(request.manifest_digest, verified.digest);
    assert_eq!(
        Some(&request.signer_fingerprint),
        verified.signer_fingerprint.as_ref()
    );
    assert_eq!(request.component, "example.capture");
    assert_eq!(
        request.component_sha256,
        package.manifest.components[1].sha256
    );
    assert_eq!(request.target, current_target());
    assert_eq!(request.operation, PrivilegedOperation::VkmsFrameExportV1);
    assert_eq!(request.resource.drm_card, 1);
    assert_eq!(
        request.helper,
        package.manifest.privileged_requests[0].helper
    );
    let host_root = package.directory.join("staged");
    let digest = stage_package(&package.directory, &host_root, &package.bootstrap, false).unwrap();
    let staged = package_directory(&host_root, &digest).unwrap();
    assert_eq!(
        fs::read(staged.join("capture-helper")).unwrap(),
        fs::read(package.directory.join("capture-helper")).unwrap()
    );
    assert_eq!(
        verify_package(
            &staged,
            &package.bootstrap,
            VerificationPolicy {
                minimum_sequence: 5,
                allow_historical: false,
                now: unix_timestamp(),
            }
        )
        .unwrap()
        .privileged_requests,
        verified.privileged_requests
    );
}

#[test]
fn unsigned_request_is_denied_even_in_development() {
    let mut package = requested_package();
    package.bootstrap.allow_unsigned = true;
    package.manifest.components[0].id = "example.runtime".into();
    package.write_signed_manifest();
    fs::remove_file(package.directory.join("manifest.sig")).unwrap();
    assert_rejected(package.verify(), "unsigned privileged requests");
}

#[test]
fn rejects_unknown_operation_before_any_authority_is_produced() {
    let package = requested_package();
    let mut value = serde_json::to_value(&package.manifest).unwrap();
    value["privileged_requests"][0]["operation"] = "execute_anything".into();
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(package.directory.join("manifest.json"), &bytes).unwrap();
    write_json_atomically(
        &package.directory.join("manifest.sig"),
        &ReleaseSignature {
            key_id: "test-key".into(),
            signature: encode_hex(&package.signing_key.sign(&bytes).to_bytes()),
        },
    )
    .unwrap();
    assert_rejected(package.verify(), "unknown variant");
}

#[test]
#[cfg(target_os = "linux")]
fn rejects_request_scope_and_artifact_errors() {
    type Change = fn(&mut PrivilegedRequest);
    let cases: &[(Change, &str)] = &[
        (|r| r.schema = 2, "unsupported privileged request schema"),
        (
            |r| r.target = "windows-x86_64".into(),
            "exact Linux package target",
        ),
        (
            |r| r.component = "unpackaged.capture".into(),
            "packaged feature component",
        ),
        (
            |r| r.component = "amaterasu.runtime".into(),
            "packaged feature component",
        ),
        (|r| r.resource.drm_card = 64, "DRM card"),
        (
            |r| r.helper.file = "../capture-helper".into(),
            "simple artifact filename",
        ),
        (
            |r| r.helper.file = "/capture-helper".into(),
            "simple artifact filename",
        ),
        (
            |r| r.helper.file = ".capture-helper".into(),
            "simple artifact filename",
        ),
        (|r| r.helper.size = 0, "size bounds"),
        (
            |r| r.helper.size = MAX_PRIVILEGED_HELPER_BYTES + 1,
            "size bounds",
        ),
        (|r| r.helper.sha256 = "a".repeat(63), "canonical SHA-256"),
        (|r| r.helper.sha256 = "A".repeat(64), "canonical SHA-256"),
        (|r| r.helper.sha256 = "0".repeat(64), "integrity failed"),
        (|r| r.helper.size += 1, "integrity failed"),
    ];
    for (change, reason) in cases {
        let mut package = requested_package();
        change(&mut package.manifest.privileged_requests[0]);
        package.write_signed_manifest();
        assert_rejected(package.verify(), reason);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn rejects_duplicates_component_collisions_and_overlong_lists() {
    let mut package = requested_package();
    package
        .manifest
        .privileged_requests
        .push(package.manifest.privileged_requests[0].clone());
    package.write_signed_manifest();
    assert_rejected(package.verify(), "duplicate privileged component operation");
    package.manifest.privileged_requests.truncate(1);
    package.manifest.privileged_requests[0].helper.file = "runtime.bin".into();
    package.write_signed_manifest();
    assert_rejected(
        package.verify(),
        "duplicate privileged helper artifact path",
    );
    package.manifest.privileged_requests =
        vec![package.manifest.privileged_requests[0].clone(); MAX_PRIVILEGED_REQUESTS + 1];
    package.write_signed_manifest();
    assert_rejected(package.verify(), "too many privileged requests");
}

#[test]
#[cfg(target_os = "linux")]
fn changing_helper_bytes_invalidates_verified_request() {
    let package = requested_package();
    fs::write(
        package.directory.join("capture-helper"),
        b"replacement helper",
    )
    .unwrap();
    assert_rejected(
        package.verify(),
        "privileged helper artifact integrity failed",
    );
}

#[test]
#[cfg(target_os = "linux")]
fn signer_fingerprint_depends_on_key_bytes_and_not_key_label() {
    let mut package = requested_package();
    let expected = package.verify().unwrap().signer_fingerprint;
    let key = package.bootstrap.trusted_keys["test-key"].clone();
    package.bootstrap.trusted_keys.clear();
    package
        .bootstrap
        .trusted_keys
        .insert("publisher renamed".into(), key);
    let bytes = fs::read(package.directory.join("manifest.json")).unwrap();
    write_json_atomically(
        &package.directory.join("manifest.sig"),
        &ReleaseSignature {
            key_id: "publisher renamed".into(),
            signature: encode_hex(&package.signing_key.sign(&bytes).to_bytes()),
        },
    )
    .unwrap();
    assert_eq!(package.verify().unwrap().signer_fingerprint, expected);
}
