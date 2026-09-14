//! Local-only publisher tooling. Generated keys are not production release identities.
use amaterasu_package::{
    ActivationState, BootstrapConfig, ComponentManifest, PackageResult, ReleaseManifest,
    ReleaseSignature, create_private_directory, current_target, decode_hex, encode_hex,
    read_bounded, read_json, sha256, unix_timestamp, write_json_atomically,
};
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;

use crate::profiles::{ComponentDefinition, PackageProfile, RUNTIME_ID, development_policy};
use std::fs;
use std::path::Path;

const DEVELOPMENT_KEY_ID: &str = "local-development";

pub fn initialize(root: &Path) -> PackageResult<()> {
    if root.join("bootstrap.json").exists() || root.join("development-key.json").exists() {
        return Err(
            "development root is already initialized; refusing to replace its trust identity"
                .into(),
        );
    }
    create_private_directory(root)?;

    let mut secret = [0; 32];
    getrandom::fill(&mut secret).map_err(|error| error.to_string())?;
    let signing_key = SigningKey::from_bytes(&secret);
    let mut bootstrap = BootstrapConfig {
        development: true,
        allow_unsigned: false,
        trusted_keys: BTreeMap::from([(
            DEVELOPMENT_KEY_ID.into(),
            encode_hex(signing_key.verifying_key().as_bytes()),
        )]),
        policy: development_policy(root)?,
    };
    let web_directory = root.join("web");
    create_private_directory(&web_directory)?;
    bootstrap.policy.settings.insert("amaterasu.management".into(), serde_json::json!({
        "enabled": false,
        "web": {"directory": web_directory.canonicalize().map_err(|error| error.to_string())?, "trusted_keys": bootstrap.trusted_keys}
    }));

    write_json_atomically(&root.join("development-key.json"), &encode_hex(&secret))?;
    write_json_atomically(&root.join("bootstrap.json"), &bootstrap)?;
    write_json_atomically(&root.join("selection.json"), &ActivationState::default())
}

pub fn build_package(
    root: &Path,
    artifacts: &Path,
    output: &Path,
    sequence: u64,
    profile: PackageProfile,
) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("development publisher cannot use a production root".into());
    }
    if output.try_exists().map_err(|error| error.to_string())? {
        return Err("package output already exists; use a new directory for each release".into());
    }
    create_private_directory(output)?;

    let mut components = Vec::new();
    for definition in profile.components() {
        let version = if definition.id == "amaterasu.example-echo" {
            format!("0.1.{sequence}")
        } else {
            "0.1.0".to_owned()
        };
        components.push(copy_component(artifacts, output, definition, &version)?);
    }

    let manifest = ReleaseManifest {
        schema: 1,
        project: "amaterasu".into(),
        sequence,
        expires_unix: unix_timestamp() + 7 * 24 * 60 * 60,
        target: current_target(),
        components,
    };
    sign_manifest(root, output, &manifest)
}

fn copy_component(
    artifacts: &Path,
    output: &Path,
    definition: &ComponentDefinition,
    version: &str,
) -> PackageResult<ComponentManifest> {
    let filename = library_filename(definition.library);
    let bytes = read_bounded(&artifacts.join(&filename), 256 * 1024 * 1024)?;
    fs::write(output.join(&filename), &bytes).map_err(|error| error.to_string())?;

    Ok(ComponentManifest {
        id: definition.id.into(),
        version: version.into(),
        file: filename,
        sha256: sha256(&bytes),
        size: bytes.len() as u64,
        abi_major: amaterasu_abi::ABI_MAJOR,
        runtime: definition.id == RUNTIME_ID,
        requires: definition
            .dependencies
            .iter()
            .map(|dependency| ((*dependency).to_owned(), "0.1.0".to_owned()))
            .collect(),
        requested: definition.grants(),
    })
}

pub(super) fn sign_manifest(
    root: &Path,
    output: &Path,
    manifest: &impl serde::Serialize,
) -> PackageResult<()> {
    let secret: String = read_json(&root.join("development-key.json"))?;
    let signing_key = SigningKey::from_bytes(&decode_hex::<32>(&secret)?);
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    let signature = ReleaseSignature {
        key_id: DEVELOPMENT_KEY_ID.into(),
        signature: encode_hex(&signing_key.sign(&bytes).to_bytes()),
    };

    fs::write(output.join("manifest.json"), bytes).map_err(|error| error.to_string())?;
    write_json_atomically(&output.join("manifest.sig"), &signature)
}

fn library_filename(name: &str) -> String {
    format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}
