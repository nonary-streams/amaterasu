//! Per-user delivery of machine-provisioned browser TLS material.
use super::*;
use amaterasu_package::RootLock;

fn staged_path(container: &Path, sid: &str) -> PathBuf {
    container.join(format!(".management-tls-{sid}"))
}

/// Service-side work stays in the machine-owned sibling of the user's root.
pub(super) fn stage_for_user(
    root: &Path,
    container: &Path,
    sid: &str,
    active: Option<&[String; 2]>,
) -> PackageResult<()> {
    let source = root.join("management-tls");
    let staged = staged_path(container, sid);
    remove_abandoned_stages(container)?;
    if active.is_none() {
        if staged.try_exists().map_err(|error| error.to_string())? {
            canonical_real_directory(&staged, "previous management TLS snapshot")?;
            validate_real_tree(&staged)?;
            fs::remove_dir_all(&staged).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let active = active.ok_or("management TLS configuration is missing")?;
    canonical_real_directory(&source, "machine management TLS directory")?;
    validate_machine_tls_acl(&source)?;
    let pending = container.join(format!(".tls-pending-{}", random_identifier()?));
    create_real_directory(&pending)?;
    let result = (|| {
        let material = pending.join("management-tls");
        create_real_directory(&material)?;
        for name in active {
            let source_file = source.join(name);
            let metadata = fs::symlink_metadata(&source_file).map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
                return Err("machine management TLS contains an invalid file".into());
            }
            reject_reparse(&source_file)?;
            validate_machine_tls_acl(&source_file)
                .or_else(|_| validate_inherited_machine_tls_file_acl(&source_file))?;
            crate::loading::management_tls::verify_file_digest(&source_file)?;
            let target = material.join(name);
            fs::copy(&source_file, &target).map_err(|error| error.to_string())?;
            set_tls_stage_dacl(&target, sid)?;
        }
        set_tls_stage_dacl(&material, sid)?;
        set_tls_stage_dacl(&pending, sid)?;
        if staged.try_exists().map_err(|error| error.to_string())? {
            canonical_real_directory(&staged, "previous management TLS snapshot")?;
            validate_real_tree(&staged)?;
            fs::remove_dir_all(&staged).map_err(|error| error.to_string())?;
        }
        fs::rename(&pending, &staged).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}

fn remove_abandoned_stages(container: &Path) -> PackageResult<()> {
    for entry in fs::read_dir(container).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".tls-pending-")
        {
            continue;
        }
        let path = entry.path();
        canonical_real_directory(&path, "abandoned management TLS stage")?;
        validate_real_tree(&path)?;
        fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Development roots are user-owned; protect their TLS subtree before loading.
pub(crate) fn protect_development_files(
    root: &Path,
    state_root: &Path,
    active: &[String; 2],
) -> PackageResult<()> {
    let mut token = std::ptr::null_mut();
    // SAFETY: current process handle is valid and the returned token is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err("development TLS token is unavailable".into());
    }
    let sid = sid_string(token);
    // SAFETY: OpenProcessToken returned this owned handle.
    unsafe { CloseHandle(token) };
    let sid = sid?;
    for directory in [
        root.to_path_buf(),
        root.join("data"),
        state_root.to_path_buf(),
    ] {
        canonical_real_directory(&directory, "development TLS ancestor")?;
        validate_owner(&directory, &[&sid, "S-1-5-18"])?;
        set_user_dacl(&directory, &sid)?;
    }
    let enclave = state_root.join("management-tls");
    canonical_real_directory(&enclave, "development management TLS directory")?;
    validate_owner(&enclave, &[&sid, "S-1-5-18"])?;
    set_user_dacl(&enclave, &sid)?;
    for name in active {
        let path = enclave.join(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("development TLS file is not regular".into());
        }
        reject_reparse(&path)?;
        crate::loading::management_tls::verify_file_digest(&path)?;
        validate_owner(&path, &[&sid, "S-1-5-18"])?;
        set_user_dacl(&path, &sid)?;
    }
    Ok(())
}

/// User-token worker copies only missing versioned files into its own state.
pub(crate) fn apply_staged_tls(root: &Path, state_root: &Path) -> PackageResult<()> {
    let sid = state_root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|sid| valid_sid(sid))
        .ok_or("user state root has no SID")?;
    let container = root.join("user-state");
    let staged = staged_path(&container, sid);
    if !staged.try_exists().map_err(|error| error.to_string())? {
        let _lock = RootLock::acquire(state_root)?;
        prune_obsolete_user_files(state_root, sid, &[])?;
        return Ok(());
    }
    let _lock = RootLock::acquire(state_root)?;
    let staged = canonical_real_directory(&staged, "staged management TLS")?;
    validate_tls_stage_acl(&staged, sid)?;
    let source = canonical_real_directory(
        &staged.join("management-tls"),
        "staged management TLS files",
    )?;
    validate_tls_stage_acl(&source, sid)?;
    let destination = state_root.join("management-tls");
    if !destination
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        create_real_directory(&destination)?;
    }
    canonical_real_directory(&destination, "user management TLS directory")?;
    validate_owner(&destination, &[sid, "S-1-5-18"])?;
    set_user_dacl(&destination, sid)?;
    let mut active_names = Vec::new();
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let source_file = entry.path();
        let name = entry.file_name();
        valid_pem_name(&name)?;
        active_names.push(name.clone());
        let metadata = fs::symlink_metadata(&source_file).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
            return Err("staged management TLS contains an invalid file".into());
        }
        reject_reparse(&source_file)?;
        validate_tls_stage_acl(&source_file, sid)?;
        crate::loading::management_tls::verify_file_digest(&source_file)?;
        let target = destination.join(name);
        if target.try_exists().map_err(|error| error.to_string())? {
            let existing = fs::symlink_metadata(&target).map_err(|error| error.to_string())?;
            if !existing.is_file() {
                return Err("existing management TLS file is invalid".into());
            }
            reject_reparse(&target)?;
            validate_management_tls_acl(&target)?;
            if fs::read(&source_file).map_err(|error| error.to_string())?
                != fs::read(&target).map_err(|error| error.to_string())?
            {
                return Err("management TLS versioned file changed in place".into());
            }
            continue;
        }
        let temporary = destination.join(format!(".tls-{}", random_identifier()?));
        let result = (|| {
            fs::copy(&source_file, &temporary).map_err(|error| error.to_string())?;
            set_user_dacl(&temporary, sid)?;
            fs::rename(&temporary, &target).map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    prune_obsolete_user_files(state_root, sid, &active_names)?;
    Ok(())
}

fn prune_obsolete_user_files(
    state_root: &Path,
    sid: &str,
    active: &[std::ffi::OsString],
) -> PackageResult<()> {
    let directory = state_root.join("management-tls");
    if !directory.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    canonical_real_directory(&directory, "user management TLS directory")?;
    validate_management_tls_acl(&directory)?;
    for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        if crate::loading::management_tls::validate_file_name(text, "cert").is_err()
            && crate::loading::management_tls::validate_file_name(text, "key").is_err()
        {
            continue;
        }
        if active.contains(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("obsolete management TLS path is not a file".into());
        }
        reject_reparse(&path)?;
        validate_owner(&path, &[sid, "S-1-5-18"])?;
        validate_management_tls_acl(&path)?;
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn valid_pem_name(name: &OsStr) -> PackageResult<()> {
    let name = name
        .to_str()
        .ok_or("management TLS filename is not Unicode")?;
    if name.is_empty()
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || !name.ends_with(".pem")
        || name.contains(['/', '\\', ':'])
    {
        return Err("management TLS filename is invalid".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_stages_are_removed_without_touching_active_snapshots() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-tls-stage-test-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let pending = root.join(".tls-pending-interrupted");
        fs::create_dir(&pending).unwrap();
        fs::write(pending.join("partial.pem"), b"partial").unwrap();
        let active = root.join(".management-tls-S-1-5-21-1234");
        fs::create_dir(&active).unwrap();
        remove_abandoned_stages(&root).unwrap();
        assert!(!pending.exists());
        assert!(active.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn per_user_rotation_restart_and_disable_prune_only_managed_keys() {
        let mut token = std::ptr::null_mut();
        // SAFETY: current process pseudo-handle is valid and token is closed below.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) },
            0
        );
        let sid = sid_string(token).unwrap();
        // SAFETY: OpenProcessToken returned this owned token.
        unsafe { CloseHandle(token) };
        let root = std::env::temp_dir().join(format!(
            "amaterasu-tls-prune-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let directory = root.join("management-tls");
        fs::create_dir(&directory).unwrap();
        set_user_dacl(&directory, &sid).unwrap();
        let old = format!("management-key-{}.pem", "a".repeat(64));
        let current = format!("management-key-{}.pem", "b".repeat(64));
        for name in [&old, &current, "unrelated.txt"] {
            let path = directory.join(name);
            fs::write(&path, b"key").unwrap();
            set_user_dacl(&path, &sid).unwrap();
        }
        prune_obsolete_user_files(&root, &sid, &[current.clone().into()]).unwrap();
        assert!(!directory.join(&old).exists());
        prune_obsolete_user_files(&root, &sid, &[current.clone().into()]).unwrap();
        assert!(directory.join(&current).exists());
        prune_obsolete_user_files(&root, &sid, &[]).unwrap();
        assert!(!directory.join(&current).exists());
        assert!(directory.join("unrelated.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
