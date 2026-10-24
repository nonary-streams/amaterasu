use super::{Backend, SourceSession};
use crate::{
    buffers::{Mailbox, Plane},
    model::{
        Availability, CaptureResult, PipeWireState, SourceDiagnostics, SourceStage, SourceState,
        SourceType,
    },
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::Arc,
};

#[derive(Default)]
#[repr(C)]
struct Probe {
    available: u32,
    version: u32,
    source_types: u32,
    cursor_modes: u32,
}
#[derive(Default)]
#[repr(C)]
struct Snapshot {
    state: u32,
    stage: u32,
    node: u32,
    width: u32,
    height: u32,
    format: u32,
    error: u32,
    pipewire_state: i32,
    node_present: u32,
    serial_present: u32,
    pipewire_error: i32,
    serial: u64,
}

unsafe extern "C" {
    fn am_pw_probe(output: *mut Probe) -> i32;
    fn am_pw_start(
        source_type: u32,
        timeout: u32,
        callback: unsafe extern "C" fn(*mut c_void, *const Plane, u32, u32, u32, u64) -> u32,
        context: *mut c_void,
    ) -> *mut c_void;
    fn am_pw_snapshot(session: *mut c_void, output: *mut Snapshot);
    fn am_pw_destroy(session: *mut c_void);
}

pub struct NativeBackend {
    availability: Availability,
}

impl NativeBackend {
    pub fn new() -> Self {
        let mut probe = Probe::default();
        // SAFETY: private adapter writes only this live, fixed-layout probe slot.
        unsafe { am_pw_probe(&mut probe) };
        Self {
            availability: Availability {
                native_supported: true,
                available: probe.available != 0,
                portal_version: probe.version,
                source_types: probe.source_types,
                cursor_modes: probe.cursor_modes,
                reason: (probe.available == 0).then_some(
                    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
                        "session_bus_unavailable"
                    } else {
                        "portal_unavailable"
                    },
                ),
            },
        }
    }
}

impl Backend for NativeBackend {
    fn availability(&self) -> Availability {
        self.availability
    }

    fn select(&self, source: SourceType, timeout: u32) -> CaptureResult<Box<dyn SourceSession>> {
        let mailbox = Arc::new(Mailbox::default());
        let context = Arc::as_ptr(&mailbox).cast_mut().cast();
        // SAFETY: Arc's allocation has a stable address until session Drop
        // cancels and joins the sole C worker before destroying its mailbox.
        let native =
            NonNull::new(unsafe { am_pw_start(source.mask(), timeout, frame_callback, context) })
                .ok_or("native_worker_unavailable")?;
        Ok(Box::new(NativeSession { native, mailbox }))
    }
}

struct NativeSession {
    native: NonNull<c_void>,
    mailbox: Arc<Mailbox>,
}
// SAFETY: adapter snapshots/cancellation use atomics and a thread-safe GCancellable;
// only its dedicated worker accesses GIO/PipeWire resources. Drop joins that worker.
unsafe impl Send for NativeSession {}

impl SourceSession for NativeSession {
    fn state(&self) -> SourceState {
        let mut snapshot = Snapshot::default();
        // SAFETY: this opaque session lives until Drop; output is aligned and writable.
        unsafe { am_pw_snapshot(self.native.as_ptr(), &mut snapshot) };
        match snapshot.state {
            1 => SourceState::Selecting,
            2 if self.mailbox.has_frame() => SourceState::Ready {
                node: snapshot.node,
                width: snapshot.width,
                height: snapshot.height,
            },
            2 => SourceState::Selecting,
            3 => SourceState::Denied,
            5 => SourceState::Revoked,
            _ => SourceState::Failed(match snapshot.error {
                1 => "session_bus_unavailable",
                2 => "portal_request_failed",
                3 => "invalid_portal_source",
                4 => "pipewire_failed",
                5 => "unsupported_cpu_buffer",
                6 => "source_mode_changed",
                7 => "source_selection_timeout",
                _ => "capture_failed",
            }),
        }
    }
    fn diagnostics(&self) -> SourceDiagnostics {
        let mut snapshot = Snapshot::default();
        // SAFETY: this opaque session lives until Drop; output is aligned and writable.
        unsafe { am_pw_snapshot(self.native.as_ptr(), &mut snapshot) };
        SourceDiagnostics {
            stage: SourceStage::from_native(snapshot.stage),
            pipewire_node: (snapshot.node_present != 0).then_some(snapshot.node),
            pipewire_serial: (snapshot.serial_present != 0).then_some(snapshot.serial),
            pipewire_state: PipeWireState::from_native(snapshot.pipewire_state),
            pipewire_error: (snapshot.pipewire_error != 0).then_some(snapshot.pipewire_error),
            adapter_error_code: (snapshot.error != 0).then_some(snapshot.error),
        }
    }
    fn mailbox(&self) -> Arc<Mailbox> {
        self.mailbox.clone()
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        // SAFETY: uniquely owns this session. Destroy cancels I/O, tears down the
        // portal and PipeWire resources and joins before callback storage is dropped.
        unsafe { am_pw_destroy(self.native.as_ptr()) };
    }
}

unsafe extern "C" fn frame_callback(
    context: *mut c_void,
    plane: *const Plane,
    width: u32,
    height: u32,
    format: u32,
    timestamp_us: u64,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || plane.is_null() {
            return 2;
        }
        // SAFETY: adapter retains the Arc allocation and dequeued mapped SPA buffer
        // through this call. Rust validates the descriptor before copying rows.
        let mailbox = unsafe { &*context.cast::<Mailbox>() };
        unsafe { mailbox.publish(&*plane, width, height, format, timestamp_us) }
    }))
    .unwrap_or(2)
}

#[cfg(test)]
mod tests;
