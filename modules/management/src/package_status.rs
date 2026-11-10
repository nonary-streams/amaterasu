//! What the running package is and which modules it loaded.
//!
//! The inventory and every module description come from the manifest whose
//! digest the host reports for the bytes it loaded. Persisted selection phases
//! are shown beside that digest and never stand in for it. None of this
//! establishes that an individual module is healthy.
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use amaterasu_package::{ComponentManifest, ReleaseManifest};
use serde_json::{Value, json};
use std::path::Path;

const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MODULES: usize = 128;
/// Longest list a module description carries, so one reply stays far below
/// the management response limit whatever a manifest declares.
const MAX_DESCRIBED_ENTRIES: usize = 96;

pub(crate) fn unavailable() -> Value {
    json!({"ok":false,"code":"package_selection_unavailable"})
}

/// The digest of the package the host verified and loaded into this worker.
pub(crate) fn running_digest(runtime: &RuntimeClient) -> Option<String> {
    match runtime.send(Command::GetRunningPackageDigest {}) {
        Ok(Response::Result { payload }) => payload
            .get("digest")
            .and_then(Value::as_str)
            .filter(|digest| is_sha256(digest))
            .map(str::to_owned),
        _ => None,
    }
}

/// Persisted selection phases with the running package and its modules.
pub(crate) fn selection_status(package_root: &str, running_digest: Option<&str>) -> Value {
    package_selection_status_with_running(
        package_root,
        running_package_status_value(running_digest),
    )
    .unwrap_or_else(unavailable)
}

fn package_selection_status(package_root: &str) -> Option<Value> {
    let root = trusted_root(package_root)?;
    let observed = amaterasu_package::repository::observe_native_selection(root).ok()?;
    let selection = &observed.selection;
    for digest in [
        Some(selection.revision.as_str()),
        selection.active.as_deref(),
        selection.previous.as_deref(),
        selection.pending.as_deref(),
        selection.trial.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !is_sha256(digest) {
            return None;
        }
    }

    Some(json!({
        "ok": true,
        "revision": selection.revision,
        "active": selection.active,
        "previous": selection.previous,
        "pending": selection.pending,
        "trial": selection.trial,
        "highest_sequence": selection.highest_sequence.to_string(),
    }))
}

/// A package root is used only when it is the real directory it names.
fn trusted_root(package_root: &str) -> Option<&Path> {
    let root = Path::new(package_root);
    if !root.is_absolute() {
        return None;
    }
    let metadata = std::fs::symlink_metadata(root).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    if root.canonicalize().ok()? != root {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return None;
        }
    }
    Some(root)
}

fn package_selection_status_with_running(package_root: &str, mut running: Value) -> Option<Value> {
    let mut status = package_selection_status(package_root)?;
    if running["available"] == true
        && let Some(digest) = running["digest"]
            .as_str()
            .filter(|digest| is_sha256(digest))
            .map(str::to_owned)
    {
        let root = Path::new(package_root);
        if let Some(modules) = running_package_modules(root, &digest) {
            running["modules"] = modules;
        }
        if let Some(source) = running_package_source(root, &digest) {
            running["source"] = source;
        }
    }
    status["running"] = running;
    Some(status)
}

/// The repository recorded as the supplier of the running set, when the
/// selection keeps that provenance.
fn running_package_source(root: &Path, digest: &str) -> Option<Value> {
    let observed = amaterasu_package::repository::observe_native_selection(root).ok()?;
    let source = observed.repository_sets.get(digest)?;
    (is_repository_id(&source.repository_id) && is_sha256(&source.key_fingerprint)).then(|| {
        json!({
            "repository": source.repository_id,
            "fingerprint": source.key_fingerprint,
        })
    })
}

fn running_manifest(root: &Path, digest: &str) -> Option<ReleaseManifest> {
    let path = amaterasu_package::package_directory(root, digest)
        .ok()?
        .join("manifest.json");
    if path.canonicalize().ok()? != path {
        return None;
    }
    let raw = amaterasu_package::read_bounded(&path, MAX_MANIFEST_BYTES).ok()?;
    // The running digest comes from the host's verified loaded bytes. Never
    // derive this inventory from persisted active/pending selection or settings.
    if amaterasu_package::sha256(&raw) != digest {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

fn running_package_modules(root: &Path, digest: &str) -> Option<Value> {
    let manifest = running_manifest(root, digest)?;
    let modules: Vec<_> = manifest
        .components
        .iter()
        .map(|component| amaterasu_module_sdk::RunningModuleIdentity {
            id: component.id.clone(),
            version: component.version.clone(),
            sha256: component.sha256.clone(),
            runtime: component.runtime,
        })
        .collect();
    let value = serde_json::to_value(modules).ok()?;
    valid_running_modules(&value).then_some(value)
}

fn valid_running_modules(value: &Value) -> bool {
    let Ok(modules) =
        serde_json::from_value::<Vec<amaterasu_module_sdk::RunningModuleIdentity>>(value.clone())
    else {
        return false;
    };
    let mut ids = std::collections::BTreeSet::new();
    !modules.is_empty()
        && modules.len() <= MAX_MODULES
        && modules.iter().all(|module| {
            is_module_id(&module.id)
                && !module.version.is_empty()
                && module.version.len() <= 128
                && !module.version.chars().any(char::is_control)
                && is_sha256(&module.sha256)
                && ids.insert(&module.id)
        })
}

/// What one running module declares in the signed manifest: the modules it
/// needs, the modules that need it, and the services it asked to provide and
/// call. These are the package's requests; host policy decides what is granted.
pub(crate) fn module_description(
    package_root: &str,
    running_digest: Option<&str>,
    id: &str,
) -> Value {
    let described = running_digest
        .filter(|digest| is_sha256(digest))
        .zip(trusted_root(package_root))
        .and_then(|(digest, root)| running_manifest(root, digest))
        .filter(|manifest| valid_inventory(&manifest.components));
    let Some(manifest) = described else {
        return unavailable();
    };
    let Some(component) = manifest
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        return json!({"ok":false,"code":"module_not_found"});
    };
    let required_by: Vec<&str> = manifest
        .components
        .iter()
        .filter(|other| other.requires.contains_key(id))
        .map(|other| other.id.as_str())
        .collect();
    let requires: Vec<Value> = component
        .requires
        .iter()
        .filter(|(module, version)| is_module_id(module) && is_plain_text(version))
        .take(MAX_DESCRIBED_ENTRIES)
        .map(|(module, version)| json!({"id": module, "version": version}))
        .collect();
    let requested = &component.requested;
    json!({
        "ok": true,
        "module": {
            "id": component.id,
            "version": component.version,
            "sha256": component.sha256,
            "runtime": component.runtime,
            "size": component.size,
            "abi_major": component.abi_major,
            "requires": requires,
            "required_by": bounded(required_by.into_iter()),
            "provides": bounded(requested.provide.iter().map(String::as_str)),
            "calls": bounded(requested.call.iter().map(String::as_str)),
            "native_provides": bounded(requested.native_provide.iter().map(String::as_str)),
            "native_uses": bounded(requested.native_use.iter().map(String::as_str)),
            "publishes": bounded(requested.publish.iter().map(String::as_str)),
            "subscribes": bounded(requested.subscribe.iter().map(String::as_str)),
            "hooks": bounded(requested.hook.iter().map(String::as_str)),
        },
    })
}

fn valid_inventory(components: &[ComponentManifest]) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    !components.is_empty()
        && components.len() <= MAX_MODULES
        && components.iter().all(|component| {
            is_module_id(&component.id)
                && is_plain_text(&component.version)
                && is_sha256(&component.sha256)
                && ids.insert(&component.id)
        })
}

fn bounded<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    names
        .filter(|name| is_plain_text(name))
        .take(MAX_DESCRIBED_ENTRIES)
        .collect()
}

fn is_plain_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(crate) fn is_module_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn is_repository_id(value: &str) -> bool {
    is_module_id(value) && value != "." && value != ".."
}

fn running_package_status_value(digest: Option<&str>) -> Value {
    digest.filter(|digest| is_sha256(digest)).map_or_else(
        || json!({"available":false}),
        |digest| json!({"available":true,"digest":digest}),
    )
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_package_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-management-selection-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn package_selection_status_reads_fresh_bounded_phase_fields_only() {
        let root = temporary_package_root();
        let active = "a".repeat(64);
        let pending = "b".repeat(64);
        let trial = "c".repeat(64);
        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &json!({
                "active": active.clone(),
                "previous": null,
                "pending": pending.clone(),
                "trial": trial.clone(),
                "highest_sequence": u64::MAX,
                "repository_sets": {"set": {"repository_id":"private-publisher", "key_fingerprint":"d".repeat(64)}},
                "repository_floors": {},
                "healthy_sets": [],
            }),
        )
        .unwrap();

        let status = package_selection_status_with_running(
            root.to_str().unwrap(),
            running_package_status_value(Some(&"e".repeat(64))),
        )
        .unwrap();
        assert_eq!(status["ok"], true);
        assert!(is_sha256(status["revision"].as_str().unwrap()));
        assert_eq!(status["active"], active);
        assert_eq!(status["previous"], serde_json::Value::Null);
        assert_eq!(status["pending"], pending);
        assert_eq!(status["trial"], trial);
        assert_eq!(status["highest_sequence"], u64::MAX.to_string());
        assert_eq!(status["running"]["available"], true);
        assert_eq!(status["running"]["digest"], "e".repeat(64));
        assert!(status.get("package_root").is_none());
        assert!(status.get("repository_sets").is_none());
        assert!(!status.to_string().contains("private-publisher"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_selection_status_fails_closed_for_missing_malformed_or_unsafe_state() {
        let root = temporary_package_root();
        let empty = package_selection_status(root.to_str().unwrap()).unwrap();
        assert_eq!(empty["active"], serde_json::Value::Null);
        assert_eq!(empty["pending"], serde_json::Value::Null);
        assert_eq!(
            empty["revision"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        std::fs::write(root.join("selection.json"), b"not json").unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());

        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &json!({
                "active": "A".repeat(64),
                "previous": null,
                "pending": null,
                "trial": null,
                "highest_sequence": 1,
                "repository_sets": {},
                "repository_floors": {},
                "healthy_sets": [],
            }),
        )
        .unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());
        assert!(package_selection_status("relative/package-root").is_none());
        assert_eq!(
            running_package_status_value(None),
            json!({"available":false})
        );
        assert_eq!(
            running_package_status_value(Some("not-a-digest")),
            json!({"available":false})
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn package_selection_status_rejects_symlinked_state_files_and_roots() {
        use std::os::unix::fs::symlink;

        let root = temporary_package_root();
        let target = root.join("target.json");
        std::fs::write(
            &target,
            br#"{"active":null,"previous":null,"pending":null,"trial":null,"highest_sequence":0,"repository_sets":{},"repository_floors":{},"healthy_sets":[]}"#,
        )
        .unwrap();
        symlink(&target, root.join("selection.json")).unwrap();
        assert!(package_selection_status(root.to_str().unwrap()).is_none());

        let alias = root.with_extension("alias");
        symlink(&root, &alias).unwrap();
        assert!(package_selection_status(alias.to_str().unwrap()).is_none());

        std::fs::remove_file(alias).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn module_inventory_uses_the_running_manifest_and_rejects_tampered_bytes() {
        let root = temporary_package_root();
        let components: Vec<_> = [
            "amaterasu.runtime",
            "amaterasu.display",
            "amaterasu.frame-limiter",
        ]
        .iter()
        .enumerate()
        .map(|(index, id)| {
            json!({
                "id": id,
                "version": "0.1.0",
                "file": "private-library.so",
                "sha256": "b".repeat(64),
                "size": 1,
                "abi_major": 1,
                "runtime": index == 0,
                "requested": {}
            })
        })
        .collect();
        let raw = serde_json::to_vec(&json!({
            "schema": 1,
            "host_context_schema": 1,
            "project": "amaterasu",
            "sequence": 1,
            "expires_unix": u64::MAX,
            "target": "test",
            "components": components
        }))
        .unwrap();
        let digest = amaterasu_package::sha256(&raw);
        let directory = amaterasu_package::package_directory(&root, &digest).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manifest.json"), &raw).unwrap();
        let modules = running_package_modules(&root, &digest).unwrap();
        assert_eq!(modules[1]["id"], "amaterasu.display");
        assert_eq!(modules[2]["id"], "amaterasu.frame-limiter");
        assert!(modules[0].get("file").is_none());
        assert!(
            running_package_modules(&root, &"f".repeat(64)).is_none(),
            "a persisted active digest is never a substitute for the host's running digest"
        );
        std::fs::write(directory.join("manifest.json"), b"{}").unwrap();
        assert!(running_package_modules(&root, &digest).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Writes a signed-manifest-shaped set under `root` and returns its digest.
    fn stage_running_set(root: &Path) -> String {
        let grants = |provide: &[&str], call: &[&str]| json!({"provide": provide, "call": call});
        let raw = serde_json::to_vec(&json!({
            "schema": 1,
            "host_context_schema": 1,
            "project": "amaterasu",
            "sequence": 1,
            "expires_unix": u64::MAX,
            "target": "test",
            "components": [
                {
                    "id": "amaterasu.runtime", "version": "0.1.0", "file": "private-runtime.so",
                    "sha256": "a".repeat(64), "size": 2_877_544, "abi_major": 1, "runtime": true,
                    "requested": {}
                },
                {
                    "id": "amaterasu.display", "version": "0.2.0", "file": "private-display.so",
                    "sha256": "b".repeat(64), "size": 2_971_272, "abi_major": 1, "runtime": false,
                    "requires": {"amaterasu.runtime": "0.1.0"},
                    "requested": grants(&["amaterasu.display/1"], &["amaterasu.state/1"])
                },
                {
                    "id": "example.overlay", "version": "1.2.0", "file": "private-overlay.so",
                    "sha256": "c".repeat(64), "size": 10, "abi_major": 1, "runtime": false,
                    "requires": {"amaterasu.display": "0.2.0", "amaterasu.runtime": "0.1.0"},
                    "requested": grants(&["example.overlay/1"], &["amaterasu.display/1"])
                }
            ]
        }))
        .unwrap();
        let digest = amaterasu_package::sha256(&raw);
        let directory = amaterasu_package::package_directory(root, &digest).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("manifest.json"), &raw).unwrap();
        digest
    }

    #[test]
    fn status_is_served_while_the_running_service_holds_the_activation_lock() {
        let root = temporary_package_root();
        let digest = stage_running_set(&root);
        amaterasu_package::write_json_atomically(
            &root.join("selection.json"),
            &json!({
                "active": digest,
                "previous": null,
                "pending": null,
                "trial": null,
                "highest_sequence": 1,
                "repository_sets": {
                    digest.clone(): {
                        "repository_id": "example-plugins",
                        "key_fingerprint": "d".repeat(64)
                    }
                },
                "repository_floors": {},
                "healthy_sets": [],
            }),
        )
        .unwrap();
        // The service holds this lock from start to exit, so a status read
        // that needed it could never succeed on an installed host.
        let service_lock = amaterasu_package::RootLock::acquire(&root).unwrap();

        let status = selection_status(root.to_str().unwrap(), Some(&digest));
        assert_eq!(status["ok"], true);
        assert_eq!(status["active"], digest.as_str());
        assert_eq!(status["running"]["digest"], digest.as_str());
        assert_eq!(status["running"]["modules"].as_array().unwrap().len(), 3);
        assert_eq!(
            status["running"]["source"],
            json!({"repository": "example-plugins", "fingerprint": "d".repeat(64)})
        );

        // A set the selection does not attribute to a repository has no source.
        let unattributed = selection_status(root.to_str().unwrap(), Some(&"e".repeat(64)));
        assert_eq!(unattributed["running"]["available"], true);
        assert!(unattributed["running"].get("source").is_none());
        assert!(unattributed["running"].get("modules").is_none());
        assert_eq!(
            selection_status(root.to_str().unwrap(), None)["running"],
            json!({"available": false})
        );
        drop(service_lock);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_description_reports_declared_dependencies_and_services() {
        let root = temporary_package_root();
        let digest = stage_running_set(&root);
        let describe = |id: &str| module_description(root.to_str().unwrap(), Some(&digest), id);

        let display = describe("amaterasu.display");
        assert_eq!(display["ok"], true);
        let module = &display["module"];
        assert_eq!(module["version"], "0.2.0");
        assert_eq!(module["size"], 2_971_272);
        assert_eq!(
            module["requires"],
            json!([{"id": "amaterasu.runtime", "version": "0.1.0"}])
        );
        assert_eq!(module["required_by"], json!(["example.overlay"]));
        assert_eq!(module["provides"], json!(["amaterasu.display/1"]));
        assert_eq!(module["calls"], json!(["amaterasu.state/1"]));
        assert_eq!(module["hooks"], json!([]));
        assert!(module.get("file").is_none(), "library paths stay private");

        let runtime = describe("amaterasu.runtime");
        assert_eq!(runtime["module"]["runtime"], true);
        assert_eq!(
            runtime["module"]["required_by"],
            json!(["amaterasu.display", "example.overlay"])
        );

        assert_eq!(
            describe("amaterasu.missing"),
            json!({"ok": false, "code": "module_not_found"})
        );
        // Only the manifest of the digest the host reports is described.
        for digest in [None, Some("f".repeat(64)), Some("not-a-digest".into())] {
            assert_eq!(
                module_description(
                    root.to_str().unwrap(),
                    digest.as_deref(),
                    "amaterasu.display"
                ),
                unavailable()
            );
        }
        assert_eq!(
            module_description("relative/root", Some(&digest), "amaterasu.display"),
            unavailable()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
