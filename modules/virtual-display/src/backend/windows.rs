//! Win32 handle, overlapped-I/O and CCD boundaries. No driver code is bundled.
use super::capture_target::CaptureReadback;
use super::{Driver, Observation, Ready};
use crate::model::Specification;
use crate::protocol::{self, Binding};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::core::GUID;

const INTERFACE: GUID = GUID {
    data1: 0x5f894d6c,
    data2: 0x3a69,
    data3: 0x48a2,
    data4: [0x86, 0xef, 0xe4, 0xc6, 0x71, 0x93, 0x2d, 0x63],
};
const IO_TIMEOUT_MS: u32 = 200;

#[derive(Default)]
pub struct NativeDriver {
    handle: Option<OwnedHandle>,
}

impl NativeDriver {
    fn exchange(
        &mut self,
        code: u32,
        input: &[u8],
        output_size: usize,
    ) -> Result<Vec<u8>, &'static str> {
        if self.handle.is_none() {
            let handle = open_driver()?;
            protocol::check_version(&ioctl(&handle, protocol::VERSION, &[], 24)?)?;
            self.handle = Some(handle);
        }
        let result = ioctl(
            self.handle.as_ref().expect("connected driver"),
            code,
            input,
            output_size,
        );
        if result.is_err() {
            self.handle = None;
        }
        result
    }
}

impl Driver for NativeDriver {
    fn capture_target(
        &mut self,
        binding: Option<Binding>,
        expected: &Specification,
    ) -> Result<Option<Ready>, &'static str> {
        binding.map_or(Ok(None), |binding| monitor_identity(binding, expected))
    }
    fn supported(&self) -> bool {
        true
    }
    fn create(
        &mut self,
        spec: &Specification,
        lease: u64,
        display: u64,
        capability: &[u8; 32],
    ) -> Result<Binding, &'static str> {
        let request = protocol::create(spec, lease, display, capability);
        protocol::created(
            &self.exchange(protocol::CREATE_OWNED, &request, 56)?,
            lease,
            display,
        )
    }
    fn feed(&mut self, lease: u64) -> Result<(), &'static str> {
        self.exchange(protocol::FEED, &protocol::lease_request(lease), 0)
            .map(|_| ())
    }
    fn reclaim(
        &mut self,
        display: u64,
        lease: u64,
        capability: &[u8; 32],
    ) -> Result<(), &'static str> {
        let reply = self.exchange(
            protocol::RECLAIM,
            &protocol::reclaim_request(display, lease, capability),
            48,
        )?;
        protocol::reclaimed(&reply, lease, display)
    }
    fn remove(&mut self, lease: u64, display: u64) -> Result<(), &'static str> {
        self.exchange(
            protocol::REMOVE,
            &protocol::remove_request(lease, display),
            0,
        )
        .map(|_| ())
    }
    fn inspect(
        &mut self,
        lease: u64,
        display: u64,
        binding: Option<Binding>,
        specification: &Specification,
    ) -> Result<Observation, &'static str> {
        let state = self.exchange(protocol::QUERY_STATE, &[], protocol::STATE_BYTES)?;
        let exists = protocol::present(&state, lease, display)?;
        let occupied_by_other_lease = protocol::occupied_by_other_lease(&state, lease, display)?;
        let specification_matches =
            !exists || protocol::matches_specification(&state, lease, display, specification)?;
        let lease_state =
            self.exchange(protocol::QUERY_LEASE, &protocol::lease_request(lease), 48)?;
        let pending_departure = protocol::pending_departure(&lease_state, lease)?;
        let lease_display_count = protocol::lease_display_count(&lease_state, lease)?;
        let ready = if exists {
            match binding {
                Some(binding) => monitor_identity(binding, specification)?,
                None => None,
            }
        } else {
            None
        };
        Ok(Observation {
            exists,
            occupied_by_other_lease,
            specification_matches,
            lease_display_count,
            pending_departure,
            ready,
        })
    }
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: each successful native handle is owned exactly once by this wrapper.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn open_driver() -> Result<OwnedHandle, &'static str> {
    let mut size = 0;
    // SAFETY: GUID and size storage are live; null device ID requests this interface class.
    let result = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut size,
            &INTERFACE,
            null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if result != CR_SUCCESS || !(2..=8192).contains(&size) {
        return Err("driver_unavailable");
    }
    let mut list = vec![0u16; size as usize];
    // SAFETY: list owns exactly size writable UTF-16 code units through this call.
    if unsafe {
        CM_Get_Device_Interface_ListW(
            &INTERFACE,
            null(),
            list.as_mut_ptr(),
            size,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    } != CR_SUCCESS
    {
        return Err("driver_enumeration_failed");
    }
    let paths = protocol::parse_interfaces(&list)?;
    if paths.len() != 1 {
        return Err(if paths.is_empty() {
            "driver_unavailable"
        } else {
            "ambiguous_driver"
        });
    }
    let mut path = paths[0].encode_utf16().collect::<Vec<_>>();
    path.push(0);
    // SAFETY: path is NUL-terminated and remains live; no handle inheritance/security override.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        Err("driver_open_failed")
    } else {
        Ok(OwnedHandle(handle))
    }
}

fn ioctl(
    handle: &OwnedHandle,
    code: u32,
    input: &[u8],
    output_size: usize,
) -> Result<Vec<u8>, &'static str> {
    if input.len() > 128 || output_size > protocol::STATE_BYTES {
        return Err("invalid_driver_request");
    }
    // SAFETY: unnamed auto-reset event has no inherited handle or borrowed security data.
    let event = unsafe { CreateEventW(null(), 0, 0, null()) };
    if event.is_null() {
        return Err("driver_event_failed");
    }
    let event = OwnedHandle(event);
    let mut operation = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    let mut output = vec![0u8; output_size];
    let mut returned = 0;
    // SAFETY: input/output/OVERLAPPED/event remain owned and live until completion is
    // observed below, including after cancellation. METHOD_BUFFERED copies the payload.
    let started = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            if input.is_empty() {
                null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output_size as u32,
            &mut returned,
            &mut operation,
        )
    };
    if started == 0 {
        // SAFETY: thread-local error belongs to the immediately preceding call.
        if unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err("driver_io_failed");
        }
        // SAFETY: the event handle is live until the operation's completion is drained.
        if unsafe { WaitForSingleObject(event.0, IO_TIMEOUT_MS) } != WAIT_OBJECT_0 {
            // SAFETY: cancel only this exact operation, then drain it before freeing any
            // buffer. A broken driver that never completes cancellation can block here;
            // the isolated host worker watchdog is the final containment boundary.
            unsafe {
                CancelIoEx(handle.0, &operation);
                GetOverlappedResult(handle.0, &operation, &mut returned, 1);
            }
            return Err("driver_io_timeout");
        }
    }
    // SAFETY: completion was synchronous or its event signalled; output remains writable.
    if unsafe { GetOverlappedResult(handle.0, &operation, &mut returned, 0) } == 0 {
        return Err("driver_io_failed");
    }
    if returned as usize != output_size {
        return Err("invalid_driver_response");
    }
    Ok(output)
}

fn display_paths()
-> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), &'static str> {
    let mut path_count = 0;
    let mut mode_count = 0;
    // SAFETY: scalar outputs are writable; this read-only CCD call has no side effects.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut path_count, &mut mode_count) }
        != ERROR_SUCCESS
    {
        return Err("display_query_failed");
    }
    if path_count > 256 || mode_count > 256 {
        return Err("display_query_limit");
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    // SAFETY: owned buffers have the advertised capacities. A topology race is returned
    // as an error rather than retrying an unbounded native enumeration loop.
    let result = unsafe {
        QueryDisplayConfig(
            QDC_ALL_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            null_mut(),
        )
    };
    if result != ERROR_SUCCESS
        || path_count as usize > paths.len()
        || mode_count as usize > modes.len()
    {
        return Err("display_query_changed");
    }
    paths.truncate(path_count as usize);
    modes.truncate(mode_count as usize);
    Ok((paths, modes))
}

fn owned_target(path: &DISPLAYCONFIG_PATH_INFO, binding: Binding) -> bool {
    path.targetInfo.adapterId.LowPart == binding.adapter_low
        && path.targetInfo.adapterId.HighPart == binding.adapter_high
        && path.targetInfo.id == binding.target
        && path.targetInfo.targetAvailable != 0
}

fn monitor_identity(
    binding: Binding,
    specification: &Specification,
) -> Result<Option<Ready>, &'static str> {
    let (paths, modes) = display_paths()?;
    let path = paths
        .iter()
        .find(|path| owned_target(path, binding) && path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
        .or_else(|| paths.iter().find(|path| owned_target(path, binding)));
    let Some(path) = path else { return Ok(None) };
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: binding.target,
        },
        ..Default::default()
    };
    // SAFETY: header identifies the full correctly sized owned output structure.
    if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
        return Err("display_identity_unavailable");
    }
    let Some(device_path) = protocol::display_path(&target.monitorDevicePath)? else {
        return Ok(None);
    };
    // Identity publication can precede display activation. Capture readiness is
    // independently sampled and may remain unavailable while the coordinator
    // composes the target into the desktop; this never selects another output.
    let capture_readiness =
        verified_capture_output(binding, specification, &device_path, &paths, &modes);
    let capture_error = capture_readiness.as_ref().err().copied();
    Ok(Some(Ready {
        device_path,
        active: path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0,
        capture_output: capture_readiness.ok(),
        capture_error,
    }))
}

fn active_owned_path(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    binding: Binding,
) -> Result<&DISPLAYCONFIG_PATH_INFO, &'static str> {
    let mut matches = paths
        .iter()
        .filter(|path| owned_target(path, binding) && path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0);
    let path = matches.next().ok_or("capture_target_inactive")?;
    if matches.next().is_some() {
        return Err("capture_target_ambiguous");
    }
    Ok(path)
}

fn capture_readback(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> Result<CaptureReadback, &'static str> {
    // SAFETY: the query does not request virtual-mode-aware indices, so the
    // documented modeInfoIdx union member indexes this owned snapshot.
    let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
    let source = modes
        .get(index)
        .filter(|mode| {
            mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                && mode.id == path.sourceInfo.id
                && mode.adapterId.LowPart == path.sourceInfo.adapterId.LowPart
                && mode.adapterId.HighPart == path.sourceInfo.adapterId.HighPart
        })
        .ok_or("capture_target_mode_unavailable")?;
    // SAFETY: the checked SOURCE tag selects sourceMode in the native union.
    let source = unsafe { source.Anonymous.sourceMode };
    let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: color owns the concrete writable packet described by its header.
    let result = unsafe { DisplayConfigGetDeviceInfo(&mut color.header) };
    let hdr_enabled = match result {
        0 => {
            // SAFETY: value is the documented raw bitfield view of this packet.
            let flags = unsafe { color.Anonymous.value };
            if flags & 4 != 0 || (flags & 2 != 0 && flags & 1 == 0) {
                return Err("capture_target_color_unavailable");
            }
            flags & 2 != 0
        }
        value if value == ERROR_NOT_SUPPORTED as i32 => false,
        _ => return Err("capture_target_color_unavailable"),
    };
    Ok(CaptureReadback {
        active: path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0,
        width: source.width,
        height: source.height,
        refresh_numerator: path.targetInfo.refreshRate.Numerator,
        refresh_denominator: path.targetInfo.refreshRate.Denominator,
        hdr_enabled,
    })
}

fn verified_capture_output(
    binding: Binding,
    specification: &Specification,
    device_path: &str,
    paths: &[DISPLAYCONFIG_PATH_INFO],
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> Result<String, &'static str> {
    let before_path = active_owned_path(paths, binding)?;
    let before = capture_readback(before_path, modes)?;
    if !before.matches(specification) {
        return Err("capture_target_mode_mismatch");
    }
    let (selection, selected) =
        amaterasu_windows_capture_geometry::resolve_target_matching(|target| {
            target.eq_ignore_ascii_case(device_path)
        })
        .map_err(|_| "capture_target_enumeration_unavailable")?;
    if selected.rectangle.width != specification.width
        || selected.rectangle.height != specification.height
    {
        return Err("capture_target_geometry_mismatch");
    }
    let (after_paths, after_modes) = display_paths()?;
    let after_path = active_owned_path(&after_paths, binding)?;
    let after = capture_readback(after_path, &after_modes)?;
    if before != after
        || after_path.sourceInfo.id != before_path.sourceInfo.id
        || after_path.sourceInfo.adapterId.LowPart != before_path.sourceInfo.adapterId.LowPart
        || after_path.sourceInfo.adapterId.HighPart != before_path.sourceInfo.adapterId.HighPart
    {
        return Err("capture_target_changed");
    }
    amaterasu_module_sdk::capture::exact_target_selector(&selection.target_id.to_ascii_lowercase())
}
