//! Explicit development administrator actions for approved publishers.
use amaterasu_package::repository::{
    ApprovedRepository, Candidate, CandidateKind, HttpsRepository, RepositoryStore,
    activate_staged_native, download_candidate, refresh_catalog, stage_native_candidate,
    verify_candidate,
};
use amaterasu_package::{
    BootstrapConfig, PackageResult, current_target, read_json, unix_timestamp, web,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn development(root: &str) -> PackageResult<(PathBuf, BootstrapConfig, RepositoryStore)> {
    let root = PathBuf::from(root);
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("production repository changes need installed service authorization".into());
    }
    let store = RepositoryStore::new(root.join("repositories"))?;
    Ok((root, bootstrap, store))
}

pub fn approve(
    root: &str,
    id: &str,
    location: &str,
    public_key: &str,
    confirmed_fingerprint: &str,
) -> PackageResult<()> {
    let (_, _, store) = development(root)?;
    let approved = ApprovedRepository::approve(
        id.into(),
        location.into(),
        public_key.into(),
        confirmed_fingerprint.into(),
    )?;
    store.approve(approved)?;
    println!(
        "Approved {id} to supply any native module slot and the UI. This does not grant elevation; native code has its worker process privileges."
    );
    Ok(())
}

pub fn revoke(root: &str, id: &str) -> PackageResult<()> {
    let (_, _, store) = development(root)?;
    store.revoke(id)?;
    println!(
        "Revoked {id}. A running native worker must be stopped separately; future starts of its sets will fail. Restore a retained set from a still-trusted signer with `amaterasu native-recover ROOT [DIGEST]`, and restore a selected UI with repository-restore-ui ROOT [DIGEST]."
    );
    Ok(())
}

pub fn refresh(root: &str, id: &str) -> PackageResult<()> {
    let (_, _, store) = development(root)?;
    let approved = store.approved(id)?;
    let source = HttpsRepository::new(&approved)?;
    refresh_catalog(&store, id, &source, &current_target(), unix_timestamp())?;
    println!("Verified and saved catalog for {id}.");
    Ok(())
}

pub fn list(root: &str) -> PackageResult<()> {
    let (_, _, store) = development(root)?;
    let listing = store.list(&current_target(), unix_timestamp())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&listing).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn choice<'a>(
    candidates: &'a [Candidate],
    slot: &str,
    implementation: &str,
    version: &str,
) -> PackageResult<&'a Candidate> {
    candidates
        .iter()
        .find(|candidate| {
            candidate.slot == slot
                && candidate.implementation == implementation
                && candidate.version == version
        })
        .ok_or_else(|| "selected implementation is absent from the verified catalog".into())
}

pub fn download(
    root: &str,
    id: &str,
    slot: &str,
    implementation: &str,
    version: &str,
    output: &Path,
) -> PackageResult<()> {
    let (_, bootstrap, store) = development(root)?;
    let approved = store.approved(id)?;
    let catalog = store.catalog(id, &current_target(), unix_timestamp())?;
    let candidate = choice(catalog.candidates(), slot, implementation, version)?;
    let source = HttpsRepository::new(&approved)?;
    download_candidate(
        &source,
        &approved,
        &catalog,
        candidate,
        output,
        &bootstrap,
        unix_timestamp(),
    )?;
    println!(
        "Verified candidate at {}. No code was loaded or activated.",
        output.display()
    );
    Ok(())
}

pub fn install_ui(
    root: &str,
    id: &str,
    implementation: &str,
    version: &str,
    output: &Path,
) -> PackageResult<()> {
    let (root, bootstrap, store) = development(root)?;
    let approved = store.approved(id)?;
    let catalog = store.catalog(id, &current_target(), unix_timestamp())?;
    let candidate = choice(
        catalog.candidates(),
        "amaterasu.ui",
        implementation,
        version,
    )?;
    if candidate.kind != CandidateKind::WebBundle {
        return Err("selected choice is not a UI bundle".into());
    }
    if output.exists() {
        verify_candidate(
            &approved,
            &catalog,
            candidate,
            output,
            &bootstrap,
            unix_timestamp(),
        )?;
    } else {
        let source = HttpsRepository::new(&approved)?;
        download_candidate(
            &source,
            &approved,
            &catalog,
            candidate,
            output,
            &bootstrap,
            unix_timestamp(),
        )?;
    }
    let digest = store.with_current_catalog(
        id,
        catalog.digest(),
        &current_target(),
        unix_timestamp(),
        |current_approval, current_catalog| {
            let current_choice = choice(
                current_catalog.candidates(),
                "amaterasu.ui",
                implementation,
                version,
            )?;
            verify_candidate(
                current_approval,
                current_catalog,
                current_choice,
                output,
                &bootstrap,
                unix_timestamp(),
            )?;
            let keys = BTreeMap::from([(
                current_approval.fingerprint.clone(),
                current_approval.public_key.clone(),
            )]);
            web::stage_repository_bundle(
                output,
                &root.join("web"),
                &keys,
                &current_approval.fingerprint,
                unix_timestamp(),
            )
        },
    )?;
    println!("Selected UI {digest}. Recovery remains at /recovery and repository-restore-ui.");
    Ok(())
}

pub fn stage_native(
    root: &str,
    id: &str,
    slot: &str,
    implementation: &str,
    version: &str,
    source: &Path,
) -> PackageResult<()> {
    let (root, bootstrap, store) = development(root)?;
    let catalog = store.catalog(id, &current_target(), unix_timestamp())?;
    let digest = store.with_current_catalog(
        id,
        catalog.digest(),
        &current_target(),
        unix_timestamp(),
        |approved, current_catalog| {
            let candidate = choice(current_catalog.candidates(), slot, implementation, version)?;
            stage_native_candidate(
                source,
                &root,
                &bootstrap,
                approved,
                current_catalog,
                candidate,
                unix_timestamp(),
            )
        },
    )?;
    println!(
        "Staged signed native set {digest} without loading code. After review, explicitly authorize a future worker trial with `amaterasu-release repository-activate-native ROOT {digest}`."
    );
    Ok(())
}

pub fn activate_native(root: &str, digest: &str) -> PackageResult<()> {
    let (root, bootstrap, store) = development(root)?;
    activate_staged_native(&root, &bootstrap, &store, digest, unix_timestamp())?;
    println!(
        "Authorized signed native set {digest} for the next development worker trial. Its native code will execute under the user account when the worker starts."
    );
    Ok(())
}

pub fn restore_ui(root: &str, digest: Option<&str>) -> PackageResult<()> {
    let (root, bootstrap, store) = development(root)?;
    let digest = store.with_active_keys(|repository_keys| {
        let mut keys = bootstrap.trusted_keys.clone();
        keys.extend(repository_keys.clone());
        match digest {
            Some(digest) => {
                web::restore_version(&root.join("web"), digest, &keys, unix_timestamp())
            }
            None => web::restore_previous(&root.join("web"), &keys, unix_timestamp()),
        }
    })?;
    println!("Restored previous UI {digest} without lowering the accepted release sequence.");
    Ok(())
}
