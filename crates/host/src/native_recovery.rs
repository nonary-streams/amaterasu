//! Recovery of a retained native set is owned by the fixed host shell, not a
//! replaceable management or feature library. No candidate code is loaded here.
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, RootLock, read_json, write_json_atomically,
};
use std::path::Path;

pub fn restore(root: &Path, digest: Option<&str>) -> PackageResult<()> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    if !bootstrap.development {
        return Err("production native recovery requires installed-service authorization".into());
    }
    let path = root.join("selection.json");
    let selection: ActivationState = read_json(&path)?;
    let previous = match digest {
        Some(digest) => digest.to_owned(),
        None => selection.previous.clone().ok_or("no previous native set")?,
    };
    crate::publisher_selection::with_verified_selected(
        root,
        &previous,
        &bootstrap,
        crate::publisher_selection::SelectionPurpose::ExplicitRecovery,
        &mut |_, _| Ok(()),
        |verified| {
            let _lock = RootLock::acquire(root)?;
            let mut selection: ActivationState = read_json(&path)?;
            if digest.is_none() && selection.previous.as_deref() != Some(&previous) {
                return Err("previous native set changed before recovery".into());
            }
            if selection.active.as_deref() == Some(&previous) {
                return Err("requested native set is already active".into());
            }
            if selection.previous.as_deref() != Some(&previous)
                && !selection.healthy_sets.contains(&previous)
            {
                return Err("native recovery requires a previously healthy selected set".into());
            }
            if !verified.signed || verified.digest != previous {
                return Err("previous native set is not authenticated".into());
            }
            let old_active = selection.active.replace(previous.clone());
            selection.previous = old_active;
            selection.pending = None;
            selection.trial = None;
            write_json_atomically(&path, &selection)
        },
    )?;
    println!(
        "Restored signed native set {previous}; accepted release floor remains intact. Its native code is authorized to execute under the user account at the next worker start."
    );
    Ok(())
}
