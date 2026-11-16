//! Offline publisher of a signed repository: one catalog and the native sets it lists.
//!
//! The source of each set is a package this checkout packed and signed with
//! its development key. Publishing verifies that package, gives each component
//! its released version, signs the resulting manifest with the publisher key
//! and lists it in a catalog signed with the same key. Nothing is uploaded.
use crate::rtx_repository_publisher::checked_payload_files;
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
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const CATALOG_DOMAIN: &[u8] = b"amaterasu.repository.catalog.v1\n";
const MAX_PUBLISHED_LIFETIME: u64 = 366 * 24 * 60 * 60;
const MAX_SETS: usize = 16;
/// The publisher key never appears on a command line or in a file argument.
pub const PUBLISHER_KEY_VARIABLE: &str = "AMATERASU_PUBLISHER_KEY";

/// One packed native set and where the catalog files it.
pub struct SetSource<'a> {
    /// A component of the set; the catalog lists the set at this component's version.
    pub slot: &'a str,
    pub implementation: &'a str,
    pub package: &'a Path,
}

pub struct Publication<'a> {
    pub repository: &'a str,
    pub location: &'a str,
    pub sequence: u64,
    pub expires_unix: u64,
    /// Released versions by component identity. A component it does not name
    /// keeps the version its package was packed with.
    pub versions: &'a BTreeMap<String, String>,
}

pub fn publisher_key_from_environment() -> PackageResult<SigningKey> {
    let encoded = std::env::var(PUBLISHER_KEY_VARIABLE)
        .map_err(|_| format!("{PUBLISHER_KEY_VARIABLE} is required to publish a repository"))?;
    Ok(SigningKey::from_bytes(&decode_hex::<32>(encoded.trim())?))
}

/// Produce a new, standalone repository tree. Never changes an existing tree.
pub fn publish(
    bootstrap_path: &Path,
    sets: &[SetSource<'_>],
    output: &Path,
    publication: &Publication<'_>,
    signer: &SigningKey,
) -> PackageResult<()> {
    if sets.is_empty() || sets.len() > MAX_SETS {
        return Err("a repository publishes between one and sixteen native sets".into());
    }
    let bootstrap: BootstrapConfig = read_json(bootstrap_path)?;
    let public = signer.verifying_key().to_bytes();
    let approved = ApprovedRepository::approve(
        publication.repository.into(),
        publication.location.into(),
        encode_hex(&public),
        sha256(&public),
    )?;
    let now = unix_timestamp();
    if publication.sequence == 0 || publication.expires_unix <= now {
        return Err("catalog sequence and expiry must be positive and current".into());
    }
    if publication.expires_unix > now.saturating_add(MAX_PUBLISHED_LIFETIME) {
        return Err("published repository lifetime exceeds one year".into());
    }

    let mut prepared = Vec::with_capacity(sets.len());
    let mut destinations = BTreeSet::new();
    for set in sets {
        let entry = prepare_set(&bootstrap, set, publication, now)?;
        if !destinations.insert((set.implementation, entry.candidate.version.clone())) {
            return Err("two sets publish the same implementation and version".into());
        }
        prepared.push(entry);
    }
    let catalog = Catalog {
        schema: 1,
        repository: publication.repository.into(),
        sequence: publication.sequence,
        expires_unix: publication.expires_unix,
        target: current_target(),
        candidates: prepared
            .iter()
            .map(|entry| entry.candidate.clone())
            .collect(),
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
        for (set, entry) in sets.iter().zip(&prepared) {
            let destination = output
                .join(if entry.candidate.kind == CandidateKind::NativePlugin {
                    "plugins"
                } else {
                    "native"
                })
                .join(set.implementation)
                .join(&entry.candidate.version);
            fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            for name in &entry.files {
                fs::copy(set.package.join(name), destination.join(name))
                    .map_err(|e| e.to_string())?;
            }
            fs::write(destination.join("manifest.json"), &entry.manifest)
                .map_err(|e| e.to_string())?;
            write_json_atomically(
                &destination.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: approved.fingerprint.clone(),
                    signature: encode_hex(&signer.sign(&entry.manifest).to_bytes()),
                },
            )?;
            // Check the tree exactly as a host that approved this publisher will.
            verify_candidate(
                &approved,
                &verified_catalog,
                &entry.candidate,
                &destination,
                &bootstrap,
                now,
            )?;
        }
        fs::write(output.join("catalog.json"), &raw_catalog).map_err(|e| e.to_string())?;
        fs::write(output.join("catalog.sig"), &catalog_signature).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    result
}

struct PreparedSet {
    candidate: Candidate,
    manifest: Vec<u8>,
    files: Vec<String>,
}

fn prepare_set(
    bootstrap: &BootstrapConfig,
    set: &SetSource<'_>,
    publication: &Publication<'_>,
    now: u64,
) -> PackageResult<PreparedSet> {
    let raw = amaterasu_package::read_bounded(&set.package.join("manifest.json"), 1024 * 1024)?;
    if serde_json::from_slice::<amaterasu_package::plugins::PluginManifest>(&raw).is_ok() {
        let mut manifest = amaterasu_package::plugins::verify_package(set.package, bootstrap, now)?;
        if manifest.id != set.slot {
            return Err("plugin publication slot does not match its signed identity".into());
        }
        if publication.versions.contains_key(&manifest.id) {
            return Err("pack plugins at their released version before publication".into());
        }
        manifest.expires_unix = publication.expires_unix;
        let files = manifest
            .artifacts()?
            .into_iter()
            .map(|artifact| artifact.file)
            .collect();
        let raw = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
        return Ok(PreparedSet {
            candidate: Candidate {
                slot: manifest.id,
                implementation: set.implementation.into(),
                version: manifest.version,
                kind: CandidateKind::NativePlugin,
                manifest_sha256: sha256(&raw),
            },
            manifest: raw,
            files,
        });
    }
    let verified = verify_package(
        set.package,
        bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now,
        },
    )?;
    if !verified.signed {
        return Err("repository source package must be signed".into());
    }
    let files = checked_payload_files(&verified.manifest)?
        .into_iter()
        .map(str::to_owned)
        .collect();
    // The publisher signs the verified source composition under its own
    // release lifetime; a development package is only valid for days.
    let mut manifest = verified.manifest.clone();
    manifest.expires_unix = publication.expires_unix;
    apply_versions(&mut manifest, publication.versions)?;
    let version = manifest
        .components
        .iter()
        .find(|component| component.id == set.slot)
        .map(|component| component.version.clone())
        .ok_or_else(|| format!("{} is not a component of its set", set.slot))?;
    let raw = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    Ok(PreparedSet {
        candidate: Candidate {
            slot: set.slot.into(),
            implementation: set.implementation.into(),
            version,
            kind: CandidateKind::NativeSet,
            manifest_sha256: sha256(&raw),
        },
        manifest: raw,
        files,
    })
}

/// Give each named component its released version and keep every dependency
/// naming the exact version of the component it selects.
fn apply_versions(
    manifest: &mut ReleaseManifest,
    versions: &BTreeMap<String, String>,
) -> PackageResult<()> {
    for component in &mut manifest.components {
        if let Some(version) = versions.get(&component.id) {
            if !released_version(version) {
                return Err(format!(
                    "{} has an unpublishable version {version:?}",
                    component.id
                ));
            }
            component.version.clone_from(version);
        }
    }
    let selected: BTreeMap<String, String> = manifest
        .components
        .iter()
        .map(|component| (component.id.clone(), component.version.clone()))
        .collect();
    for component in &mut manifest.components {
        for (dependency, version) in &mut component.requires {
            let current = selected
                .get(dependency)
                .ok_or_else(|| format!("{} requires absent {dependency}", component.id))?;
            version.clone_from(current);
        }
    }
    Ok(())
}

/// A version is also a repository path segment.
fn released_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 128
        && version != "."
        && version != ".."
        && version.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::PackageProfile;
    use amaterasu_package::{ComponentManifest, random_identifier};

    const SLOT: &str = "amaterasu.runtime";
    const IMPLEMENTATION: &str = "amaterasu-linux-session";

    struct Fixture {
        root: std::path::PathBuf,
        source: std::path::PathBuf,
        bootstrap: std::path::PathBuf,
        versions: BTreeMap<String, String>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "repository-publisher-{}",
                random_identifier().unwrap()
            ));
            let source = root.join("source");
            fs::create_dir_all(&source).unwrap();
            let packer = SigningKey::from_bytes(&[7; 32]);
            let mut policy = amaterasu_contracts::RuntimeConfig::default();
            let mut components = Vec::new();
            for definition in PackageProfile::LinuxSession.components() {
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
                    runtime: definition.id == SLOT,
                    requires: definition
                        .dependencies
                        .iter()
                        .map(|id| ((*id).into(), "0.1.0".into()))
                        .collect(),
                    requested,
                });
            }
            let bootstrap = root.join("bootstrap.json");
            write_json_atomically(
                &bootstrap,
                &BootstrapConfig {
                    development: true,
                    allow_unsigned: false,
                    trusted_keys: BTreeMap::from([(
                        "local".into(),
                        encode_hex(&packer.verifying_key().to_bytes()),
                    )]),
                    policy,
                },
            )
            .unwrap();
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
                    signature: encode_hex(&packer.sign(&bytes).to_bytes()),
                },
            )
            .unwrap();
            Self {
                root,
                source,
                bootstrap,
                versions: BTreeMap::new(),
            }
        }

        fn publisher() -> SigningKey {
            SigningKey::from_bytes(&[9; 32])
        }

        fn run(&self, output: &Path) -> PackageResult<()> {
            publish(
                &self.bootstrap,
                &[SetSource {
                    slot: SLOT,
                    implementation: IMPLEMENTATION,
                    package: &self.source,
                }],
                output,
                &Publication {
                    repository: "amaterasu",
                    location: "https://example.test/linux-x86_64",
                    sequence: 5,
                    expires_unix: unix_timestamp() + 1800,
                    versions: &self.versions,
                },
                &Self::publisher(),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_published_set_is_signed_by_the_publisher_and_listed_at_its_slot_version() {
        let mut fixture = Fixture::new();
        fixture.versions = BTreeMap::from([
            (SLOT.to_owned(), "0.2.0".to_owned()),
            ("amaterasu.input".to_owned(), "0.3.1".to_owned()),
        ]);
        let output = fixture.root.join("output");
        fixture.run(&output).unwrap();

        let catalog: Catalog = read_json(&output.join("catalog.json")).unwrap();
        assert_eq!(catalog.repository, "amaterasu");
        assert_eq!(catalog.sequence, 5);
        assert_eq!(catalog.candidates.len(), 1);
        let listed = &catalog.candidates[0];
        assert_eq!(
            (
                listed.slot.as_str(),
                listed.implementation.as_str(),
                listed.version.as_str()
            ),
            (SLOT, IMPLEMENTATION, "0.2.0")
        );

        let directory = output.join("native").join(IMPLEMENTATION).join("0.2.0");
        let raw = fs::read(directory.join("manifest.json")).unwrap();
        assert_eq!(sha256(&raw), listed.manifest_sha256);
        let signature: ReleaseSignature = read_json(&directory.join("manifest.sig")).unwrap();
        let public = Fixture::publisher().verifying_key().to_bytes();
        assert_eq!(signature.key_id, sha256(&public));

        let manifest: ReleaseManifest = serde_json::from_slice(&raw).unwrap();
        let version = |id: &str| {
            manifest
                .components
                .iter()
                .find(|component| component.id == id)
                .map(|component| component.version.as_str())
        };
        assert_eq!(version(SLOT), Some("0.2.0"));
        assert_eq!(version("amaterasu.input"), Some("0.3.1"));
        assert_eq!(version("amaterasu.state"), Some("0.1.0"));
        // Every dependency follows the component it names to its new version.
        for component in &manifest.components {
            for (dependency, required) in &component.requires {
                assert_eq!(version(dependency), Some(required.as_str()));
            }
        }
        assert!(directory.join("libamaterasu_input.so").is_file());

        // A second run cannot replace what was published.
        assert!(fixture.run(&output).is_err());
        assert!(output.join("catalog.json").is_file());
    }

    #[test]
    fn a_changed_artifact_or_an_unlisted_slot_publishes_nothing() {
        let fixture = Fixture::new();
        let output = fixture.root.join("output");
        let absent = publish(
            &fixture.bootstrap,
            &[SetSource {
                slot: "amaterasu.absent",
                implementation: IMPLEMENTATION,
                package: &fixture.source,
            }],
            &output,
            &Publication {
                repository: "amaterasu",
                location: "https://example.test/linux-x86_64",
                sequence: 5,
                expires_unix: unix_timestamp() + 1800,
                versions: &fixture.versions,
            },
            &Fixture::publisher(),
        );
        assert!(absent.is_err());
        assert!(!output.exists());

        fs::write(fixture.source.join("libamaterasu_input.so"), b"tampered").unwrap();
        assert!(fixture.run(&output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn a_version_that_cannot_be_a_repository_path_is_refused() {
        let mut fixture = Fixture::new();
        fixture.versions = BTreeMap::from([(SLOT.to_owned(), "../0.2.0".to_owned())]);
        let output = fixture.root.join("output");
        assert!(fixture.run(&output).is_err());
        assert!(!output.exists());
    }
}
