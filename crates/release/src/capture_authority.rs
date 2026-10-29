//! Explicit administrative deployment of verified capture authority requests.
//!
//! Review is read-only. Approval copies a verified snapshot into a protected
//! installation and only then records the decision. It never starts a service.
use amaterasu_capture_authority::{ApprovalRecord, ApprovalStore, approval_id};
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, random_identifier,
    read_bounded, read_json, sha256, unix_timestamp, verify_package,
};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize)]
struct Review<'a> {
    status: &'static str,
    approval_id: String,
    authority: &'a ApprovalRecord,
    access: &'static str,
    execution_boundary: &'static str,
}

struct Candidate {
    bootstrap: BootstrapConfig,
    records: Vec<ApprovalRecord>,
}

pub fn review(
    bootstrap: &Path,
    package: &Path,
    worker: &Path,
    uid: &str,
    gid: &str,
) -> PackageResult<()> {
    let candidate = candidate(bootstrap, package, worker, uid, gid)?;
    let reviews = candidate.records.iter().map(|record| {
        Ok(Review {
            status: "requested; not approved or deployed",
            approval_id: approval_id(record)?,
            authority: record,
            access: "Export frames from the approved vibeshine_drm card, including its display contents. The exact approved helper is trusted native code executed as root; the protocol exposes no arbitrary command request and does not sandbox the helper's native code.",
            execution_boundary: "The protected broker starts an unprivileged trusted native worker. Native modules in that worker are not isolated from one another.",
        })
    }).collect::<PackageResult<Vec<_>>>()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&reviews).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn approve(
    bootstrap: &Path,
    package: &Path,
    worker: &Path,
    uid: &str,
    gid: &str,
    installation: &Path,
    confirmed: &str,
) -> PackageResult<()> {
    require_root()?;
    let candidate = candidate(bootstrap, package, worker, uid, gid)?;
    let record = candidate
        .records
        .iter()
        .find(|record| approval_id(record).is_ok_and(|id| id == confirmed))
        .ok_or("confirmed approval ID does not match a verified request and worker identity")?;
    let installation = protected_directory(installation)?;
    let packages = protected_directory(&installation.join("packages"))?;
    let final_root = packages.join(confirmed);
    if final_root.try_exists().map_err(|error| error.to_string())? {
        return Err(
            "this deployment already exists; revoke before installing a different identity".into(),
        );
    }
    let temporary = packages.join(format!(".prepare-{}", random_identifier()?));
    protected_directory(&temporary)?;
    let mut deployed = false;
    let mut deployment_bootstrap = candidate.bootstrap.clone();
    // This explicit administrative deployment turns the local publisher policy
    // into a signed-only installed policy; it never changes the source root.
    deployment_bootstrap.development = false;
    deployment_bootstrap.allow_unsigned = false;
    let result = (|| {
        write_snapshot(&deployment_bootstrap, package, worker, record, &temporary)?;
        // The installed bytes are checked with the copied trust data, rather
        // than relying on the earlier verification of mutable source files.
        let installed_package = temporary.join("sets").join(&record.request.manifest_digest);
        let verified = verify_package(
            &installed_package,
            &deployment_bootstrap,
            verification_policy(),
        )?;
        if !verified
            .privileged_requests
            .iter()
            .any(|request| request == &record.request)
        {
            return Err("installed package no longer matches the reviewed authority".into());
        }
        if sha256(&read_bounded(
            &temporary.join("amaterasu"),
            MAX_ARTIFACT_BYTES,
        )?) != record.worker_sha256
        {
            return Err("installed worker no longer matches the reviewed identity".into());
        }
        fs::rename(&temporary, &final_root).map_err(|error| error.to_string())?;
        deployed = true;
        let store = ApprovalStore::create(&installation.join("approvals"))?;
        if let Err(error) = store.put(record) {
            // No process can receive authority without the protected decision.
            let _ = fs::remove_dir_all(&final_root);
            return Err(error);
        }
        println!("Approved and deployed {confirmed}. No process was started.");
        println!("Package root: {}", final_root.display());
        println!(
            "Approval store: {}",
            installation.join("approvals").display()
        );
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    if result.is_err() && deployed {
        let _ = fs::remove_dir_all(&final_root);
    }
    result
}

pub fn revoke(installation: &Path, id: &str) -> PackageResult<()> {
    require_root()?;
    let installation = validate_existing_protected_path(installation)?;
    let store = ApprovalStore::open(&installation.join("approvals"))?;
    store.remove(id)?;
    println!(
        "Revoked {id}. The broker refuses further operations and terminates affected sessions; the deployment snapshot is retained."
    );
    Ok(())
}

fn candidate(
    bootstrap: &Path,
    package: &Path,
    worker: &Path,
    uid: &str,
    gid: &str,
) -> PackageResult<Candidate> {
    let uid = parse_identity(uid)?;
    let gid = parse_identity(gid)?;
    let bootstrap: BootstrapConfig = read_json(bootstrap)?;
    let verified = verify_package(package, &bootstrap, verification_policy())?;
    if !verified.signed || verified.privileged_requests.is_empty() {
        return Err("package has no verified signed privileged requests".into());
    }
    let worker_bytes = read_regular(worker, MAX_ARTIFACT_BYTES)?;
    if worker_bytes.is_empty() {
        return Err("worker executable is empty".into());
    }
    let worker_sha256 = sha256(&worker_bytes);
    let records = verified
        .privileged_requests
        .into_iter()
        .map(|request| ApprovalRecord {
            schema: 1,
            request,
            worker_sha256: worker_sha256.clone(),
            uid,
            gid,
        })
        .collect();
    Ok(Candidate { bootstrap, records })
}

fn verification_policy() -> VerificationPolicy {
    VerificationPolicy {
        minimum_sequence: 0,
        allow_historical: true,
        now: unix_timestamp(),
    }
}

fn parse_identity(value: &str) -> PackageResult<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(
            "worker UID and GID must be numeric identities from 1 through 4294967294".into(),
        );
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value != 0 && *value != u32::MAX)
        .ok_or_else(|| {
            "worker UID and GID must be numeric identities from 1 through 4294967294".into()
        })
}

fn write_snapshot(
    bootstrap: &BootstrapConfig,
    source: &Path,
    worker: &Path,
    record: &ApprovalRecord,
    destination: &Path,
) -> PackageResult<()> {
    let sets = protected_directory(&destination.join("sets"))?;
    let package = protected_directory(&sets.join(&record.request.manifest_digest))?;
    let bootstrap_bytes =
        serde_json::to_vec_pretty(bootstrap).map_err(|error| error.to_string())?;
    write_new(&destination.join("bootstrap.json"), &bootstrap_bytes, 0o444)?;
    let manifest_bytes = read_regular(&source.join("manifest.json"), 1024 * 1024)?;
    if sha256(&manifest_bytes) != record.request.manifest_digest {
        return Err("source manifest changed after review".into());
    }
    let verified = verify_package(source, bootstrap, verification_policy())?;
    let selection = ActivationState {
        active: Some(record.request.manifest_digest.clone()),
        highest_sequence: verified.manifest.sequence,
        ..ActivationState::default()
    };
    write_new(
        &destination.join("selection.json"),
        &serde_json::to_vec_pretty(&selection).map_err(|error| error.to_string())?,
        0o444,
    )?;
    if !verified
        .privileged_requests
        .iter()
        .any(|request| request == &record.request)
    {
        return Err("source package changed after review".into());
    }
    write_new(&package.join("manifest.json"), &manifest_bytes, 0o444)?;
    write_new(
        &package.join("manifest.sig"),
        &read_regular(&source.join("manifest.sig"), 1024 * 1024)?,
        0o444,
    )?;
    for component in &verified.manifest.components {
        copy_verified(
            source,
            &package,
            &component.file,
            &component.sha256,
            component.size,
            0o444,
        )?;
    }
    for request in &verified.manifest.privileged_requests {
        copy_verified(
            source,
            &package,
            &request.helper.file,
            &request.helper.sha256,
            request.helper.size,
            0o544,
        )?;
    }
    let worker_bytes = read_regular(worker, MAX_ARTIFACT_BYTES)?;
    if sha256(&worker_bytes) != record.worker_sha256 {
        return Err("worker executable changed after review".into());
    }
    write_new(&destination.join("amaterasu"), &worker_bytes, 0o555)
}

fn copy_verified(
    source: &Path,
    destination: &Path,
    file: &str,
    digest: &str,
    size: u64,
    mode: u32,
) -> PackageResult<()> {
    let bytes = read_regular(&source.join(file), MAX_ARTIFACT_BYTES)?;
    if bytes.len() as u64 != size || sha256(&bytes) != digest {
        return Err(format!("source artifact changed after review: {file}"));
    }
    write_new(&destination.join(file), &bytes, mode)
}

fn read_regular(path: &Path, maximum: u64) -> PackageResult<Vec<u8>> {
    if !fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("source is not a regular file: {}", path.display()));
    }
    read_bounded(path, maximum)
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> PackageResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn require_root() -> PackageResult<()> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid only reads the current process's effective UID.
    if unsafe { geteuid() } != 0 {
        return Err(
            "approval deployment and revocation require explicit OS administrator authorization"
                .into(),
        );
    }
    Ok(())
}

fn validate_install_path(path: &Path) -> PackageResult<()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("installation must name a non-root absolute normal path".into());
    }
    Ok(())
}

fn validate_existing_protected_path(path: &Path) -> PackageResult<PathBuf> {
    validate_install_path(path)?;
    let mut current = PathBuf::from("/");
    for part in path.components().skip(1) {
        current.push(part.as_os_str());
        validate_directory(&current)?;
    }
    Ok(path.to_path_buf())
}

fn protected_directory(path: &Path) -> PackageResult<PathBuf> {
    validate_install_path(path)?;
    let mut current = PathBuf::from("/");
    for part in path.components().skip(1) {
        current.push(part.as_os_str());
        if !current.try_exists().map_err(|error| error.to_string())? {
            fs::create_dir(&current).map_err(|error| error.to_string())?;
            fs::set_permissions(&current, fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
        }
        validate_directory(&current)?;
    }
    Ok(path.to_path_buf())
}

fn validate_directory(path: &Path) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "installation ancestor must be a root-owned directory without group or other write permission: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        bootstrap: PathBuf,
        package: PathBuf,
        worker: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            use amaterasu_contracts::{Grants, RuntimeConfig};
            use amaterasu_package::{
                ComponentManifest, ReleaseManifest, ReleaseSignature, current_target, encode_hex,
            };
            use ed25519_dalek::{Signer, SigningKey};
            use std::collections::BTreeMap;
            let root = std::env::temp_dir().join(format!(
                "amaterasu-authority-review-{}",
                random_identifier().unwrap()
            ));
            let package = root.join("package");
            fs::create_dir_all(&package).unwrap();
            let worker = root.join("host");
            fs::write(&worker, b"reviewed host fixture").unwrap();
            let helper = root.join("helper-source");
            fs::write(&helper, b"helper fixture").unwrap();
            let request = crate::capture_authority_package::prepare(
                &crate::capture_authority_package::PackageSpec {
                    schema: 1,
                    helper_source: helper,
                    drm_card: 2,
                },
                &package,
            )
            .unwrap();
            let component = |id: &str, runtime, file: &str| {
                let bytes = format!("fixture bytes for {id}");
                fs::write(package.join(file), &bytes).unwrap();
                ComponentManifest {
                    id: id.into(),
                    version: "0.1.0".into(),
                    file: file.into(),
                    sha256: sha256(bytes.as_bytes()),
                    size: bytes.len() as u64,
                    abi_major: amaterasu_abi::ABI_MAJOR,
                    runtime,
                    requires: BTreeMap::new(),
                    requested: Grants::default(),
                }
            };
            let manifest = ReleaseManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                sequence: 1,
                expires_unix: unix_timestamp() + 3600,
                target: current_target(),
                components: vec![
                    component("amaterasu.runtime", true, "runtime.so"),
                    component("amaterasu.capture-vkms", false, "capture.so"),
                ],
                privileged_requests: vec![request],
            };
            let key = SigningKey::from_bytes(&[61; 32]);
            let bytes = serde_json::to_vec(&manifest).unwrap();
            fs::write(package.join("manifest.json"), &bytes).unwrap();
            let signature = ReleaseSignature {
                key_id: "test".into(),
                signature: encode_hex(&key.sign(&bytes).to_bytes()),
            };
            fs::write(
                package.join("manifest.sig"),
                serde_json::to_vec(&signature).unwrap(),
            )
            .unwrap();
            let bootstrap = root.join("bootstrap.json");
            let config = BootstrapConfig {
                development: true,
                allow_unsigned: false,
                trusted_keys: BTreeMap::from([(
                    "test".into(),
                    encode_hex(key.verifying_key().as_bytes()),
                )]),
                policy: RuntimeConfig::default(),
            };
            fs::write(&bootstrap, serde_json::to_vec(&config).unwrap()).unwrap();
            Self {
                root,
                bootstrap,
                package,
                worker,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn review_uses_verified_authority_and_changes_when_worker_changes() {
        let fixture = Fixture::new();
        let reviewed = candidate(
            &fixture.bootstrap,
            &fixture.package,
            &fixture.worker,
            "1000",
            "1000",
        )
        .unwrap();
        let request = &reviewed.records[0].request;
        assert_eq!(request.component, "amaterasu.capture-vkms");
        assert_eq!(request.resource.drm_card, 2);
        assert_eq!(
            request.manifest_digest,
            sha256(&fs::read(fixture.package.join("manifest.json")).unwrap())
        );
        let first = approval_id(&reviewed.records[0]).unwrap();
        fs::write(&fixture.worker, b"replacement host fixture").unwrap();
        let replaced = candidate(
            &fixture.bootstrap,
            &fixture.package,
            &fixture.worker,
            "1000",
            "1000",
        )
        .unwrap();
        assert_ne!(first, approval_id(&replaced.records[0]).unwrap());
    }

    #[test]
    fn review_refuses_unsigned_or_replaced_helper_artifacts() {
        let fixture = Fixture::new();
        fs::write(
            fixture.package.join("amaterasu-vkms-frame-helper"),
            b"replaced helper",
        )
        .unwrap();
        assert!(
            candidate(
                &fixture.bootstrap,
                &fixture.package,
                &fixture.worker,
                "1000",
                "1000"
            )
            .is_err()
        );
        let fixture = Fixture::new();
        fs::remove_file(fixture.package.join("manifest.sig")).unwrap();
        assert!(
            candidate(
                &fixture.bootstrap,
                &fixture.package,
                &fixture.worker,
                "1000",
                "1000"
            )
            .is_err()
        );
    }

    #[test]
    fn identities_cannot_select_root_or_non_numeric_accounts() {
        for value in ["0", "", "-1", "root", " 1000", "4294967295", "4294967296"] {
            assert!(parse_identity(value).is_err(), "{value:?}");
        }
        assert_eq!(parse_identity("1000").unwrap(), 1000);
    }

    #[test]
    fn deployment_rejects_relative_root_and_parent_components() {
        for value in ["/", "relative", "/var/../tmp/authority", ""] {
            assert!(
                validate_install_path(Path::new(value)).is_err(),
                "{value:?}"
            );
        }
        assert!(validate_install_path(Path::new("/var/lib/amaterasu/authority")).is_ok());
    }

    #[test]
    fn private_user_directory_is_not_a_protected_installation() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-authority-protection-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_directory(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
