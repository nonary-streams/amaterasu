//! Windows Service Control Manager boundary for the development host.
//!
//! The service shell only starts the verified development lifecycle. It has no
//! stdin command channel and deliberately reports no root, package, or runtime
//! error details through the SCM boundary.
use amaterasu_package::PackageResult;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: u32 = 1063;
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;
const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
const SERVICE_ACCEPT_SESSIONCHANGE: u32 = 0x0000_0080;
const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;
const SERVICE_CONTROL_SESSIONCHANGE: u32 = 0x0000_000E;
const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
const SERVICE_STOPPED: u32 = 0x0000_0001;
const SERVICE_START_PENDING: u32 = 0x0000_0002;
const SERVICE_STOP_PENDING: u32 = 0x0000_0003;
const SERVICE_RUNNING: u32 = 0x0000_0004;

static SERVICE_CONTEXT: OnceLock<ServiceContext> = OnceLock::new();
static SERVICE_NAME: [u16; 10] = [65, 109, 97, 116, 101, 114, 97, 115, 117, 0];

struct ServiceContext {
    root: PathBuf,
    stop_requested: AtomicBool,
    session_changed: AtomicBool,
    status_handle: AtomicUsize,
}

#[repr(C)]
struct ServiceTableEntry {
    name: *mut u16,
    service_main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}

#[repr(C)]
struct ServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
    service_specific_exit_code: u32,
    check_point: u32,
    wait_hint: u32,
}

type ServiceStatusHandle = *mut c_void;

unsafe extern "system" {
    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        service_name: *const u16,
        handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
        context: *mut c_void,
    ) -> ServiceStatusHandle;
    fn SetServiceStatus(status_handle: ServiceStatusHandle, status: *const ServiceStatus) -> i32;
    fn GetLastError() -> u32;
}

pub(crate) fn run(root: &Path) -> PackageResult<()> {
    SERVICE_CONTEXT
        .set(ServiceContext {
            root: root.to_path_buf(),
            stop_requested: AtomicBool::new(false),
            session_changed: AtomicBool::new(true),
            status_handle: AtomicUsize::new(0),
        })
        .map_err(|_| "Windows service dispatcher was already initialized")?;
    let table = [
        ServiceTableEntry {
            // SAFETY: SERVICE_NAME is a static NUL-terminated UTF-16 string and
            // the SCM only reads it while StartServiceCtrlDispatcherW is active.
            name: SERVICE_NAME.as_ptr().cast_mut(),
            service_main: Some(service_main),
        },
        ServiceTableEntry {
            name: std::ptr::null_mut(),
            service_main: None,
        },
    ];
    // SAFETY: table remains alive until the dispatcher returns; both entries use
    // the ABI layouts declared above and the callback has static lifetime.
    let started = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if started != 0 {
        return Ok(());
    }
    // SAFETY: GetLastError reads the calling thread's Win32 error slot.
    let error = unsafe { GetLastError() };
    if error == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
        return crate::service::run_controlled_service(root);
    }
    Err(format!(
        "Windows service dispatcher failed with error {error}"
    ))
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Some(context) = SERVICE_CONTEXT.get() else {
        return;
    };
    // SAFETY: SERVICE_NAME and `context` are static for this dispatcher's full
    // lifetime. The handler merely flips the AtomicBool owned by that context.
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            SERVICE_NAME.as_ptr(),
            Some(service_control_handler),
            (context as *const ServiceContext).cast_mut().cast(),
        )
    };
    if status_handle.is_null() {
        return;
    }
    context
        .status_handle
        .store(status_handle as usize, Ordering::Release);
    report_status(status_handle, SERVICE_START_PENDING, 0, 0);
    let result = crate::service::run_interactive_service(
        &context.root,
        || context.stop_requested.load(Ordering::Acquire),
        || context.session_changed.swap(false, Ordering::AcqRel),
        || report_status(status_handle, SERVICE_RUNNING, 0, 0),
    );
    if result.is_ok() {
        report_status(status_handle, SERVICE_STOPPED, 0, 0);
    } else {
        report_status(
            status_handle,
            SERVICE_STOPPED,
            0,
            ERROR_SERVICE_SPECIFIC_ERROR,
        );
    }
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _: u32,
    _: *mut c_void,
    context: *mut c_void,
) -> u32 {
    if control == SERVICE_CONTROL_SESSIONCHANGE {
        // The event payload is intentionally ignored: it contains a session
        // identifier supplied by SCM. The supervisor re-enumerates active WTS
        // sessions before it creates or replaces a user-token worker.
        let context = unsafe { &*(context.cast::<ServiceContext>()) };
        context.session_changed.store(true, Ordering::Release);
        return 0;
    }
    if !matches!(control, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
        return 0;
    }
    // SAFETY: RegisterServiceCtrlHandlerExW receives this pointer from the
    // static ServiceContext above. It remains valid until service_main returns.
    let context = unsafe { &*(context.cast::<ServiceContext>()) };
    context.stop_requested.store(true, Ordering::Release);
    let status_handle = context.status_handle.load(Ordering::Acquire) as ServiceStatusHandle;
    if !status_handle.is_null() {
        report_status(status_handle, SERVICE_STOP_PENDING, 0, 0);
    }
    0
}

fn report_status(
    status_handle: ServiceStatusHandle,
    state: u32,
    controls_accepted: u32,
    win32_exit_code: u32,
) {
    let controls_accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
    } else {
        controls_accepted
    };
    let status = ServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: state,
        controls_accepted,
        win32_exit_code,
        service_specific_exit_code: u32::from(win32_exit_code != 0),
        check_point: 0,
        wait_hint: 5_000,
    };
    // SAFETY: `status` remains valid for the synchronous API call and the
    // handle came from RegisterServiceCtrlHandlerExW in this service process.
    unsafe {
        let _ = SetServiceStatus(status_handle, &status);
    }
}
