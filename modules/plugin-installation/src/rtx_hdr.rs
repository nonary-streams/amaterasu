//! Stage NVIDIA's own pinned runtime after the official add-on was verified.
use amaterasu_module_sdk::network::Network;
use amaterasu_package::{
    BootstrapConfig, PackageResult, RootLock, plugins::rtx_hdr::runtime_parent,
};
use amaterasu_rtx_runtime::{DIRECTORY, RemoteWheel};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
    time::Duration,
};

pub(super) fn ensure_runtime(
    bootstrap: &BootstrapConfig,
    root: &Path,
    network: Network,
    cancelled: Arc<AtomicBool>,
) -> PackageResult<()> {
    let parent = runtime_parent(bootstrap)?;
    if parent != root.join("data/state/rtx-hdr") {
        return Err("rtx_hdr_runtime_location_unavailable".into());
    }
    // Use a separate lock directory: the runtime verifier rejects unexpected
    // entries inside the installed runtime itself.
    let locks = root.join("data/state/plugin-installation/rtx-runtime-lock");
    amaterasu_package::create_private_directory(&locks)?;
    let _lock = RootLock::acquire(&locks)?;
    if parent
        .join(DIRECTORY)
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        return amaterasu_rtx_runtime::verify(parent, &cancelled)
            .map(|_| ())
            .map_err(|error| format!("rtx_hdr_runtime_verification_failed: {error}"));
    }
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(600)))
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .max_response_header_size(8192)
        .build();
    let agent =
        amaterasu_module_sdk::network_http::agent_cancellable(config, network, cancelled.clone());
    let download = RemoteWheel::open(&agent, cancelled.clone(), Arc::new(AtomicU64::new(0)))
        .and_then(|wheel| amaterasu_rtx_runtime::install(wheel, parent, &cancelled));
    download
        .map(|_| ())
        .map_err(|error| format!("rtx_hdr_runtime_install_failed: {error}"))
}
