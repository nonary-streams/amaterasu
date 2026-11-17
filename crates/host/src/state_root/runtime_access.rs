//! Read-only access to the authenticated machine runtime for the active worker.
//!
//! The machine root also contains credentials and recovery journals. Never
//! inherit the worker grant into that root: only the verified payload and the
//! metadata needed to verify and load it are readable by the interactive SID.
use super::*;
use amaterasu_package::RootLock;

// File-specific rights survive a filesystem ACL round trip unchanged. Generic
// GR/GW/GX bits are mapped by Windows and cannot be compared to the stored ACE.
const READ_EXECUTE: u32 = 0x0012_00a9; // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
const READ_WRITE: u32 = 0x0012_019f; // FILE_GENERIC_READ | FILE_GENERIC_WRITE

pub(super) fn prepare(root: &Path, digest: &str, sid: &str) -> PackageResult<()> {
    let owners = ["S-1-5-18", "S-1-5-32-544"];
    let package = amaterasu_package::package_directory(root, digest)?;
    grant_read(root, sid, &owners)?;
    grant_read(&root.join("bootstrap.json"), sid, &owners)?;
    grant_read(&root.join("selection.json"), sid, &owners)?;
    grant_read(&root.join("sets"), sid, &owners)?;
    grant_read_tree(&package, sid, &owners)?;

    let plugins = root.join("plugins");
    if plugins.try_exists().map_err(|e| e.to_string())? {
        grant_read(&plugins, sid, &owners)?;
        let selection = plugins.join("selection.json");
        if selection.try_exists().map_err(|e| e.to_string())? {
            grant_read(&selection, sid, &owners)?;
        }
    }

    // Repository approvals remain machine-owned and read-only. The existing
    // lock alone is writable so publisher revocation and worker loading still
    // serialize using the same lock; the user cannot replace the lock file.
    let repositories = root.join("repositories");
    if repositories.try_exists().map_err(|e| e.to_string())? {
        canonical_real_directory(&repositories, "repository state directory")?;
        let _repository_lock = RootLock::acquire(&repositories)?;
        grant_read(&repositories, sid, &owners)?;
        let lock = repositories.join("host.lock");
        grant(&lock, sid, &owners, READ_WRITE)?;
        let approvals = repositories.join("approved.json");
        if approvals.try_exists().map_err(|e| e.to_string())? {
            grant_read(&approvals, sid, &owners)?;
        }
        let selection = amaterasu_package::plugins::installed::read(root)?;
        let sources = if selection.trial {
            &selection
                .pending
                .as_ref()
                .ok_or("plugin_trial_missing")?
                .sources
        } else {
            &selection.active
        };
        let store = amaterasu_package::repository::RepositoryStore::new(repositories.clone())?;
        for source in sources {
            let candidate = store.existing_native_candidate_cache_path(
                &source.repository,
                &source.manifest_digest,
            )?;
            grant_read(&repositories.join("native-candidates"), sid, &owners)?;
            grant_read(
                candidate.parent().ok_or("invalid candidate directory")?,
                sid,
                &owners,
            )?;
            grant_read_tree(&candidate, sid, &owners)?;
        }
    }
    Ok(())
}

fn grant_read_tree(path: &Path, sid: &str, owners: &[&str]) -> PackageResult<()> {
    grant_read(path, sid, owners)?;
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            grant_read_tree(&entry.map_err(|e| e.to_string())?.path(), sid, owners)?;
        }
    }
    Ok(())
}

fn grant_read(path: &Path, sid: &str, owners: &[&str]) -> PackageResult<()> {
    grant(path, sid, owners, READ_EXECUTE)
}

fn grant(path: &Path, sid: &str, owners: &[&str], mask: u32) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    reject_reparse(path)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("runtime read access requires a regular file or directory".into());
    }
    validate_owner(path, owners)?;
    // No worker ACE is inheritable: private siblings and later machine-created
    // state remain SYSTEM/Administrators-only.
    let sddl = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x{mask:08x};;;{sid})");
    apply_dacl(path, &sddl)?;
    validate_tls_acl(
        path,
        owners,
        &[
            ("S-1-5-18", FILE_ALL_ACCESS),
            ("S-1-5-32-544", FILE_ALL_ACCESS),
            (sid, mask),
        ],
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_read_grants_do_not_inherit_into_private_state() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-runtime-access-{}-{}",
            std::process::id(),
            amaterasu_package::random_identifier().unwrap()
        ));
        fs::create_dir_all(root.join("sets/selected")).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        fs::write(root.join("host.lock"), b"").unwrap();
        fs::write(root.join("sets/selected/module.dll"), b"signed payload").unwrap();
        fs::write(root.join("data/secret"), b"private").unwrap();
        let mut token = std::ptr::null_mut();
        // SAFETY: the process pseudo-handle is valid and token is writable.
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) },
            0
        );
        let sid = sid_string(token).unwrap();
        // SAFETY: OpenProcessToken returned an owned handle.
        unsafe { CloseHandle(token) };
        let owners = [&*sid, "S-1-5-18", "S-1-5-32-544"];
        let private = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
        set_dacl(&root.join("data/secret"), private).unwrap();
        grant_read(&root, &sid, &owners).unwrap();
        grant_read(&root.join("sets"), &sid, &owners).unwrap();
        grant_read_tree(&root.join("sets/selected"), &sid, &owners).unwrap();
        validate_tls_acl(
            &root.join("sets/selected/module.dll"),
            &owners,
            &[
                ("S-1-5-18", FILE_ALL_ACCESS),
                ("S-1-5-32-544", FILE_ALL_ACCESS),
                (&sid, READ_EXECUTE),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            fs::read(root.join("sets/selected/module.dll")).unwrap(),
            b"signed payload"
        );
        assert!(fs::write(root.join("sets/selected/module.dll"), b"modified").is_err());
        assert!(fs::write(root.join("sets/selected/extra.dll"), b"unverified").is_err());
        // The shared repository lock is the sole writable machine-owned file.
        grant(&root.join("host.lock"), &sid, &owners, READ_WRITE).unwrap();
        let lock = RootLock::acquire(&root).unwrap();
        assert!(RootLock::acquire(&root).is_err());
        drop(lock);
        assert!(RootLock::acquire(&root).is_ok());
        // The root's SID entry has no inheritance flags, so it cannot turn
        // newly created machine-private descendants into user-readable data.
        validate_dacl(&root.join("data/secret"), private).unwrap();
        assert!(fs::read(root.join("data/secret")).is_err());
        // Restore ownership access for cleanup in a non-elevated test process.
        let full = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})");
        set_dacl(&root.join("host.lock"), &full).unwrap();
        set_dacl(&root.join("data/secret"), &full).unwrap();
        set_dacl(&root.join("sets/selected/module.dll"), &full).unwrap();
        set_dacl(&root.join("sets/selected"), &full).unwrap();
        set_dacl(&root.join("sets"), &full).unwrap();
        set_dacl(&root, &full).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
