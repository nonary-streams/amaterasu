//! Read-only verification of the same device interface used by the display
//! provider. No monitor is created, changed, or removed by this probe.
use amaterasu_package::PackageResult;
use std::ptr::{null, null_mut};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*, Foundation::*, Storage::FileSystem::*,
        System::IO::DeviceIoControl,
    },
    core::GUID,
};
const INTERFACE: GUID = GUID {
    data1: 0x5f894d6c,
    data2: 0x3a69,
    data3: 0x48a2,
    data4: [0x86, 0xef, 0xe4, 0xc6, 0x71, 0x93, 0x2d, 0x63],
};
struct Device(HANDLE);
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub(super) fn probe() -> PackageResult<()> {
    let mut size = 0;
    // SAFETY: GUID and writable size are valid for this bounded local query.
    if unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut size,
            &INTERFACE,
            null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    } != CR_SUCCESS
        || !(2..=8192).contains(&size)
    {
        return Err("The virtual display control device is unavailable".into());
    }
    let mut paths = vec![0u16; size as usize];
    // SAFETY: output owns exactly size writable UTF-16 units.
    if unsafe {
        CM_Get_Device_Interface_ListW(
            &INTERFACE,
            null(),
            paths.as_mut_ptr(),
            size,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    } != CR_SUCCESS
    {
        return Err("The virtual display control interface could not be read".into());
    }
    let interfaces: Vec<_> = paths
        .split(|value| *value == 0)
        .filter(|value| !value.is_empty())
        .collect();
    if interfaces.len() != 1 {
        return Err("Expected exactly one virtual display control device".into());
    }
    let path: Vec<_> = interfaces[0].iter().copied().chain(Some(0)).collect();
    // SAFETY: read/write access matches actual runtime operations. This opens
    // only the fixed driver interface, with no inherited handle or custom ACL.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err("The runtime account cannot open the virtual display device; complete its access setup or restart the computer".into());
    }
    let device = Device(handle);
    let mut version = [0u8; 24];
    let mut returned = 0;
    // GET_PROTOCOL_VERSION, FILE_ANY_ACCESS, METHOD_BUFFERED. The signed
    // adapter owns this driver-specific ABI; no consumer receives an OS handle.
    const QUERY_VERSION: u32 = (0x22 << 16) | (0x900 << 2);
    // SAFETY: this synchronous, read-only query owns its fixed output buffer
    // and handle until the call returns. The helper process has a time limit.
    if unsafe {
        DeviceIoControl(
            device.0,
            QUERY_VERSION,
            null(),
            0,
            version.as_mut_ptr().cast(),
            version.len() as u32,
            &mut returned,
            null_mut(),
        )
    } == 0
        || returned != 24
    {
        return Err("The virtual display driver did not answer its protocol query".into());
    }
    let namespace = [
        0x84, 0x42, 0x86, 0xa2, 0xfe, 0x77, 0x36, 0x43, 0xa8, 0x28, 0x00, 0xfe, 0xec, 0x89, 0xeb,
        0xac,
    ];
    if version[..16] != namespace
        || u16::from_le_bytes([version[16], version[17]]) != 3
        || u16::from_le_bytes([version[18], version[19]]) < 6
    {
        return Err("The installed virtual display control protocol is incompatible".into());
    }
    Ok(())
}
