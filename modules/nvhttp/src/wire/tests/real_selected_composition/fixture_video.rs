//! A device-free native video provider for the authenticated PLAY qualification.
//! It produces no frames; ownership and ABI contracts remain real.
use amaterasu_media::{MEDIA_PENDING, PacketSink, VideoApi};
use amaterasu_module_sdk::{Command, RuntimeClient, abi::*};
use serde_json::json;
use std::ffi::c_void;

pub(super) struct Video {
    factory: Box<NativeServiceFactory>,
    live: bool,
}
impl Video {
    pub fn start(runtime: &RuntimeClient) -> Self {
        let factory = Box::new(NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: std::ptr::null_mut(),
            bind: Some(bind),
        });
        runtime
            .require_success(Command::Register {
                service: "amaterasu.video/1".into(),
            })
            .unwrap();
        // SAFETY: this owned factory and its static callback code outlive all
        // consumer bindings; every successful binding owns its own table.
        unsafe {
            amaterasu_module_sdk::video::register(runtime, "amaterasu.video/1", &factory).unwrap()
        };
        Self {
            factory,
            live: false,
        }
    }
    pub fn pump(&mut self, runtime: &RuntimeClient) {
        while let Some(request) = runtime.poll_serialized().unwrap() {
            let payload = match request.payload["action"].as_str().unwrap_or("") {
                "open" => {
                    self.live = true;
                    json!({"ok":true,"stream":77,"state":"running","packet_boundaries":true})
                }
                "lookup" if self.live => {
                    json!({"ok":true,"stream":77,"state":"running","packet_boundaries":true})
                }
                "lookup" => json!({"ok":false,"code":"not_found"}),
                "observe_capture" if self.live => {
                    json!({"ok":true,"stream":77,"observed_capture":{"protocol":"wayland-wl-output","registry_global":4,"output_name":"Virtual-Fixture","width":1280,"height":720,"refresh_millihz":60000}})
                }
                "status" if self.live => json!({"ok":true,"stream":77,"state":"running"}),
                "drain" => json!({"ok":true,"stream":77,"state":"drained"}),
                "close" => {
                    self.live = false;
                    json!({"ok":true,"stream":77,"state":"closed"})
                }
                other => panic!("unexpected qualification video operation: {other}"),
            };
            runtime
                .require_success(Command::Complete {
                    request: request.id,
                    payload,
                })
                .unwrap();
        }
        let _ = &self.factory;
    }
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: each successful bind creates exactly one Box whose pointer is the
    // returned context. The runtime calls release once after the final native use.
    drop(unsafe { Box::from_raw(context.cast::<VideoApi>()) });
}
unsafe extern "C" fn receive(_context: *mut c_void, _stream: u64, _sink: PacketSink) -> Status {
    MEDIA_PENDING
}
unsafe extern "C" fn bind(
    _context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    out: *mut *const NativeInterface,
) -> Status {
    if query.is_null() || out.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the runtime lends initialized descriptors and principal bytes for
    // this synchronous call; no borrowed pointer is retained.
    let Ok(principal) = (unsafe { principal.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let query = unsafe { &*query };
    if principal != b"stream"
        || !query.header.supports::<NativeInterfaceQuery>()
        || query.interface_major != 1
        || query.min_minor != 0
        || query.min_size as usize > size_of::<VideoApi>()
    {
        return STATUS_INVALID_ARGUMENT;
    }
    let mut table = Box::new(VideoApi {
        base: NativeInterface {
            header: AbiHeader::of::<VideoApi>(),
            context: std::ptr::null_mut(),
            release: Some(release),
        },
        receive,
    });
    table.base.context = (&mut *table as *mut VideoApi).cast();
    // SAFETY: ownership of the boxed table transfers to the runtime binding.
    unsafe {
        *out = &(*Box::into_raw(table)).base;
    }
    STATUS_OK
}
