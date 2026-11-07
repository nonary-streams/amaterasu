//! Windows metadata checks. This module never creates a D3D device or capture.
use super::{ExactGeometry, OutputObservation, ready, verify_outputs};
use crate::backend::Backend;
use amaterasu_module_sdk::capture::{EXACT_TARGET_PREFIX, exact_target_key, exact_target_selector};
use amaterasu_module_sdk::capture::{SourcePreflightRequest, SourcePreflightResponse};
use amaterasu_windows_capture_geometry::resolve_target_matching;
use std::{marker::PhantomData, rc::Rc};
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::{
    Foundation::HANDLE,
    System::{
        LibraryLoader::GetModuleHandleW,
        RemoteDesktop::{
            ProcessIdToSessionId, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW,
            WTSQuerySessionInformationW, WTSSessionInfoEx,
        },
        StationsAndDesktops::{
            GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
        },
        Threading::GetCurrentProcessId,
    },
    UI::WindowsAndMessaging::WSF_VISIBLE,
};
use windows::core::w;

pub(super) fn query<B: Backend>(
    backend: &B,
    request: &SourcePreflightRequest,
) -> SourcePreflightResponse {
    if let Err(reason) = environment() {
        return SourcePreflightResponse::unavailable(request, reason);
    }
    let _apartment = match MetadataApartment::enter() {
        Ok(apartment) => apartment,
        Err(reason) => return SourcePreflightResponse::unavailable(request, reason),
    };
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return SourcePreflightResponse::unsupported(request, "graphics_capture_unavailable");
    }
    if let Some(route) = request.route.as_deref() {
        if let Err(reason) = exact_output(backend, route, request) {
            return SourcePreflightResponse::unavailable(request, reason);
        }
        let format = crate::format::CaptureFormat {
            ten_bit: request.mode.bit_depth == Some(10) || request.mode.hdr,
            hdr: request.mode.hdr,
            color_proof: request.mode.bit_depth.is_some() || request.mode.hdr,
        };
        // The exact route must independently prove scan-out range before a
        // high-depth source is admitted. This creates no device, pool or worker.
        if let Err(error) = backend.verify_source_format(route, format) {
            return SourcePreflightResponse::unavailable(request, error.code);
        }
    }
    ready(request)
}

struct MetadataApartment {
    initialized: bool,
    _thread: PhantomData<Rc<()>>,
}
impl MetadataApartment {
    fn enter() -> Result<Self, &'static str> {
        // SAFETY: this balanced query-thread apartment is used only for WinRT
        // capability metadata. No capture item, pool, device or permission is created.
        let initialized = match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => true,
            Err(error) if error.code() == RPC_E_CHANGED_MODE => false,
            Err(_) => return Err("capture_metadata_unavailable"),
        };
        Ok(Self {
            initialized,
            _thread: PhantomData,
        })
    }
}
impl Drop for MetadataApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances only this successful initialization on the same
            // thread, after every metadata interface in the query has retired.
            unsafe { RoUninitialize() };
        }
    }
}

fn environment() -> Result<(), &'static str> {
    // SAFETY: module queries only inspect already loaded DLLs; they neither
    // load a library nor initialize a graphics device.
    if unsafe { GetModuleHandleW(w!("dxgi.dll")) }.is_err()
        || unsafe { GetModuleHandleW(w!("d3d11.dll")) }.is_err()
    {
        return Err("capture_libraries_unavailable");
    }
    let mut session = 0;
    // SAFETY: the scalar session output remains writable during this query.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }.is_err() || session == 0
    {
        return Err("interactive_session_unavailable");
    }
    unlocked_session(session)?;
    // SAFETY: this returns a borrowed process window station. No desktop is
    // opened, switched, or closed, and no permission changes are requested.
    let station =
        unsafe { GetProcessWindowStation() }.map_err(|_| "interactive_session_unavailable")?;
    let mut flags = USEROBJECTFLAGS::default();
    let mut needed = 0;
    // SAFETY: flags and needed are correctly sized writable stack outputs;
    // the station handle remains owned by Windows throughout this observation.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_FLAGS,
            Some((&mut flags as *mut USEROBJECTFLAGS).cast()),
            size_of::<USEROBJECTFLAGS>() as u32,
            Some(&mut needed),
        )
    }
    .map_err(|_| "interactive_session_unavailable")?;
    if flags.dwFlags & WSF_VISIBLE as u32 == 0 {
        return Err("interactive_session_unavailable");
    }
    Ok(())
}

fn exact_output<B: Backend>(
    backend: &B,
    route: &str,
    request: &SourcePreflightRequest,
) -> Result<(), &'static str> {
    let outputs = backend
        .enumerate()
        .map_err(|_| "source_enumeration_unavailable")?;
    let exact = if route.starts_with(EXACT_TARGET_PREFIX) {
        if exact_target_key(route).is_none() {
            return Err("invalid_exact_source");
        }
        Some(
            resolve_target_matching(|identity| {
                exact_target_selector(&identity.to_ascii_lowercase())
                    .is_ok_and(|candidate| candidate == route)
            })
            .map_err(|_| "exact_source_unavailable")?
            .1,
        )
    } else {
        None
    };
    let exact = exact.map(|selected| ExactGeometry {
        source_name: selected.source_name,
        left: selected.rectangle.left,
        top: selected.rectangle.top,
        width: selected.rectangle.width,
        height: selected.rectangle.height,
    });
    verify_outputs(
        outputs.iter().map(|output| OutputObservation {
            id: &output.id,
            source_name: &output.device_name,
            attached: output.attached,
            left: output.left,
            top: output.top,
            width: output.width,
            height: output.height,
        }),
        route,
        exact.as_ref(),
        request,
    )
}

fn unlocked_session(session: u32) -> Result<(), &'static str> {
    let mut buffer = windows::core::PWSTR::null();
    let mut bytes = 0;
    // SAFETY: Windows allocates one read-only metadata buffer for this query;
    // its exact allocation is released below on every path, without opening a desktop.
    unsafe {
        WTSQuerySessionInformationW(None, session, WTSSessionInfoEx, &mut buffer, &mut bytes)
    }
    .map_err(|_| "interactive_session_unavailable")?;
    struct Metadata(*mut core::ffi::c_void);
    impl Drop for Metadata {
        fn drop(&mut self) {
            // SAFETY: this uniquely owns the successful WTS query allocation.
            unsafe { WTSFreeMemory(self.0) };
        }
    }
    let metadata = Metadata(buffer.0.cast());
    if metadata.0.is_null()
        || bytes < size_of::<WTSINFOEXW>() as u32
        || !(metadata.0 as usize).is_multiple_of(align_of::<WTSINFOEXW>())
    {
        return Err("interactive_session_unavailable");
    }
    // SAFETY: the checked, aligned WTS allocation covers the complete metadata
    // structure and remains retained until this function returns.
    let information = unsafe { &*metadata.0.cast::<WTSINFOEXW>() };
    if information.Level != 1 {
        return Err("interactive_session_unavailable");
    }
    // SAFETY: Level 1 selects this documented union member; it contains no
    // borrowed pointers, and the WTS allocation is still live.
    let state = unsafe { information.Data.WTSInfoExLevel1 };
    if state.SessionId != session
        || state.SessionState != WTSActive
        || state.SessionFlags != WTS_SESSIONSTATE_UNLOCK as i32
    {
        return Err("interactive_session_locked_or_disconnected");
    }
    Ok(())
}
