//! The vibeshine_drm primary node and its two private presentation ioctls.
//!
//! The helper never becomes or stays DRM master and never changes display
//! state. Its only non-query ioctl is DROP_MASTER, used when the kernel
//! handed this descriptor master on open because no compositor held it, so
//! that the compositor can still acquire the device.
use crate::service::{CardOpener, DriverFrame, DriverPresentation, OpenFailure, PresentationCard};
use amaterasu_vkms_frame_helper::protocol::{Failure, MAX_PLANES};
use std::{
    ffi::CString,
    io,
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

pub const DRM_MAJOR: u32 = 226;
pub const VIBESHINE_DRIVER_NAME: &[u8] = b"vibeshine_drm";
/// Largest Linux minor number (MINORBITS = 20).
const MAX_MINOR: u32 = (1 << 20) - 1;

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "x86",
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
compile_error!("the frame helper encodes ioctl numbers with the generic Linux layout");

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const DRM_IOCTL_BASE: u32 = b'd' as u32;
const DRM_COMMAND_BASE: u32 = 0x40;

/// Generic Linux `_IOC(direction, 'd', number, size)`.
pub const fn drm_ioctl(direction: u32, number: u32, size: usize) -> u32 {
    (direction << 30) | ((size as u32) << 16) | (DRM_IOCTL_BASE << 8) | number
}

/// `struct drm_version` from the DRM uapi.
#[repr(C)]
struct DrmVersion {
    version_major: libc::c_int,
    version_minor: libc::c_int,
    version_patchlevel: libc::c_int,
    name_len: libc::size_t,
    name: *mut libc::c_char,
    date_len: libc::size_t,
    date: *mut libc::c_char,
    desc_len: libc::size_t,
    desc: *mut libc::c_char,
}

/// `struct drm_auth` from the DRM uapi.
#[repr(C)]
struct DrmAuth {
    magic: libc::c_uint,
}

/// `struct vibeshine_drm_wait_present` from vibeshine_drm_uapi.h.
#[repr(C)]
#[derive(Default)]
pub struct WaitPresentRecord {
    pub abi_version: u32,
    pub crtc_id: u32,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub timeout_ms: u32,
    pub flags: u32,
    pub reserved: [u64; 2],
}

/// `struct vibeshine_drm_frame` from vibeshine_drm_uapi.h.
#[repr(C)]
#[derive(Default)]
pub struct FrameRecord {
    pub abi_version: u32,
    pub crtc_id: u32,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub plane_count: u32,
    pub dma_buf_fds: [i32; MAX_PLANES],
    pub pitches: [u32; MAX_PLANES],
    pub offsets: [u32; MAX_PLANES],
    pub sync_file_fds: [i32; MAX_PLANES],
    pub reserved_u32: u32,
    pub reserved: [u64; 4],
}

const _: () = assert!(size_of::<WaitPresentRecord>() == 48);
const _: () = assert!(size_of::<FrameRecord>() == 152);

pub const DRM_IOCTL_VERSION: u32 = drm_ioctl(IOC_READ | IOC_WRITE, 0x00, size_of::<DrmVersion>());
pub const DRM_IOCTL_AUTH_MAGIC: u32 = drm_ioctl(IOC_WRITE, 0x11, size_of::<DrmAuth>());
pub const DRM_IOCTL_DROP_MASTER: u32 = drm_ioctl(0, 0x1f, 0);
pub const VIBESHINE_IOCTL_WAIT_PRESENT: u32 = drm_ioctl(
    IOC_READ | IOC_WRITE,
    DRM_COMMAND_BASE,
    size_of::<WaitPresentRecord>(),
);
pub const VIBESHINE_IOCTL_GET_FRAME: u32 = drm_ioctl(
    IOC_READ | IOC_WRITE,
    DRM_COMMAND_BASE + 1,
    size_of::<FrameRecord>(),
);

/// Issues one ioctl whose argument is a single plain record.
///
/// # Safety
/// `request` must be an ioctl whose argument is exactly one `T`, read and
/// written in place without retaining the pointer.
unsafe fn ioctl_record<T>(device: &OwnedFd, request: u32, record: *mut T) -> io::Result<()> {
    // SAFETY: the caller pairs the request number with its argument type;
    // the record pointer is valid for the duration of the call.
    let result = unsafe { libc::ioctl(device.as_raw_fd(), request as libc::Ioctl, record) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Opens `/dev/dri/card<minor>` when it is that exact vibeshine_drm node.
pub struct DeviceOpener;

impl CardOpener for DeviceOpener {
    type Card = VibeshineCard;

    fn open(&self, major: u32, minor: u32) -> Result<VibeshineCard, OpenFailure> {
        if major != DRM_MAJOR || minor > MAX_MINOR {
            return Err(rejected(Failure::DeviceRejected));
        }
        let device = open_node(minor)?;
        verify_device_number(&device, major, minor)?;
        verify_driver_name(&device)?;
        release_unintended_master(&device)?;
        Ok(VibeshineCard { device })
    }
}

fn rejected(failure: Failure) -> OpenFailure {
    OpenFailure {
        failure,
        os_error: 0,
    }
}

fn os_failure(failure: Failure, error: &io::Error) -> OpenFailure {
    OpenFailure {
        failure,
        os_error: error.raw_os_error().unwrap_or(0),
    }
}

fn open_node(minor: u32) -> Result<OwnedFd, OpenFailure> {
    let path = CString::new(format!("/dev/dri/card{minor}")).expect("no interior NUL");
    let flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NOFOLLOW;
    // SAFETY: `path` is a NUL-terminated string that outlives the call.
    let raw = unsafe { libc::open(path.as_ptr(), flags) };
    if raw < 0 {
        return Err(os_failure(
            Failure::DeviceUnavailable,
            &io::Error::last_os_error(),
        ));
    }
    // SAFETY: open returned a new descriptor owned only by this value.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn verify_device_number(device: &OwnedFd, major: u32, minor: u32) -> Result<(), OpenFailure> {
    // SAFETY: stat is plain data; zero is a valid initial value.
    let mut status: libc::stat = unsafe { zeroed() };
    // SAFETY: `status` is writable for one stat record.
    if unsafe { libc::fstat(device.as_raw_fd(), &mut status) } != 0 {
        return Err(os_failure(
            Failure::DeviceUnavailable,
            &io::Error::last_os_error(),
        ));
    }
    let character_device = status.st_mode & libc::S_IFMT == libc::S_IFCHR;
    if !character_device || status.st_rdev != libc::makedev(major, minor) {
        return Err(rejected(Failure::DeviceRejected));
    }
    Ok(())
}

fn verify_driver_name(device: &OwnedFd) -> Result<(), OpenFailure> {
    let mut name = [0u8; 32];
    let mut version = DrmVersion {
        version_major: 0,
        version_minor: 0,
        version_patchlevel: 0,
        name_len: name.len(),
        name: name.as_mut_ptr().cast(),
        date_len: 0,
        date: std::ptr::null_mut(),
        desc_len: 0,
        desc: std::ptr::null_mut(),
    };
    // SAFETY: DRM_IOCTL_VERSION takes one drm_version; the kernel writes at
    // most `name_len` bytes into `name`, which lives through the call, and
    // writes nothing through the zero-length date and description buffers.
    unsafe { ioctl_record(device, DRM_IOCTL_VERSION, &mut version) }
        .map_err(|error| os_failure(Failure::DeviceRejected, &error))?;
    // The kernel reports the full name length even when it truncated the copy.
    let matches = version.name_len == VIBESHINE_DRIVER_NAME.len()
        && &name[..VIBESHINE_DRIVER_NAME.len()] == VIBESHINE_DRIVER_NAME;
    if !matches {
        return Err(rejected(Failure::WrongDriver));
    }
    Ok(())
}

/// The kernel makes the first opener of an unowned primary node its master.
/// Keeping that role would stop the compositor from acquiring the device, so
/// it is released immediately. Master-only AUTH_MAGIC with the never-issued
/// magic 0 answers EACCES for a non-master descriptor and changes nothing.
fn release_unintended_master(device: &OwnedFd) -> Result<(), OpenFailure> {
    let mut auth = DrmAuth { magic: 0 };
    // SAFETY: DRM_IOCTL_AUTH_MAGIC takes one drm_auth that lives through the call.
    let probe = unsafe { ioctl_record(device, DRM_IOCTL_AUTH_MAGIC, &mut auth) };
    if matches!(&probe, Err(error) if error.raw_os_error() == Some(libc::EACCES)) {
        return Ok(());
    }
    // SAFETY: DROP_MASTER takes no argument; the null pointer is never read.
    match unsafe { ioctl_record::<u8>(device, DRM_IOCTL_DROP_MASTER, std::ptr::null_mut()) } {
        Ok(()) => Ok(()),
        // EINVAL: this descriptor was not master after all.
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        Err(error) => Err(os_failure(Failure::DeviceUnavailable, &error)),
    }
}

pub struct VibeshineCard {
    device: OwnedFd,
}

impl PresentationCard for VibeshineCard {
    fn export_frame(&self, crtc_id: u32) -> io::Result<DriverFrame> {
        // The driver requires every field except the version and CRTC to be zero.
        let mut record = FrameRecord {
            abi_version: crate::service::DRIVER_ABI_VERSION,
            crtc_id,
            ..FrameRecord::default()
        };
        // SAFETY: GET_FRAME takes one vibeshine_drm_frame that lives through the call.
        unsafe { ioctl_record(&self.device, VIBESHINE_IOCTL_GET_FRAME, &mut record) }?;
        Ok(take_frame(record))
    }

    fn wait_present(
        &self,
        crtc_id: u32,
        last_sequence: u64,
        timeout_ms: u32,
    ) -> io::Result<DriverPresentation> {
        let mut record = WaitPresentRecord {
            abi_version: crate::service::DRIVER_ABI_VERSION,
            crtc_id,
            sequence: last_sequence,
            timeout_ms,
            ..WaitPresentRecord::default()
        };
        // SAFETY: WAIT_PRESENT takes one vibeshine_drm_wait_present that lives
        // through the call.
        unsafe { ioctl_record(&self.device, VIBESHINE_IOCTL_WAIT_PRESENT, &mut record) }?;
        Ok(DriverPresentation {
            abi_version: record.abi_version,
            crtc_id: record.crtc_id,
            sequence: record.sequence,
            timestamp_ns: record.timestamp_ns,
            timeout_ms: record.timeout_ms,
            flags: record.flags,
            reserved_zero: record.reserved == [0; 2],
        })
    }
}

/// Takes ownership of every descriptor a successful GET_FRAME installed.
/// Only called after success: on failure the driver already closed them.
fn take_frame(record: FrameRecord) -> DriverFrame {
    let own = |raw: i32| {
        // SAFETY: the driver installed each nonnegative descriptor in this
        // process for the caller to own and close.
        (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw) })
    };
    DriverFrame {
        abi_version: record.abi_version,
        crtc_id: record.crtc_id,
        sequence: record.sequence,
        timestamp_ns: record.timestamp_ns,
        flags: record.flags,
        width: record.width,
        height: record.height,
        fourcc: record.fourcc,
        modifier: record.modifier,
        plane_count: record.plane_count,
        dma_bufs: record.dma_buf_fds.map(own),
        pitches: record.pitches,
        offsets: record.offsets,
        sync_files: record.sync_file_fds.map(own),
        reserved_zero: record.reserved_u32 == 0 && record.reserved == [0; 4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(DRM_IOCTL_VERSION, 0xC040_6400);
        assert_eq!(DRM_IOCTL_AUTH_MAGIC, 0x4004_6411);
        assert_eq!(DRM_IOCTL_DROP_MASTER, 0x0000_641F);
        assert_eq!(VIBESHINE_IOCTL_WAIT_PRESENT, 0xC030_6440);
        assert_eq!(VIBESHINE_IOCTL_GET_FRAME, 0xC098_6441);
    }

    #[test]
    fn device_numbers_outside_drm_are_rejected_before_opening() {
        assert_eq!(
            DeviceOpener.open(8, 0).err().map(|failure| failure.failure),
            Some(Failure::DeviceRejected)
        );
        assert_eq!(
            DeviceOpener
                .open(DRM_MAJOR, MAX_MINOR + 1)
                .err()
                .map(|failure| failure.failure),
            Some(Failure::DeviceRejected)
        );
    }

    #[test]
    fn negative_driver_descriptors_are_not_owned() {
        let record = FrameRecord {
            dma_buf_fds: [-1; MAX_PLANES],
            sync_file_fds: [-1; MAX_PLANES],
            ..FrameRecord::default()
        };
        let frame = take_frame(record);
        assert!(frame.dma_bufs.iter().all(Option::is_none));
        assert!(frame.sync_files.iter().all(Option::is_none));
        assert!(frame.reserved_zero);
    }

    #[test]
    #[ignore = "requires vibeshine_drm and CAP_SYS_ADMIN"]
    fn exports_a_frame_from_real_hardware() {
        let card = DeviceOpener
            .open(DRM_MAJOR, 0)
            .expect("vibeshine_drm card0");
        let _ = card.export_frame(1);
    }
}
