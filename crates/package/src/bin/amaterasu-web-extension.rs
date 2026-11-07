//! Local publisher and administrator utility for web extensions.
use amaterasu_package::{
    ReleaseSignature, decode_hex, encode_hex, sha256, unix_timestamp, web::extension,
    write_json_atomically,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{collections::BTreeMap, path::Path};

fn main() {
    if let Err(error) = run() {
        eprintln!("amaterasu-web-extension: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pack") if args.len() == 9 || args.len() == 10 => {
            let sequence = args[6].parse().map_err(|_| "invalid sequence")?;
            let expiry = args[7].parse().map_err(|_| "invalid expiry")?;
            let secret = std::fs::read_to_string(&args[8]).map_err(|e| e.to_string())?;
            let signer = SigningKey::from_bytes(&decode_hex::<32>(secret.trim())?);
            let mut manifest = extension::prepare_extension(Path::new(&args[1]), Path::new(&args[2]), &args[3], &args[4], &args[5], sequence, expiry)?;
            if let Some(methods_path) = args.get(9) {
                let bytes = amaterasu_package::read_bounded(Path::new(methods_path), 65_536)?;
                apply_ui_configuration(&mut manifest, &bytes)?;
            }
            extension::validate_manifest(&manifest, unix_timestamp())?;
            let raw = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
            amaterasu_storage::write_atomically(&Path::new(&args[2]).join("manifest.json"), &raw)?;
            let fingerprint = sha256(signer.verifying_key().as_bytes());
            let signature = ReleaseSignature { key_id: fingerprint.clone(), signature: encode_hex(&signer.sign(&raw).to_bytes()) };
            write_json_atomically(&Path::new(&args[2]).join("manifest.sig"), &signature)?;
            println!("publisher fingerprint: {fingerprint}\npublisher public key: {}", encode_hex(signer.verifying_key().as_bytes()));
            Ok(())
        }
        Some("stage") if args.len() == 5 || args.len() == 7 => {
            let fingerprint = &args[3];
            let mut keys = BTreeMap::from([(fingerprint.clone(), args[4].clone())]);
            if args.len() == 7 {
                keys.insert(args[5].clone(), args[6].clone());
            }
            let digest = extension::stage_extension(Path::new(&args[1]), Path::new(&args[2]), &keys, fingerprint, unix_timestamp())?;
            println!("selected digest: {digest}");
            Ok(())
        }
        _ => Err("usage: amaterasu-web-extension pack SOURCE OUTPUT ID NAME ENTRYPOINT SEQUENCE EXPIRES_UNIX SECRET_HEX_FILE [METHODS_OR_UI_JSON] | stage BUNDLE DEDICATED_ROOT APPROVED_FINGERPRINT APPROVED_PUBLIC_KEY_HEX [PREVIOUS_FINGERPRINT PREVIOUS_PUBLIC_KEY_HEX]".into()),
    }
}

fn apply_ui_configuration(manifest: &mut extension::Manifest, bytes: &[u8]) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UiConfiguration {
        #[serde(default)]
        methods: Vec<extension::MethodDeclaration>,
        placements: Vec<String>,
        #[serde(default)]
        settings_provider: Option<String>,
        #[serde(default)]
        settings_owner: Option<String>,
        #[serde(default)]
        default_expanded: bool,
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value.is_array() {
        manifest.methods = serde_json::from_value(value).map_err(|e| e.to_string())?;
    } else {
        let config: UiConfiguration = serde_json::from_value(value).map_err(|e| e.to_string())?;
        manifest.methods = config.methods;
        manifest.placements = config.placements;
        manifest.settings_provider = config.settings_provider;
        manifest.settings_owner = config.settings_owner;
        manifest.default_expanded = config.default_expanded;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaging_preserves_expansion_hint_and_legacy_defaults() {
        let mut manifest: extension::Manifest = serde_json::from_value(serde_json::json!({
            "schema":1,"project":"amaterasu.web.extension","id":"example","name":"Example",
            "sequence":1,"expires_unix":100,"sdk_version":1,"entrypoint":"main.js",
            "files":{"main.js":{"sha256":"a".repeat(64),"size":1}}
        }))
        .unwrap();
        apply_ui_configuration(
            &mut manifest,
            br#"{"placements":["application"],"default_expanded":true}"#,
        )
        .unwrap();
        assert!(manifest.default_expanded);
        apply_ui_configuration(&mut manifest, br#"{"placements":["application"]}"#).unwrap();
        assert!(!manifest.default_expanded);
        assert!(
            apply_ui_configuration(
                &mut manifest,
                br#"{"placements":["application"],"default_expanded":"true"}"#,
            )
            .is_err()
        );
        apply_ui_configuration(&mut manifest, b"[]").unwrap();
        assert!(!manifest.default_expanded);
    }
}
