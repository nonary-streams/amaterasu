//! Session 0 supervision of a user worker, with explicit cleanup on every exit.
use amaterasu_contracts::HostLifecycleAction;
use amaterasu_package::{PackageResult, RootLock};
use std::path::Path;
use std::time::Duration;

use super::{SERVICE_READY_FILE, publish_service_readiness, remove_service_readiness};

pub(crate) fn run_interactive_service(
    root: &Path,
    mut stop_requested: impl FnMut() -> bool,
    mut session_changed: impl FnMut() -> bool,
    on_started: impl FnOnce(),
) -> PackageResult<()> {
    let _lock = RootLock::acquire(root)?;
    let digest = crate::supervisor::select_active(root)?;
    let mut supervisor = crate::interactive_worker::Supervisor::new(root, &digest)?;
    let result = (|| {
        supervisor.refresh(true, &mut stop_requested, || remove_service_readiness(root))?;
        if stop_requested() {
            return Ok(());
        }
        if !supervisor.has_ready_worker()? {
            // A running SCM shell without a verified user worker cannot satisfy
            // the installation's stream-capable readiness gate.
            return Err("no active interactive worker is ready".into());
        }
        publish_service_readiness(root, &digest)?;
        on_started();
        while !stop_requested() {
            if supervisor.refresh(session_changed(), &mut stop_requested, || {
                remove_service_readiness(root)
            })? == Some(HostLifecycleAction::Quit)
            {
                break;
            }
            if stop_requested() || !supervisor.has_ready_worker()? {
                remove_service_readiness(root);
            } else if !root
                .join(SERVICE_READY_FILE)
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                publish_service_readiness(root, &digest)?;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    })();
    // Clear admission before a potentially slow drain, including failures in
    // WTS enumeration, policy authorization, readiness, or worker protocol.
    remove_service_readiness(root);
    crate::worker_shutdown::finish_with_cleanup(result, || supervisor.shutdown())
}
