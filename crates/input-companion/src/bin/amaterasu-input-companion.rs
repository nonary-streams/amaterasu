//! Opt-in, separately installed privileged companion service. No driver or
//! service installation is performed by this executable.
#[cfg(windows)]
mod windows {
    use amaterasu_input_companion::{run_installed_service, wake_listener};
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    const SERVICE_ACCEPT_STOP: u32 = 1;
    const SERVICE_ACCEPT_SHUTDOWN: u32 = 4;
    const SERVICE_CONTROL_STOP: u32 = 1;
    const SERVICE_CONTROL_SHUTDOWN: u32 = 5;
    const SERVICE_WIN32_OWN_PROCESS: u32 = 0x10;
    const SERVICE_STOPPED: u32 = 1;
    const SERVICE_START_PENDING: u32 = 2;
    const SERVICE_STOP_PENDING: u32 = 3;
    const SERVICE_RUNNING: u32 = 4;
    const SERVICE_SPECIFIC_ERROR: u32 = 1066;

    static CONTEXT: OnceLock<ServiceContext> = OnceLock::new();
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();

    struct ServiceContext {
        root: PathBuf,
        stop: Arc<AtomicBool>,
        status_handle: AtomicUsize,
    }

    #[repr(C)]
    struct ServiceTableEntry {
        name: *mut u16,
        main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
    }

    #[repr(C)]
    struct ServiceStatus {
        service_type: u32,
        state: u32,
        accepted_controls: u32,
        win32_exit_code: u32,
        service_specific_exit_code: u32,
        checkpoint: u32,
        wait_hint: u32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
        fn RegisterServiceCtrlHandlerExW(
            name: *const u16,
            handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
            context: *mut c_void,
        ) -> *mut c_void;
        fn SetServiceStatus(handle: *mut c_void, status: *const ServiceStatus) -> i32;
    }

    pub(super) fn run() -> Result<(), String> {
        let mut args = std::env::args_os();
        let _executable = args.next();
        let root = args.next().ok_or("usage: amaterasu-input-companion ROOT")?;
        if args.next().is_some() {
            return Err("usage: amaterasu-input-companion ROOT".into());
        }
        CONTEXT
            .set(ServiceContext {
                root: root.into(),
                stop: Arc::new(AtomicBool::new(false)),
                status_handle: AtomicUsize::new(0),
            })
            .map_err(|_| "companion service already initialized")?;
        let name = NAME.get_or_init(|| {
            "AmaterasuInputCompanion"
                .encode_utf16()
                .chain(Some(0))
                .collect()
        });
        let table = [
            ServiceTableEntry {
                // SAFETY: static OnceLock storage lives throughout dispatch.
                name: name.as_ptr().cast_mut(),
                main: Some(service_main),
            },
            ServiceTableEntry {
                name: std::ptr::null_mut(),
                main: None,
            },
        ];
        // SAFETY: the table and callbacks remain valid while SCM dispatches.
        if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
            return Err("companion must be started by the Service Control Manager".into());
        }
        Ok(())
    }

    unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
        let Some(context) = CONTEXT.get() else {
            return;
        };
        let name = NAME.get().expect("service name initialized");
        // SAFETY: callback context is static for the full service lifetime.
        let handle = unsafe {
            RegisterServiceCtrlHandlerExW(
                name.as_ptr(),
                Some(control),
                (context as *const ServiceContext).cast_mut().cast(),
            )
        };
        if handle.is_null() {
            return;
        }
        context
            .status_handle
            .store(handle as usize, Ordering::Release);
        report(context, SERVICE_START_PENDING, false);
        let result = run_installed_service(&context.root, Arc::clone(&context.stop), || {
            report(context, SERVICE_RUNNING, false);
        });
        report(context, SERVICE_STOPPED, result.is_err());
    }

    unsafe extern "system" fn control(
        command: u32,
        _: u32,
        _: *mut c_void,
        context: *mut c_void,
    ) -> u32 {
        if matches!(command, SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN) {
            // SAFETY: SCM receives the static context pointer registered above.
            let context = unsafe { &*(context.cast::<ServiceContext>()) };
            context.stop.store(true, Ordering::Release);
            report(context, SERVICE_STOP_PENDING, false);
            wake_listener();
        }
        0
    }

    fn report(context: &ServiceContext, state: u32, failed: bool) {
        let handle = context.status_handle.load(Ordering::Acquire) as *mut c_void;
        if handle.is_null() {
            return;
        }
        let status = ServiceStatus {
            service_type: SERVICE_WIN32_OWN_PROCESS,
            state,
            accepted_controls: if state == SERVICE_RUNNING {
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
            } else {
                0
            },
            win32_exit_code: if failed { SERVICE_SPECIFIC_ERROR } else { 0 },
            service_specific_exit_code: u32::from(failed),
            checkpoint: 0,
            wait_hint: 5_000,
        };
        // SAFETY: status is live for this synchronous SCM call.
        unsafe { SetServiceStatus(handle, &status) };
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() {
        eprintln!("amaterasu-input-companion: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("amaterasu-input-companion is Windows-only");
    std::process::exit(1);
}
