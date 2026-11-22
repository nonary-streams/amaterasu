use amaterasu_media::{CpuFrame, CpuFrameFitTarget, PIXEL_BGRA8, PIXEL_NV12, frame_size};
use amaterasu_module_sdk::media::OwnedCpuFrame;
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn am_video_converter_open() -> *mut c_void;
    fn am_video_converter_close(context: *mut c_void);
    fn am_video_convert(
        context: *mut c_void,
        source: *const u8,
        source_width: i32,
        source_height: i32,
        source_stride: i32,
        destination: *mut u8,
        width: i32,
        height: i32,
        bt709: i32,
        full_range: i32,
    ) -> i32;
}

pub struct Converter(NonNull<c_void>);

impl Converter {
    pub fn new() -> Option<Self> {
        // SAFETY: the returned context has a single owner.
        NonNull::new(unsafe { am_video_converter_open() }).map(Self)
    }

    pub fn convert(
        &mut self,
        frame: &CpuFrame,
        target: CpuFrameFitTarget,
    ) -> Option<OwnedCpuFrame> {
        if frame.format != PIXEL_BGRA8
            || !target.width.is_multiple_of(2)
            || !target.height.is_multiple_of(2)
        {
            return None;
        }
        let length = frame_size(target.width, target.height, PIXEL_NV12, target.width).ok()?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).ok()?;
        bytes.resize(length, 0);
        // SAFETY: the worker holds the validated source and owned destination
        // allocations through the call. The converter retains no pixel pointer.
        let status = unsafe {
            am_video_convert(
                self.0.as_ptr(),
                frame.data,
                frame.width.try_into().ok()?,
                frame.height.try_into().ok()?,
                frame.stride.try_into().ok()?,
                bytes.as_mut_ptr(),
                target.width as i32,
                target.height as i32,
                target.bt709 as i32,
                target.full_range as i32,
            )
        };
        if status != 0 {
            return None;
        }
        OwnedCpuFrame::new(
            target.width,
            target.height,
            PIXEL_NV12,
            target.width,
            bytes,
            frame.timestamp_us,
            frame.sequence,
        )
        .ok()
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: the context belongs exclusively to this converter.
        unsafe { am_video_converter_close(self.0.as_ptr()) };
    }
}
