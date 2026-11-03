//! Explicit real-network test; no sockets are opened by the runtime installer.
#![cfg(target_os = "linux")]
use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeInterfaceQuery, STATUS_OK};
use amaterasu_module_sdk::{
    network::{self, Network},
    network_http,
};
use amaterasu_rtx_runtime::{RemoteWheel, install, verify};
use std::{
    os::unix::fs::DirBuilderExt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[test]
#[ignore = "downloads the pinned NVIDIA wheel ranges through a real Network SDK provider"]
fn nvidia_wheel_installs_with_the_network_provider() {
    let factory = amaterasu_network_sockets::Factory::new();
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: network::abi::MAJOR,
        min_minor: 0,
        min_size: size_of::<network::abi::NetworkApi>() as u32,
    };
    let mut table = std::ptr::null();
    // SAFETY: factory stays alive for bind and table owns a provider reference.
    assert_eq!(
        unsafe {
            (factory.api.bind.unwrap())(
                factory.api.context,
                BorrowedBytes::new(b"rtx.test"),
                &query,
                &mut table,
            )
        },
        STATUS_OK
    );
    // SAFETY: bind returned an owned, thread-safe matching NetworkApi.
    let provider = unsafe { Network::from_native_table(table.cast()) }.unwrap();
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(600)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .build();
    let agent = network_http::agent(config, provider);
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let reader = RemoteWheel::open(&agent, cancel.clone(), progress.clone()).unwrap();
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).unwrap();
    let root = std::path::PathBuf::from(
        std::env::var_os("AMATERASU_RTX_TEST_ROOT").expect("test root on worktree volume"),
    )
    .join(format!(
        "amaterasu-rtx-live-{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let result = install(reader, &root, &cancel).and_then(|path| {
        assert_eq!(verify(&root, &cancel)?, path);
        Ok(())
    });
    eprintln!(
        "NVIDIA wheel transferred {} bytes",
        progress.load(Ordering::Relaxed)
    );
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}
