//! Independently signed and selected web extensions. Each extension has its own
//! release floor and cannot replace the base web bundle.
use super::*;
use crate::{RootLock, create_private_directory, random_identifier, write_json_atomically};
use std::fs;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodDeclaration {
    pub alias: String,
    pub provider: String,
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub scope: String,
    pub min_role: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub project: String,
    pub id: String,
    pub name: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub sdk_version: u32,
    pub entrypoint: String,
    /// Signed UI locations. Existing extensions remain standalone pages.
    #[serde(default = "default_placements")]
    pub placements: Vec<String>,
    /// Associates an application UI with its provider's existing settings.
    /// This is presentation metadata, never an API grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_provider: Option<String>,
    #[serde(default)]
    pub methods: Vec<MethodDeclaration>,
    pub files: BTreeMap<String, Asset>,
}

fn default_placements() -> Vec<String> {
    vec!["page".into()]
}

pub struct VerifiedExtension {
    pub digest: String,
    pub manifest: Manifest,
    pub assets: BTreeMap<String, Vec<u8>>,
}

fn validate_id(id: &str) -> PackageResult<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || id.starts_with('-')
        || id.ends_with('-')
    {
        return Err("invalid web extension id".into());
    }
    Ok(())
}

pub fn validate_manifest(manifest: &Manifest, now: u64) -> PackageResult<()> {
    validate_id(&manifest.id)?;
    if manifest.schema != 1
        || manifest.project != "amaterasu.web.extension"
        || manifest.expires_unix <= now
        || manifest.sdk_version != 1
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 128
    {
        return Err("invalid, incompatible or expired extension metadata".into());
    }
    if manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
        || !manifest.entrypoint.ends_with(".js")
        || !manifest.files.contains_key(&manifest.entrypoint)
    {
        return Err("extension requires a JavaScript entrypoint and bounded assets".into());
    }
    if manifest.methods.len() > 64 {
        return Err("too many extension method declarations".into());
    }
    if manifest.placements.is_empty()
        || manifest.placements.len() > 4
        || manifest.placements.iter().any(|place| {
            !matches!(
                place.as_str(),
                "page" | "application" | "overview" | "settings"
            )
        })
        || manifest
            .placements
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != manifest.placements.len()
    {
        return Err("invalid extension UI placements".into());
    }
    if let Some(provider) = &manifest.settings_provider
        && (!manifest
            .placements
            .iter()
            .any(|place| place == "application")
            || provider.is_empty()
            || provider.len() > 128
            || !provider.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            }))
    {
        return Err("invalid application settings provider".into());
    }
    let mut aliases = std::collections::BTreeSet::new();
    for method in &manifest.methods {
        for field in [
            &method.alias,
            &method.provider,
            &method.service,
            &method.method,
            &method.scope,
            &method.min_role,
        ] {
            if field.is_empty()
                || field.len() > 128
                || !field
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b))
            {
                return Err("invalid extension method declaration".into());
            }
        }
        if !aliases.insert(&method.alias) {
            return Err("duplicate extension method alias".into());
        }
    }
    let mut total = 0_u64;
    for (name, asset) in &manifest.files {
        validate_asset_name(name)?;
        decode_hex::<32>(&asset.sha256)?;
        if asset.size > MAX_ASSET_BYTES {
            return Err("extension asset exceeds limit".into());
        }
        total = total
            .checked_add(asset.size)
            .ok_or("extension size overflow")?;
    }
    if total > MAX_BUNDLE_BYTES {
        return Err("extension exceeds size limit".into());
    }
    Ok(())
}

/// Build a manifest and copy its source assets. The publisher must serialize
/// and sign the returned manifest as manifest.json and manifest.sig.
pub fn prepare_extension(
    source: &Path,
    output: &Path,
    id: &str,
    name: &str,
    entrypoint: &str,
    sequence: u64,
    expires_unix: u64,
) -> PackageResult<Manifest> {
    if output.try_exists().map_err(|e| e.to_string())? {
        return Err("extension output already exists".into());
    }
    let mut files = BTreeMap::new();
    publication::collect(source, "", &mut files, &mut 0)?;
    let manifest = Manifest {
        schema: 1,
        project: "amaterasu.web.extension".into(),
        id: id.into(),
        name: name.into(),
        sequence,
        expires_unix,
        sdk_version: 1,
        entrypoint: entrypoint.into(),
        placements: default_placements(),
        settings_provider: None,
        methods: Vec::new(),
        files,
    };
    validate_manifest(&manifest, 0)?;
    create_private_directory(output)?;
    for (name, asset) in &manifest.files {
        let bytes = read_bounded(&safe_path(source, name)?, MAX_ASSET_BYTES)?;
        if bytes.len() as u64 != asset.size || sha256(&bytes) != asset.sha256 {
            return Err("extension source changed during packaging".into());
        }
        publication::write_asset(output, name, &bytes)?;
    }
    Ok(manifest)
}

pub fn verify_extension(
    directory: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<VerifiedExtension> {
    let raw = read_bounded(&safe_path(directory, "manifest.json")?, 65_536)?;
    let signature: ReleaseSignature = read_json(&safe_path(directory, "manifest.sig")?)?;
    verify_detached_signature(keys, &raw, &signature)?;
    let manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    validate_manifest(&manifest, now)?;
    let mut assets = BTreeMap::new();
    for (name, asset) in &manifest.files {
        let bytes = read_bounded(&safe_path(directory, name)?, MAX_ASSET_BYTES)?;
        if bytes.len() as u64 != asset.size || sha256(&bytes) != asset.sha256 {
            return Err("extension asset integrity verification failed".into());
        }
        assets.insert(name.clone(), bytes);
    }
    Ok(VerifiedExtension {
        digest: sha256(&raw),
        manifest,
        assets,
    })
}

/// Stages and selects a locally supplied signed extension. The incoming signer
/// is pinned by `fingerprint`; `keys` may also contain the previously approved
/// signer to permit an explicit publisher rotation. `root` is dedicated to this
/// extension ID and must retain its local selection state for rollback defense.
pub fn stage_extension(
    source: &Path,
    root: &Path,
    keys: &BTreeMap<String, String>,
    fingerprint: &str,
    now: u64,
) -> PackageResult<String> {
    decode_hex::<32>(fingerprint)?;
    if fingerprint != fingerprint.to_ascii_lowercase() || keys.is_empty() {
        return Err("extension requires its pinned approved publisher key".into());
    }
    for (key_id, key) in keys {
        if key_id != &key_id.to_ascii_lowercase()
            || decode_hex::<32>(key).map_or(true, |bytes| sha256(&bytes) != *key_id)
        {
            return Err("extension publisher fingerprint does not match its key".into());
        }
    }
    let current_key = keys
        .get(fingerprint)
        .ok_or("extension publisher key is absent")?;
    let current_keys = BTreeMap::from([(fingerprint.to_owned(), current_key.clone())]);
    create_private_directory(root)?;
    let _lock = RootLock::acquire(root)?;
    let verified = verify_extension(source, &current_keys, now)?;
    if root.file_name().and_then(|name| name.to_str()) != Some(verified.manifest.id.as_str()) {
        return Err("extension root must be named for its signed id".into());
    }
    let previous = if root.join("selection.json").exists() {
        Some(read_selection(root)?)
    } else {
        None
    };
    if let Some(old) = &previous {
        let existing = load_selected(root, keys, 0)?;
        if existing.manifest.id != verified.manifest.id {
            return Err("extension selection belongs to another id".into());
        }
        if verified.manifest.sequence < old.highest_sequence
            || (verified.manifest.sequence == old.highest_sequence
                && old
                    .project_floor
                    .as_ref()
                    .map_or(old.digest.as_str(), |floor| floor.digest.as_str())
                    != verified.digest)
        {
            return Err("extension sequence replay or equivocation across publishers".into());
        }
        if let Some(floor) = old.publisher_floors.get(fingerprint)
            && (verified.manifest.sequence < floor.sequence
                || (verified.manifest.sequence == floor.sequence
                    && verified.digest != floor.digest))
        {
            return Err("extension sequence replay or equivocation".into());
        }
    }
    let bundles = root.join("bundles");
    create_private_directory(&bundles)?;
    require_directory(&bundles)?;
    let destination = bundles.join(&verified.digest);
    if destination.exists() {
        let existing = verify_extension(&destination, &current_keys, now)?;
        if existing.digest != verified.digest {
            return Err("existing extension mismatch".into());
        }
    } else {
        let temporary = bundles.join(format!(".stage-{}", random_identifier()?));
        create_private_directory(&temporary)?;
        let result = (|| {
            for (name, bytes) in &verified.assets {
                publication::write_asset(&temporary, name, bytes)?;
            }
            for name in ["manifest.json", "manifest.sig"] {
                let bytes = read_bounded(&safe_path(source, name)?, 65_536)?;
                amaterasu_storage::write_atomically(&temporary.join(name), &bytes)?;
            }
            if verify_extension(&temporary, &current_keys, now)?.digest != verified.digest {
                return Err("extension manifest changed during staging".into());
            }
            fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
            amaterasu_storage::sync_directory(&bundles)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result?;
    }
    let mut floors = previous
        .as_ref()
        .map_or_else(BTreeMap::new, |old| old.publisher_floors.clone());
    floors.insert(
        fingerprint.into(),
        PublisherFloor {
            sequence: verified.manifest.sequence,
            digest: verified.digest.clone(),
        },
    );
    let selection = Selection {
        digest: verified.digest.clone(),
        highest_sequence: verified
            .manifest
            .sequence
            .max(previous.as_ref().map_or(0, |old| old.highest_sequence)),
        active_sequence: Some(verified.manifest.sequence),
        previous_digest: previous.as_ref().and_then(|old| {
            if old.digest == verified.digest {
                old.previous_digest.clone()
            } else {
                Some(old.digest.clone())
            }
        }),
        publisher_floors: floors,
        project_floor: Some(PublisherFloor {
            sequence: verified.manifest.sequence,
            digest: verified.digest.clone(),
        }),
    };
    write_json_atomically(&root.join("selection.json"), &selection)?;
    Ok(verified.digest)
}

pub fn load_selected(
    root: &Path,
    keys: &BTreeMap<String, String>,
    now: u64,
) -> PackageResult<VerifiedExtension> {
    let selection = read_selection(root)?;
    let directory = safe_directory(root, &format!("bundles/{}", selection.digest))?;
    let bundle = verify_extension(&directory, keys, now)?;
    if bundle.digest != selection.digest
        || bundle.manifest.sequence
            != selection
                .active_sequence
                .unwrap_or(selection.highest_sequence)
    {
        return Err("extension selection does not match signed bundle".into());
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_hex, write_json_atomically};
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn application_placements_are_bounded_and_legacy_pages_keep_their_default() {
        let legacy = serde_json::json!({"schema":1,"project":"amaterasu.web.extension",
            "id":"hello","name":"Hello","sequence":1,"expires_unix":100,
            "sdk_version":1,"entrypoint":"main.js","methods":[],
            "files":{"main.js":{"sha256":"a".repeat(64),"size":1}}});
        let mut manifest: Manifest = serde_json::from_value(legacy).unwrap();
        assert_eq!(manifest.placements, ["page"]);
        manifest.placements = vec!["application".into()];
        manifest.settings_provider = Some("amaterasu.rtx-hdr".into());
        assert!(validate_manifest(&manifest, 0).is_ok());
        manifest.settings_provider = Some("wrong/provider".into());
        assert!(validate_manifest(&manifest, 0).is_err());
        manifest.settings_provider = None;
        for placements in [vec![], vec!["application", "application"], vec!["unknown"]] {
            manifest.placements = placements.into_iter().map(str::to_owned).collect();
            assert!(validate_manifest(&manifest, 0).is_err());
        }
    }

    #[test]
    fn signed_extension_is_selected_and_replay_and_tampering_are_rejected() {
        let temp = std::env::temp_dir().join(format!(
            "amaterasu-extension-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&temp).unwrap();
        let source = temp.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main.js"), "example").unwrap();
        let signer = SigningKey::from_bytes(&[42; 32]);
        let fingerprint = sha256(signer.verifying_key().as_bytes());
        let keys = BTreeMap::from([(
            fingerprint.clone(),
            encode_hex(signer.verifying_key().as_bytes()),
        )]);
        let make = |sequence, label: &str| {
            let output = temp.join(label);
            let manifest =
                prepare_extension(&source, &output, "hello", "Hello", "main.js", sequence, 100)
                    .unwrap();
            let bytes = serde_json::to_vec(&manifest).unwrap();
            fs::write(output.join("manifest.json"), &bytes).unwrap();
            write_json_atomically(
                &output.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: fingerprint.clone(),
                    signature: encode_hex(&signer.sign(&bytes).to_bytes()),
                },
            )
            .unwrap();
            output
        };
        let older = make(1, "older");
        let newer = make(2, "newer");
        let root = temp.join("hello");
        stage_extension(&newer, &root, &keys, &fingerprint, 10).unwrap();
        assert_eq!(
            load_selected(&root, &keys, 10).unwrap().manifest.id,
            "hello"
        );
        assert!(stage_extension(&older, &root, &keys, &fingerprint, 10).is_err());
        assert!(load_selected(&root, &keys, 100).is_err());
        fs::write(newer.join("main.js"), "changed").unwrap();
        assert!(verify_extension(&newer, &keys, 10).is_err());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn publisher_rotation_requires_old_key_and_preserves_sequence_floor() {
        let temp = std::env::temp_dir().join(format!(
            "amaterasu-rotation-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&temp).unwrap();
        let source = temp.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main.js"), "example").unwrap();
        let old = SigningKey::from_bytes(&[42; 32]);
        let new = SigningKey::from_bytes(&[43; 32]);
        let key = |signer: &SigningKey| {
            (
                sha256(signer.verifying_key().as_bytes()),
                encode_hex(signer.verifying_key().as_bytes()),
            )
        };
        let (old_id, old_public) = key(&old);
        let (new_id, new_public) = key(&new);
        let make = |sequence, label: &str, signer: &SigningKey, fingerprint: &str| {
            let output = temp.join(label);
            let manifest =
                prepare_extension(&source, &output, "hello", "Hello", "main.js", sequence, 100)
                    .unwrap();
            let raw = serde_json::to_vec(&manifest).unwrap();
            fs::write(output.join("manifest.json"), &raw).unwrap();
            write_json_atomically(
                &output.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: fingerprint.into(),
                    signature: encode_hex(&signer.sign(&raw).to_bytes()),
                },
            )
            .unwrap();
            output
        };
        let original = make(2, "original", &old, &old_id);
        let replay = make(1, "replay", &new, &new_id);
        let update = make(3, "update", &new, &new_id);
        let root = temp.join("hello");
        let old_keys = BTreeMap::from([(old_id.clone(), old_public.clone())]);
        let new_keys = BTreeMap::from([(new_id.clone(), new_public.clone())]);
        let both = BTreeMap::from([(old_id.clone(), old_public), (new_id.clone(), new_public)]);
        stage_extension(&original, &root, &old_keys, &old_id, 10).unwrap();
        assert!(stage_extension(&update, &root, &new_keys, &new_id, 10).is_err());
        assert!(stage_extension(&replay, &root, &both, &new_id, 10).is_err());
        stage_extension(&update, &root, &both, &new_id, 10).unwrap();
        assert_eq!(
            load_selected(&root, &new_keys, 10)
                .unwrap()
                .manifest
                .sequence,
            3
        );
        fs::remove_dir_all(temp).unwrap();
    }
}
