//! A stalled replacement network provider must retire before the installer unloads.
use super::*;
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, NativeInterface, STATUS_OK, Status};
use amaterasu_module_sdk::network::abi::*;
use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Calls {
    entered: AtomicBool,
    finished: AtomicBool,
    released: AtomicBool,
    timeout_ms: AtomicU32,
}

struct Provider {
    table: NetworkApi,
    calls: Arc<Calls>,
}

macro_rules! unsupported {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?)) => {
        unsafe extern "C" fn $name($($arg: $ty),*) -> Status { UNSUPPORTED }
    };
}
unsupported!(
    interfaces(_ctx: *mut c_void, _out: *mut InterfaceAddress, _cap: usize, _n: *mut usize)
);
unsupported!(listen(_ctx: *mut c_void, _addr: Address, _opt: ListenOptions, _out: *mut u64));
unsupported!(connect(_ctx: *mut c_void, _addr: Address, _timeout: u32, _out: *mut u64));
unsupported!(udp_bind(_ctx: *mut c_void, _addr: Address, _out: *mut u64));
unsupported!(accept(_ctx: *mut c_void, _handle: u64, _out: *mut u64, _addr: *mut Address));
unsupported!(address(_ctx: *mut c_void, _handle: u64, _out: *mut Address));
unsupported!(read(_ctx: *mut c_void, _handle: u64, _buf: *mut u8, _len: usize, _n: *mut usize));
unsupported!(write(_ctx: *mut c_void, _handle: u64, _buf: *const u8, _len: usize, _n: *mut usize));
unsupported!(
    recv_from(
        _ctx: *mut c_void,
        _handle: u64,
        _buf: *mut u8,
        _len: usize,
        _n: *mut usize,
        _addr: *mut Address,
    )
);
unsupported!(
    send_to(
        _ctx: *mut c_void,
        _handle: u64,
        _buf: *const u8,
        _len: usize,
        _addr: Address,
        _n: *mut usize,
    )
);
unsupported!(connect_udp(_ctx: *mut c_void, _handle: u64, _addr: Address));
unsupported!(option(_ctx: *mut c_void, _handle: u64, _opt: u32, _value: u64));
unsupported!(get_option(_ctx: *mut c_void, _handle: u64, _opt: u32, _out: *mut u64));
unsupported!(multicast(_ctx: *mut c_void, _handle: u64, _addr: Address, _if: u32, _join: bool));
unsupported!(
    batch(_ctx: *mut c_void, _handle: u64, _data: *const Datagram, _len: usize, _n: *mut usize)
);
unsupported!(shutdown(_ctx: *mut c_void, _handle: u64, _how: u32));
unsupported!(close(_ctx: *mut c_void, _handle: u64));

unsafe extern "C" fn capabilities(_ctx: *mut c_void, out: *mut Capabilities) -> Status {
    // SAFETY: the SDK passes a valid output descriptor.
    unsafe {
        *out = Capabilities {
            size: size_of::<Capabilities>() as u32,
            version: 1,
            features: TCP | DNS,
            max_io_bytes: 64 * 1024,
            max_handles: 4,
            max_batch: 0,
        };
    }
    STATUS_OK
}

unsafe extern "C" fn resolve(
    ctx: *mut c_void,
    _name: BorrowedBytes,
    _port: u16,
    timeout: u32,
    _out: *mut Address,
    _cap: usize,
    _count: *mut usize,
) -> Status {
    // SAFETY: the SDK retains this provider until the active callback returns.
    let provider = unsafe { &*ctx.cast::<Provider>() };
    provider.calls.timeout_ms.store(timeout, Ordering::Release);
    provider.calls.entered.store(true, Ordering::Release);
    // Simulate a provider waiting the entire declared DNS deadline.
    std::thread::sleep(Duration::from_millis(u64::from(timeout)));
    provider.calls.finished.store(true, Ordering::Release);
    TIMED_OUT
}
unsafe extern "C" fn release(ctx: *mut c_void) {
    // SAFETY: Network calls release once, after all provider callbacks complete.
    let provider = unsafe { Box::from_raw(ctx.cast::<Provider>()) };
    provider.calls.released.store(true, Ordering::Release);
}
fn network(calls: Arc<Calls>) -> Network {
    let mut provider = Box::new(Provider {
        table: NetworkApi {
            base: NativeInterface {
                header: AbiHeader {
                    size: size_of::<NetworkApi>() as u32,
                    major: MAJOR,
                    minor: MINOR,
                },
                context: std::ptr::null_mut(),
                release: Some(release),
            },
            capabilities,
            resolve,
            interfaces,
            tcp_listen: listen,
            tcp_connect: connect,
            udp_bind,
            accept,
            local_addr: address,
            peer_addr: address,
            read,
            peek: read,
            write,
            recv_from,
            send_to,
            connect_udp,
            set_option: option,
            get_option,
            join_multicast: multicast,
            send_batch: batch,
            shutdown,
            close,
        },
        calls,
    });
    provider.table.base.context = (&mut *provider as *mut Provider).cast();
    let provider = Box::into_raw(provider);
    // SAFETY: this table is complete, thread-safe, and releases its owning box.
    unsafe { Network::from_native_table(&(*provider).table).unwrap() }
}

#[test]
fn cancelled_install_waits_for_stalled_provider_and_releases_it() {
    let calls = Arc::new(Calls::default());
    let mut installer = Installer::new(
        PathBuf::from("/tmp/rtx-stalled-test-unused"),
        network(calls.clone()),
    )
    .unwrap();
    assert_eq!(installer.request("rtx_hdr_install")["phase"], "downloading");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !calls.entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "network resolver never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    let timeout = calls.timeout_ms.load(Ordering::Acquire);
    assert!(
        (1..=10_000).contains(&timeout),
        "resolver received unexpected timeout {timeout}"
    );
    installer.request("rtx_hdr_cancel");
    assert_eq!(
        installer.request("rtx_hdr_install")["code"],
        "installation_in_progress"
    );
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        drop(installer);
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(100)).is_err(),
        "installer retired before the stalled callback"
    );
    rx.recv_timeout(Duration::from_secs(12))
        .expect("installer did not retire after provider timeout");
    assert!(calls.finished.load(Ordering::Acquire));
    assert!(calls.released.load(Ordering::Acquire));
}
