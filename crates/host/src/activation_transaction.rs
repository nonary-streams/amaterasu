//! Rollback journal for an authorized Linux user-service package transition.
//!
//! This module does not authorize a package or choose file paths. Its caller
//! must hold the host root lock, identify the managed unit independently, and
//! revalidate both signed packages and the protected policy on every recovery.
use amaterasu_package::{PackageResult, random_identifier, read_bounded};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_JOURNAL: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_policy: Option<Change>,
    pub bootstrap: Change,
    pub selection: Change,
    pub unit: Change,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    trial_started: bool,
    transition: Transition,
}

pub struct Paths<'a> {
    pub input_policy: &'a Path,
    pub journal: &'a Path,
    pub bootstrap: &'a Path,
    pub selection: &'a Path,
    pub unit: &'a Path,
}

fn changes<'a>(paths: &'a Paths<'a>, transition: &'a Transition) -> Vec<(&'a Path, &'a Change)> {
    let mut items = vec![
        (paths.bootstrap, &transition.bootstrap),
        (paths.selection, &transition.selection),
        (paths.unit, &transition.unit),
    ];
    if let Some(change) = &transition.input_policy {
        items.push((paths.input_policy, change));
    }
    items
}

fn check_size(transition: &Transition) -> PackageResult<()> {
    for change in [
        &transition.bootstrap,
        &transition.selection,
        &transition.unit,
    ]
    .into_iter()
    .chain(transition.input_policy.iter())
    {
        if change.before.len() > MAX_DOCUMENT
            || change.after.len() > MAX_DOCUMENT
            || change.before == change.after
        {
            return Err("invalid activation transition document".into());
        }
    }
    Ok(())
}

fn current_bounded(path: &Path, maximum: u64) -> PackageResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no arguments or pointer ownership requirements.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        return Err("activation target is not a private user-owned file".into());
    }
    read_bounded(path, maximum)
}

fn current(path: &Path) -> PackageResult<Vec<u8>> {
    current_bounded(path, MAX_DOCUMENT as u64)
}

fn replace(path: &Path, expected: &[u8], replacement: &[u8], maximum: u64) -> PackageResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || current_bounded(path, maximum)? != expected
    {
        return Err("activation target changed".into());
    }
    let parent = path.parent().ok_or("activation target has no parent")?;
    let temporary = parent.join(format!(".activation-write-{}", random_identifier()?));
    let result = (|| -> PackageResult<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let created = file.metadata().map_err(|e| e.to_string())?;
        if created.uid() != metadata.uid() || created.gid() != metadata.gid() {
            return Err("cannot preserve activation target ownership".into());
        }
        file.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o7777))
            .map_err(|e| e.to_string())?;
        file.write_all(replacement)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        if current_bounded(path, maximum)? != expected {
            return Err("activation target changed before replacement".into());
        }
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn remove_journal(path: &Path) -> PackageResult<()> {
    fs::remove_file(path).map_err(|e| e.to_string())?;
    fs::File::open(path.parent().ok_or("journal has no parent")?)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

/// Restore the old state after a partial transition. The authorization callback
/// must validate the journal against independent host authority, not the
/// journal itself. Returns true when a transition was recovered.
pub fn recover(
    paths: &Paths<'_>,
    authorize: &mut impl FnMut(&Transition) -> PackageResult<()>,
    reload: &mut impl FnMut() -> PackageResult<()>,
) -> PackageResult<bool> {
    let metadata = match fs::symlink_metadata(paths.journal) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    // SAFETY: geteuid has no arguments or pointer ownership requirements.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err("activation journal is not a private user-owned file".into());
    }
    let record: Journal = serde_json::from_slice(&read_bounded(paths.journal, MAX_JOURNAL)?)
        .map_err(|e| e.to_string())?;
    if record.schema != 1 {
        return Err("unsupported activation journal".into());
    }
    let journal = record.transition;
    check_size(&journal)?;
    authorize(&journal)?;
    // Verify all three identities before changing any one of them.
    for (path, change) in changes(paths, &journal) {
        let bytes = current(path)?;
        if bytes != change.before && bytes != change.after {
            return Err("activation recovery target changed".into());
        }
    }
    for (path, change) in changes(paths, &journal).into_iter().rev() {
        if current(path)? == change.after {
            replace(path, &change.after, &change.before, MAX_DOCUMENT as u64)?;
        }
    }
    reload()?;
    remove_journal(paths.journal)?;
    Ok(true)
}

/// Read an authorized fully written transition for the new service's first
/// health check. A partial transition cannot be treated as a live trial.
pub fn ready_for_trial(
    paths: &Paths<'_>,
    authorize: &mut impl FnMut(&Transition) -> PackageResult<()>,
) -> PackageResult<(Transition, bool)> {
    let metadata = fs::symlink_metadata(paths.journal).map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no arguments or pointer ownership requirements.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err("activation journal is not a private user-owned file".into());
    }
    let record: Journal = serde_json::from_slice(&read_bounded(paths.journal, MAX_JOURNAL)?)
        .map_err(|e| e.to_string())?;
    if record.schema != 1 {
        return Err("unsupported activation journal".into());
    }
    let journal = record.transition;
    check_size(&journal)?;
    authorize(&journal)?;
    for (path, change) in changes(paths, &journal) {
        if current(path)? != change.after {
            return Err("activation transition is not fully written".into());
        }
    }
    Ok((journal, record.trial_started))
}

/// Durably claim the single startup attempt before loading new components.
/// A crash after this claim causes the next startup to roll back.
pub fn claim_trial(
    paths: &Paths<'_>,
    authorize: &mut impl FnMut(&Transition) -> PackageResult<()>,
) -> PackageResult<()> {
    let before = read_bounded(paths.journal, MAX_JOURNAL)?;
    let (transition, started) = ready_for_trial(paths, authorize)?;
    if started {
        return Err("activation trial was already started".into());
    }
    let after = serde_json::to_vec(&Journal {
        schema: 1,
        trial_started: true,
        transition,
    })
    .map_err(|e| e.to_string())?;
    replace(paths.journal, &before, &after, MAX_JOURNAL)
}

/// Mark the trial healthy only after the new service actually loaded and
/// reported health. The caller must hold the host root lock.
pub fn finalize_trial(
    paths: &Paths<'_>,
    authorize: &mut impl FnMut(&Transition) -> PackageResult<()>,
) -> PackageResult<()> {
    let (_, started) = ready_for_trial(paths, authorize)?;
    if !started {
        return Err("activation trial has not started".into());
    }
    remove_journal(paths.journal)
}

/// Commit a previously trialled and independently authorized transition. A
/// crash before journal removal always rolls back on the next service start.
pub fn commit(
    paths: &Paths<'_>,
    transition: &Transition,
    authorize: &mut impl FnMut(&Transition) -> PackageResult<()>,
    reload: &mut impl FnMut() -> PackageResult<()>,
) -> PackageResult<()> {
    check_size(transition)?;
    authorize(transition)?;
    for (path, change) in changes(paths, transition) {
        if current(path)? != change.before {
            return Err("activation baseline changed".into());
        }
    }
    let bytes = serde_json::to_vec(&Journal {
        schema: 1,
        trial_started: false,
        transition: transition.clone(),
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err("activation journal too large".into());
    }
    let parent = paths.journal.parent().ok_or("journal has no parent")?;
    let temporary = parent.join(format!(".activation-journal-{}", random_identifier()?));
    let publish = (|| -> PackageResult<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        // Publishing by a non-replacing hard link ensures an interruption
        // cannot expose a partially written recovery journal.
        fs::hard_link(&temporary, paths.journal).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    publish?;
    let result = (|| {
        for (path, change) in changes(paths, transition) {
            replace(path, &change.before, &change.after, MAX_DOCUMENT as u64)?;
        }
        // Keep the journal until the new service has loaded and reported
        // health. A crash or failed startup must still be recoverable.
        reload()
    })();
    if let Err(error) = result {
        return match recover(paths, authorize, reload) {
            Ok(_) => Err(error),
            Err(recovery) => Err(format!(
                "activation failed: {error}; recovery required: {recovery}"
            )),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn change(before: &str, after: &str) -> Change {
        Change {
            before: before.as_bytes().into(),
            after: after.as_bytes().into(),
        }
    }
    #[test]
    fn durable_trial_claim_handles_a_large_bounded_journal() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-activation-large-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let bootstrap = root.join("bootstrap");
        let selection = root.join("selection");
        let unit = root.join("unit");
        let journal = root.join("journal");
        let input_policy = root.join("input-companion-policy.json");
        let paths = Paths {
            input_policy: &input_policy,
            journal: &journal,
            bootstrap: &bootstrap,
            selection: &selection,
            unit: &unit,
        };
        let transition = Transition {
            input_policy: Some(change("old-input", "new-input")),
            bootstrap: Change {
                before: vec![b'a'; 200_000],
                after: vec![b'b'; 200_000],
            },
            selection: change("old-set", "new-set"),
            unit: change("old-pin", "new-pin"),
        };
        for (path, item) in changes(&paths, &transition) {
            fs::write(path, &item.before).unwrap();
        }
        let mut authorize = |_: &Transition| Ok(());
        commit(&paths, &transition, &mut authorize, &mut || Ok(())).unwrap();
        assert!(fs::metadata(&journal).unwrap().len() > MAX_DOCUMENT as u64);
        claim_trial(&paths, &mut authorize).unwrap();
        finalize_trial(&paths, &mut authorize).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_and_interrupted_recovery_preserve_exact_state() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-activation-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let bootstrap = root.join("bootstrap");
        let selection = root.join("selection");
        let unit = root.join("unit");
        let journal = root.join("journal");
        let input_policy = root.join("input-companion-policy.json");
        let paths = Paths {
            input_policy: &input_policy,
            journal: &journal,
            bootstrap: &bootstrap,
            selection: &selection,
            unit: &unit,
        };
        let transition = Transition {
            input_policy: Some(change("old-input", "new-input")),
            bootstrap: change("old-policy", "new-policy"),
            selection: change("old-set", "new-set"),
            unit: change("old-pin", "new-pin"),
        };
        for (path, item) in changes(&paths, &transition) {
            fs::write(path, &item.before).unwrap();
        }
        let mut authorize = |candidate: &Transition| {
            if candidate.bootstrap.after == b"new-policy"
                && candidate.selection.after == b"new-set"
                && candidate.unit.after == b"new-pin"
            {
                Ok(())
            } else {
                Err("unauthorized".into())
            }
        };
        let mut reload = || Ok(());
        commit(&paths, &transition, &mut authorize, &mut reload).unwrap();
        assert!(journal.exists());
        assert!(finalize_trial(&paths, &mut authorize).is_err());
        claim_trial(&paths, &mut authorize).unwrap();
        assert!(claim_trial(&paths, &mut authorize).is_err());
        // A crash after the durable claim must restore the previous package.
        assert!(recover(&paths, &mut authorize, &mut reload).unwrap());
        for (path, item) in changes(&paths, &transition) {
            assert_eq!(fs::read(path).unwrap(), item.before);
        }
        commit(&paths, &transition, &mut authorize, &mut reload).unwrap();
        claim_trial(&paths, &mut authorize).unwrap();
        finalize_trial(&paths, &mut authorize).unwrap();
        assert!(!journal.exists());
        for (path, item) in changes(&paths, &transition) {
            assert_eq!(fs::read(path).unwrap(), item.after);
            fs::write(path, &item.before).unwrap();
        }
        fs::write(
            &journal,
            serde_json::to_vec(&Journal {
                schema: 1,
                trial_started: false,
                transition: transition.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&bootstrap, &transition.bootstrap.after).unwrap();
        assert!(recover(&paths, &mut authorize, &mut reload).unwrap());
        for (path, item) in changes(&paths, &transition) {
            assert_eq!(fs::read(path).unwrap(), item.before);
        }
        assert!(!journal.exists());
        fs::write(
            &journal,
            serde_json::to_vec(&Journal {
                schema: 1,
                trial_started: false,
                transition: transition.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&selection, b"unrelated edit").unwrap();
        assert!(recover(&paths, &mut authorize, &mut reload).is_err());
        assert_eq!(fs::read(&selection).unwrap(), b"unrelated edit");
        assert!(journal.exists());
        fs::remove_file(&journal).unwrap();
        fs::write(&selection, &transition.selection.before).unwrap();
        let mut reload_calls = 0;
        let mut fail_once = || {
            reload_calls += 1;
            if reload_calls == 1 {
                Err("reload failed".into())
            } else {
                Ok(())
            }
        };
        assert!(commit(&paths, &transition, &mut authorize, &mut fail_once).is_err());
        assert_eq!(reload_calls, 2);
        for (path, item) in changes(&paths, &transition) {
            assert_eq!(fs::read(path).unwrap(), item.before);
        }
        assert!(!journal.exists());
        let mut forged = transition.clone();
        forged.unit.after = b"forged-pin".to_vec();
        fs::write(
            &journal,
            serde_json::to_vec(&Journal {
                schema: 1,
                trial_started: false,
                transition: forged,
            })
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(recover(&paths, &mut authorize, &mut reload).is_err());
        for (path, item) in changes(&paths, &transition) {
            assert_eq!(fs::read(path).unwrap(), item.before);
        }
        assert!(journal.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
