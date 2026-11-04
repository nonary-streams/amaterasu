//! Offline publisher of a signed, opt-in RTX HDR native-set repository.
use crate::profiles::{PackageProfile, linux_session::VIDEO_COMPONENT_ID};
use amaterasu_package::{
    BootstrapConfig, PackageResult, ReleaseManifest, ReleaseSignature, VerificationPolicy,
    current_target, decode_hex, encode_hex, read_json,
    repository::{
        ApprovedRepository, Candidate, CandidateKind, Catalog, CatalogCheckpoint, verify_candidate,
        verify_catalog,
    },
    sha256, unix_timestamp, verify_package, write_json_atomically,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{collections::BTreeSet, fs, path::Path};

const SLOT: &str = "amaterasu.rtx-hdr";
const IMPLEMENTATION: &str = "amaterasu-linux-session-rtx-hdr";
const SERVICE: &str = "amaterasu.rtx-hdr/1";
const CATALOG_DOMAIN: &[u8] = b"amaterasu.repository.catalog.v1\n";
const MAX_SET_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PUBLISHED_LIFETIME: u64 = 366 * 24 * 60 * 60;

/// Produce a new, standalone repository tree. Never changes an existing tree.
#[allow(clippy::too_many_arguments)]
pub fn publish(
    bootstrap_path: &Path,
    package: &Path,
    output: &Path,
    repository: &str,
    location: &str,
    key_path: &Path,
    sequence: u64,
    expires_unix: u64,
) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(bootstrap_path)?;
    let secret: String = read_json(key_path)?;
    let signer = SigningKey::from_bytes(&decode_hex::<32>(&secret)?);
    let public = signer.verifying_key().to_bytes();
    let fingerprint = sha256(&public);
    let approved = ApprovedRepository::approve(
        repository.into(),
        location.into(),
        encode_hex(&public),
        fingerprint,
    )?;
    let now = unix_timestamp();
    if sequence == 0 || expires_unix <= now {
        return Err("catalog sequence and expiry must be positive and current".into());
    }
    let verified = verify_package(
        package,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now,
        },
    )?;
    if !verified.signed {
        return Err("repository source package must be signed".into());
    }
    if expires_unix > now.saturating_add(MAX_PUBLISHED_LIFETIME) {
        return Err("published repository lifetime exceeds one year".into());
    }
    let version = check_composition(&verified.manifest)?;
    let files = checked_payload_files(&verified.manifest)?;
    // The publisher signs the exact verified source composition with its own
    // release lifetime; development builds have a short source validity.
    let mut published_manifest = verified.manifest.clone();
    published_manifest.expires_unix = expires_unix;
    let manifest = serde_json::to_vec(&published_manifest).map_err(|e| e.to_string())?;
    let candidate = Candidate {
        slot: SLOT.into(),
        implementation: IMPLEMENTATION.into(),
        version: version.into(),
        kind: CandidateKind::NativeSet,
        manifest_sha256: sha256(&manifest),
    };
    let catalog = Catalog {
        schema: 1,
        repository: repository.into(),
        sequence,
        expires_unix,
        target: current_target(),
        candidates: vec![candidate.clone()],
    };
    let raw_catalog = serde_json::to_vec(&catalog).map_err(|e| e.to_string())?;
    let mut catalog_message = CATALOG_DOMAIN.to_vec();
    catalog_message.extend_from_slice(&raw_catalog);
    let catalog_signature = encode_hex(&signer.sign(&catalog_message).to_bytes());
    let verified_catalog = verify_catalog(
        &approved,
        &raw_catalog,
        &catalog_signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        now,
    )?;

    // An exclusively created output ensures cleanup cannot remove pre-existing files.
    fs::create_dir(output).map_err(|e| format!("cannot create new repository output: {e}"))?;
    let result = (|| {
        let destination = output.join("native").join(IMPLEMENTATION).join(version);
        fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
        for name in files {
            fs::copy(package.join(name), destination.join(name)).map_err(|e| e.to_string())?;
        }
        fs::write(destination.join("manifest.json"), &manifest).map_err(|e| e.to_string())?;
        write_json_atomically(
            &destination.join("manifest.sig"),
            &ReleaseSignature {
                key_id: approved.fingerprint.clone(),
                signature: encode_hex(&signer.sign(&manifest).to_bytes()),
            },
        )?;
        verify_candidate(
            &approved,
            &verified_catalog,
            &candidate,
            &destination,
            &bootstrap,
            now,
        )?;
        fs::write(output.join("catalog.json"), &raw_catalog).map_err(|e| e.to_string())?;
        fs::write(output.join("catalog.sig"), &catalog_signature).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    result
}

fn check_composition(manifest: &ReleaseManifest) -> PackageResult<&str> {
    let actual: BTreeSet<_> = manifest.components.iter().map(|c| c.id.as_str()).collect();
    let expected: BTreeSet<_> = PackageProfile::LinuxSessionRtxHdr
        .components()
        .iter()
        .map(|c| c.id)
        .collect();
    if actual != expected || actual.len() != manifest.components.len() {
        return Err("source is not the complete RTX HDR Linux session composition".into());
    }
    let plugin = manifest
        .components
        .iter()
        .find(|c| c.id == SLOT)
        .ok_or("RTX HDR plugin is absent")?;
    let video = manifest
        .components
        .iter()
        .find(|c| c.id == VIDEO_COMPONENT_ID)
        .ok_or("Linux video component is absent")?;
    let lifecycle = manifest
        .components
        .iter()
        .find(|c| c.id == "amaterasu.session-lifecycle")
        .ok_or("session lifecycle component is absent")?;
    if !plugin.requested.provide.contains(SERVICE)
        || !plugin.requested.native_provide.contains(SERVICE)
        || !video.requested.native_use.contains(SERVICE)
        || video.requires.get(SLOT) != Some(&plugin.version)
        || !video
            .requested
            .call
            .contains("amaterasu.session-lifecycle/1")
        || video.requires.get(&lifecycle.id) != Some(&lifecycle.version)
    {
        return Err("RTX HDR signed grants or video dependency are missing".into());
    }
    Ok(&plugin.version)
}

fn checked_payload_files(manifest: &ReleaseManifest) -> PackageResult<Vec<&str>> {
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    let entries = manifest
        .components
        .iter()
        .map(|c| (c.file.as_str(), c.size))
        .chain(
            manifest
                .privileged_requests
                .iter()
                .map(|r| (r.helper.file.as_str(), r.helper.size)),
        );
    for (name, size) in entries {
        if name.is_empty()
            || name.len() > 128
            || name == "."
            || name == ".."
            || !name.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
            || matches!(name, "manifest.json" | "manifest.sig")
            || !names.insert(name)
            || size > MAX_FILE_BYTES
        {
            return Err("invalid repository payload name or size".into());
        }
        total = total
            .checked_add(size)
            .ok_or("repository payload size overflow")?;
        if total > MAX_SET_BYTES {
            return Err("repository payload exceeds download limit".into());
        }
    }
    Ok(names.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::{ComponentManifest, random_identifier};
    use std::collections::BTreeMap;

    struct Fixture {
        root: std::path::PathBuf,
        source: std::path::PathBuf,
        bootstrap: std::path::PathBuf,
        key: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("rtx-publisher-{}", random_identifier().unwrap()));
            let source = root.join("source");
            fs::create_dir_all(&source).unwrap();
            let signer = SigningKey::from_bytes(&[7; 32]);
            let bootstrap = root.join("bootstrap.json");
            let key = root.join("key.json");
            write_json_atomically(&key, &encode_hex(&signer.to_bytes())).unwrap();
            let mut policy = amaterasu_contracts::RuntimeConfig::default();
            let mut components = Vec::new();
            for definition in PackageProfile::LinuxSessionRtxHdr.components() {
                let file = format!("lib{}.so", definition.library);
                let bytes = definition.id.as_bytes();
                fs::write(source.join(&file), bytes).unwrap();
                let requested = definition.grants();
                policy
                    .principals
                    .insert(definition.id.into(), requested.clone());
                components.push(ComponentManifest {
                    id: definition.id.into(),
                    version: "0.1.0".into(),
                    file,
                    sha256: sha256(bytes),
                    size: bytes.len() as u64,
                    abi_major: 1,
                    runtime: definition.id == "amaterasu.runtime",
                    requires: definition
                        .dependencies
                        .iter()
                        .map(|id| ((*id).into(), "0.1.0".into()))
                        .collect(),
                    requested,
                });
            }
            let config = BootstrapConfig {
                development: true,
                allow_unsigned: false,
                trusted_keys: BTreeMap::from([(
                    "local".into(),
                    encode_hex(&signer.verifying_key().to_bytes()),
                )]),
                policy,
            };
            write_json_atomically(&bootstrap, &config).unwrap();
            let manifest = ReleaseManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                sequence: 1,
                expires_unix: unix_timestamp() + 3600,
                target: current_target(),
                components,
                privileged_requests: vec![],
            };
            let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
            fs::write(source.join("manifest.json"), &bytes).unwrap();
            write_json_atomically(
                &source.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: "local".into(),
                    signature: encode_hex(&signer.sign(&bytes).to_bytes()),
                },
            )
            .unwrap();
            Self {
                root,
                source,
                bootstrap,
                key,
            }
        }
        fn run(&self, output: &Path) -> PackageResult<()> {
            publish(
                &self.bootstrap,
                &self.source,
                output,
                "publisher",
                "https://example.test",
                &self.key,
                1,
                unix_timestamp() + 1800,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn publisher_creates_verified_catalog_and_repository_layout() {
        let fixture = Fixture::new();
        let output = fixture.root.join("output");
        fixture.run(&output).unwrap();
        let catalog: Catalog = read_json(&output.join("catalog.json")).unwrap();
        assert_eq!(catalog.candidates.len(), 1);
        assert_eq!(catalog.candidates[0].slot, SLOT);
        assert_eq!(catalog.candidates[0].implementation, IMPLEMENTATION);
        let candidate = output.join("native").join(IMPLEMENTATION).join("0.1.0");
        let signature: ReleaseSignature = read_json(&candidate.join("manifest.sig")).unwrap();
        let secret: String = read_json(&fixture.key).unwrap();
        let signer = SigningKey::from_bytes(&decode_hex::<32>(&secret).unwrap());
        assert_eq!(signature.key_id, sha256(&signer.verifying_key().to_bytes()));
        assert!(candidate.join("libamaterasu_rtx_hdr.so").is_file());
        assert!(fixture.run(&output).is_err());
        assert!(output.join("catalog.json").is_file());
    }

    #[test]
    fn publisher_rejects_changed_artifact_without_creating_output() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("libamaterasu_rtx_hdr.so"), b"tampered").unwrap();
        let output = fixture.root.join("output");
        assert!(fixture.run(&output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn publisher_requires_signed_live_settings_authority() {
        let fixture = Fixture::new();
        let manifest: ReleaseManifest = read_json(&fixture.source.join("manifest.json")).unwrap();
        assert!(check_composition(&manifest).is_ok());

        let mut missing_grant = manifest.clone();
        let video = missing_grant
            .components
            .iter_mut()
            .find(|c| c.id == VIDEO_COMPONENT_ID)
            .unwrap();
        video.requested.call.remove("amaterasu.session-lifecycle/1");
        assert!(check_composition(&missing_grant).is_err());

        let mut missing_dependency = manifest;
        let video = missing_dependency
            .components
            .iter_mut()
            .find(|c| c.id == VIDEO_COMPONENT_ID)
            .unwrap();
        video.requires.remove("amaterasu.session-lifecycle");
        assert!(check_composition(&missing_dependency).is_err());
    }

    #[test]
    fn publisher_signs_a_bounded_release_lifetime_for_verified_source() {
        let fixture = Fixture::new();
        let output = fixture.root.join("extended");
        let expiry = unix_timestamp() + 7200;
        publish(
            &fixture.bootstrap,
            &fixture.source,
            &output,
            "publisher",
            "https://example.test",
            &fixture.key,
            1,
            expiry,
        )
        .unwrap();
        let path = output
            .join("native")
            .join(IMPLEMENTATION)
            .join("0.1.0")
            .join("manifest.json");
        let published: ReleaseManifest = read_json(&path).unwrap();
        assert_eq!(published.expires_unix, expiry);
        let too_long = fixture.root.join("too-long");
        assert!(
            publish(
                &fixture.bootstrap,
                &fixture.source,
                &too_long,
                "publisher",
                "https://example.test",
                &fixture.key,
                1,
                unix_timestamp() + MAX_PUBLISHED_LIFETIME + 10
            )
            .is_err()
        );
        assert!(!too_long.exists());
    }
}
