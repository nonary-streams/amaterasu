//! Host-owned recovery entry point. It never loads a selected native module or
//! relies on replaceable management HTTP or UI code.
use amaterasu_package::{
    BootstrapConfig, PackageResult, read_json, repository::RepositoryStore, unix_timestamp, web,
};
use std::collections::BTreeMap;
use std::path::Path;

fn trusted_keys(root: &Path) -> PackageResult<(BootstrapConfig, BTreeMap<String, String>)> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    let mut keys = bootstrap.trusted_keys.clone();
    keys.extend(RepositoryStore::new(root.join("repositories"))?.active_keys()?);
    Ok((bootstrap, keys))
}

pub fn status(root: &Path) -> PackageResult<()> {
    let (_, keys) = trusted_keys(root)?;
    let selection = web::read_selection(&root.join("web"))?;
    let verified = web::load_selected(&root.join("web"), &keys, unix_timestamp());
    let status = serde_json::json!({
        "selection": selection.digest,
        "previous": selection.previous_digest,
        "selected_bundle_verified": verified.is_ok(),
        "recovery_command": "amaterasu web-recover ROOT DIGEST",
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&status).map_err(|error| error.to_string())?
    );
    Ok(())
}

/// Development-only mutation until installed-service owner authorization and
/// machine-protected approval state are available. A digest is a specific,
/// reviewable administrator choice; no publisher code is loaded by recovery.
pub fn restore(root: &Path, digest: &str) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("production UI recovery requires installed-service authorization".into());
    }
    let store = RepositoryStore::new(root.join("repositories"))?;
    let restored = store.with_active_keys(|repository_keys| {
        let mut keys = bootstrap.trusted_keys.clone();
        keys.extend(repository_keys.clone());
        web::restore_version(&root.join("web"), digest, &keys, unix_timestamp())
    })?;
    println!("Restored verified UI {restored}; accepted release floors remain intact.");
    Ok(())
}
