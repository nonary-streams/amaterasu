//! Win32 handle, overlapped-I/O and CCD boundaries. No driver code is bundled.
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
    ) -> Result<Observation, &'static str> {
        let state = self.exchange(protocol::QUERY_STATE, &[], protocol::STATE_BYTES)?;
        let exists = protocol::present(&state, lease, display)?;
        let lease_state =
            self.exchange(protocol::QUERY_LEASE, &protocol::lease_request(lease), 48)?;
        let pending_departure = protocol::pending_departure(&lease_state, lease)?;
        let ready = if exists {
            match binding {
                Some(binding) => monitor_identity(binding)?,
                None => None,
            }
        } else {
            None
        };
        Ok(Observation {
            exists,
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

fn monitor_identity(binding: Binding) -> Result<Option<Ready>, &'static str> {
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
    let path = paths[..path_count as usize].iter().find(|path| {
        path.targetInfo.adapterId.LowPart == binding.adapter_low
            && path.targetInfo.adapterId.HighPart == binding.adapter_high
            && path.targetInfo.id == binding.target
            && path.targetInfo.targetAvailable != 0
    });
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
    Ok(Some(Ready {
        device_path,
        active: path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0,
    }))
}
