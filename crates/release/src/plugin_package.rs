//! Offline packaging of an independent plugin and its exact installer payloads.
use amaterasu_package::{
    PackageResult, ReleaseSignature, encode_hex, plugins::PluginManifest, read_json, sha256,
    write_json_atomically,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{fs, path::Path};

pub fn pack(
    template: &Path,
    artifacts: &Path,
    output: &Path,
    signer: &SigningKey,
) -> PackageResult<()> {
    let manifest: PluginManifest = read_json(template)?;
    pack_manifest(&manifest, artifacts, output, signer)
}

pub(crate) fn pack_manifest(
    manifest: &PluginManifest,
    artifacts: &Path,
    output: &Path,
    signer: &SigningKey,
) -> PackageResult<()> {
    manifest.validate(amaterasu_package::unix_timestamp())?;
    let files = manifest.artifacts()?;
    // The template already binds sizes and hashes. Packaging never silently
    // changes a reviewed driver helper or native library's expected identity.
    for artifact in &files {
        amaterasu_package::plugins::verify_artifact(artifacts, artifact)?;
    }
    fs::create_dir(output).map_err(|e| e.to_string())?;
    let result = (|| {
        for artifact in &files {
            fs::copy(artifacts.join(&artifact.file), output.join(&artifact.file))
                .map_err(|e| e.to_string())?;
        }
        let raw = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
        fs::write(output.join("manifest.json"), &raw).map_err(|e| e.to_string())?;
        let public = signer.verifying_key().to_bytes();
        let fingerprint = sha256(&public);
        write_json_atomically(
            &output.join("manifest.sig"),
            &ReleaseSignature {
                key_id: fingerprint.clone(),
                signature: encode_hex(&signer.sign(&raw).to_bytes()),
            },
        )?;
        let bootstrap = amaterasu_package::BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: [(fingerprint, encode_hex(&public))].into(),
            policy: Default::default(),
        };
        amaterasu_package::plugins::verify_package(
            output,
            &bootstrap,
            amaterasu_package::unix_timestamp(),
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(output);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_package::{
        BootstrapConfig, ComponentManifest, current_target, plugins::PluginManifest, unix_timestamp,
    };
    use std::collections::BTreeMap;

    #[test]
    fn independent_plugin_pack_and_repository_publication_verify_original_payloads() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-publish-plugin-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("plugin.bin"), b"payload").unwrap();
        let manifest = PluginManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            id: "example.plugin".into(),
            version: "1.0.0".into(),
            sequence: 2,
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            components: vec![ComponentManifest {
                id: "example.plugin".into(),
                version: "1.0.0".into(),
                file: "plugin.bin".into(),
                sha256: sha256(b"payload"),
                size: 7,
                abi_major: 1,
                runtime: false,
                requires: BTreeMap::new(),
                requested: Grants::default(),
            }],
            drivers: vec![],
            api_methods: vec![],
            settings: BTreeMap::new(),
        };
        let template = root.join("template.json");
        write_json_atomically(&template, &manifest).unwrap();
        let signer = SigningKey::from_bytes(&[29; 32]);
        let public = signer.verifying_key().to_bytes();
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: [(sha256(&public), encode_hex(&public))].into(),
            policy: RuntimeConfig::default(),
        };
        let bootstrap_path = root.join("bootstrap.json");
        write_json_atomically(&bootstrap_path, &bootstrap).unwrap();
        let package = root.join("package");
        pack(&template, &artifacts, &package, &signer).unwrap();
        assert_eq!(
            amaterasu_package::plugins::verify_package(&package, &bootstrap, unix_timestamp())
                .unwrap()
                .id,
            "example.plugin"
        );
        let repository = root.join("repository");
        crate::repository_publisher::publish(
            &bootstrap_path,
            &[crate::repository_publisher::SetSource {
                slot: "example.plugin",
                implementation: "example-plugin",
                package: &package,
            }],
            &repository,
            &crate::repository_publisher::Publication {
                repository: "example",
                location: "https://example.test/plugins",
                sequence: 3,
                expires_unix: unix_timestamp() + 7200,
                versions: &BTreeMap::new(),
            },
            &signer,
        )
        .unwrap();
        let published = repository.join("plugins/example-plugin/1.0.0");
        let verified =
            amaterasu_package::plugins::verify_package(&published, &bootstrap, unix_timestamp())
                .unwrap();
        assert_eq!(verified.sequence, 2);
        let catalog: serde_json::Value = read_json(&repository.join("catalog.json")).unwrap();
        assert_eq!(catalog["candidates"][0]["kind"], "native_plugin");
        assert!(pack(&template, &artifacts, &package, &signer).is_err());
        assert_eq!(fs::read(package.join("plugin.bin")).unwrap(), b"payload");
        fs::write(artifacts.join("plugin.bin"), b"changed").unwrap();
        let refused = root.join("refused");
        assert!(pack(&template, &artifacts, &refused, &signer).is_err());
        assert!(!refused.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
