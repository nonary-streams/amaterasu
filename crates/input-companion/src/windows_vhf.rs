//! Windows device transport for the restricted UMDF VHF source interface.
//! Opening the device is not an elevation mechanism: the device ACL admits the
//! installed companion service SID, and the OS checks it on every open.
use crate::{InputError, InputResult, VhfTransport};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::core::GUID;

const INTERFACE: GUID = GUID {
    data1: 0xb539be52,
    data2: 0xb2c5,
    data3: 0x462a,
    data4: [0x98, 0xc2, 0xc4, 0xa9, 0xc8, 0x49, 0xcb, 0x56],
};
const GENERIC_READ_WRITE: u32 = 0xc000_0000;
const MAX_DEVICE_PATH_BYTES: u32 = 4096;

struct DeviceInfoSet(HDEVINFO);

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by SetupDiGetClassDevsW and is
        // released once after enumeration or any early return.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

pub struct WindowsVhfTransport {
    device: HANDLE,
}

impl WindowsVhfTransport {
    /// Open only from the separately installed companion service. No user
    /// module should receive the service token or the device handle.
    pub fn open() -> InputResult<Self> {
        let path = device_path()?;
        // SAFETY: `path` is NUL-terminated UTF-16 and remains alive through
        // CreateFileW. The returned device handle is owned by this transport.
        let device = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if device == INVALID_HANDLE_VALUE {
            return Err(InputError::Device);
        }
        Ok(Self { device })
    }
}

fn device_path() -> InputResult<Vec<u16>> {
    // SAFETY: the interface GUID is a valid immutable GUID, and null optional
    // arguments request present device interfaces of exactly this class.
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &INTERFACE,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw == -1 {
        return Err(InputError::Device);
    }
    let devices = DeviceInfoSet(raw);
    let mut interface = SP_DEVICE_INTERFACE_DATA {
        cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    // SAFETY: `interface` is writable with the correct cbSize; only the first
    // matching interface is accepted to prevent choosing a device by a path
    // supplied by an untrusted worker.
    if unsafe { SetupDiEnumDeviceInterfaces(devices.0, null(), &INTERFACE, 0, &mut interface) } == 0
    {
        return Err(InputError::Device);
    }
    let mut needed = 0;
    // SAFETY: this sizing call intentionally supplies no detail buffer.
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            devices.0,
            &interface,
            null_mut(),
            0,
            &mut needed,
            null_mut(),
        )
    };
    if !(std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32..=MAX_DEVICE_PATH_BYTES)
        .contains(&needed)
    {
        return Err(InputError::Device);
    }
    // u64 storage supplies the alignment required by the detail structure.
    let mut storage = vec![0u64; (needed as usize).div_ceil(8)];
    let detail = storage
        .as_mut_ptr()
        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // SAFETY: storage is aligned and at least `needed` bytes long. The API
    // writes at most that many bytes and the pointer is not retained.
    unsafe { (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 };
    let mut written = 0;
    // SAFETY: all buffers are live and sized as declared, and the interface
    // data came from the same device information set.
    if unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            devices.0,
            &interface,
            detail,
            needed,
            &mut written,
            null_mut(),
        )
    } == 0
    {
        return Err(InputError::Device);
    }
    let path_offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    if written as usize <= path_offset + 2 || written > needed {
        return Err(InputError::Device);
    }
    // SAFETY: DevicePath begins within the allocated detail buffer, and the
    // bounded slice cannot extend beyond bytes written by SetupDi.
    let path = unsafe {
        std::slice::from_raw_parts(
            (*detail).DevicePath.as_ptr(),
            (written as usize - path_offset) / 2,
        )
    };
    let end = path
        .iter()
        .position(|unit| *unit == 0)
        .ok_or(InputError::Device)?;
    if end == 0 {
        return Err(InputError::Device);
    }
    let mut result = path[..end].to_vec();
    result.push(0);
    Ok(result)
}

impl VhfTransport for WindowsVhfTransport {
    fn control(&mut self, ioctl: u32, request: &[u8], response: &mut [u8]) -> InputResult<usize> {
        let input_size = u32::try_from(request.len()).map_err(|_| InputError::InvalidEvent)?;
        let output_size = u32::try_from(response.len()).map_err(|_| InputError::InvalidEvent)?;
        let mut written = 0;
        // SAFETY: the synchronous call does not retain either byte slice.
        // The exclusive handle is owned by this transport until Drop.
        let status = unsafe {
            DeviceIoControl(
                self.device,
                ioctl,
                request.as_ptr().cast(),
                input_size,
                response.as_mut_ptr().cast(),
                output_size,
                &mut written,
                null_mut(),
            )
        };
        if status == 0 || written > output_size {
            return Err(InputError::Device);
        }
        Ok(written as usize)
    }
}

impl Drop for WindowsVhfTransport {
    fn drop(&mut self) {
        // SAFETY: this handle came from CreateFileW and is closed once. The
        // driver destroys the HID child when its owning file object closes.
        unsafe { CloseHandle(self.device) };
    }
}
