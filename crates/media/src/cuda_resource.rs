//! NVIDIA CUDA vendor schema for negotiated frame resources.
//!
//! Lets a producer and consumer in the same process hand frames over in CUDA
//! device memory, ordered on the GPU by CUDA events, instead of exporting a
//! DMA-BUF and synchronizing through sync_files between separate GL contexts.
//! Both sides must use the primary CUDA context of the device named by the
//! offer's UUID; device pointers and events are only meaningful there. These
//! are trusted native contracts between modules, not a sandbox.
use crate::{
    CaptureResourceOffer, FRAME_RESOURCE_VENDOR_BASE, FRAME_SYNC_VENDOR_BASE, FrameDescriptor,
};
use std::{ffi::c_void, mem::size_of};

/// Stable namespace UUID of this schema (`amaterasu.nvidia.cuda`).
pub const CUDA_RESOURCE_NAMESPACE: [u8; 16] = [
    0x6a, 0x8e, 0x2f, 0x41, 0x93, 0x0c, 0x4d, 0x57, 0xa1, 0x7b, 0x3c, 0x58, 0xe2, 0x19, 0xd4, 0x06,
];
/// Pitch-linear device memory; resource v1 points to `CudaDeviceFrame`, and
/// a v1 offer points to `CudaDeviceCapabilities`.
pub const FRAME_RESOURCE_CUDA_DEVICE: u32 = FRAME_RESOURCE_VENDOR_BASE | 1;
/// CUDA event recorded after the producer's last write; v1 points to
/// `CudaEventSync` on frames and `CudaEventCapabilities` on offers. The
/// consumer orders its reads with `cuStreamWaitEvent` before any access.
pub const FRAME_SYNC_CUDA_EVENT: u32 = FRAME_SYNC_VENDOR_BASE | 1;
pub const CUDA_RESOURCE_VERSION: u32 = 1;

/// 32-bit packed pixels, little endian: red in bits 0-9, green 10-19, blue
/// 20-29, two padding bits. Equivalent to DRM `ABGR2101010`/`XBGR2101010`.
pub const CUDA_FORMAT_X2B10G10R10: u32 = 1;

/// Offer payload for `FRAME_RESOURCE_CUDA_DEVICE` v1.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CudaDeviceCapabilities {
    /// `cuDeviceGetUuid` of the device whose primary context owns the memory.
    pub device_uuid: [u8; 16],
    pub format: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub reserved: u32,
}

/// Frame payload for `FRAME_RESOURCE_CUDA_DEVICE` v1. The memory stays valid
/// and immutable from successful retain until the matching release.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CudaDeviceFrame {
    pub device_uuid: [u8; 16],
    /// `CUdeviceptr` in the device's primary context.
    pub device_pointer: u64,
    pub pitch: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// Offer payload for `FRAME_SYNC_CUDA_EVENT` v1.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CudaEventCapabilities {
    pub device_uuid: [u8; 16],
}

/// Frame payload for `FRAME_SYNC_CUDA_EVENT` v1: a `CUevent` in the device's
/// primary context, recorded after the frame's last write. The producer does
/// not record it again while any reference to the frame is retained. Without
/// a retained reference a consumer enqueues its wait before submit returns;
/// with one it may enqueue the wait any time before releasing.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CudaEventSync {
    pub event: *mut c_void,
}

/// Builds a vendor descriptor of this schema pointing to `data`.
pub fn cuda_descriptor<T>(kind: u32, data: &T) -> FrameDescriptor {
    FrameDescriptor {
        kind,
        version: CUDA_RESOURCE_VERSION,
        size: size_of::<T>() as u32,
        reserved: 0,
        namespace: CUDA_RESOURCE_NAMESPACE,
        data: std::ptr::from_ref(data).cast(),
    }
}

fn payload<T>(descriptor: &FrameDescriptor, kind: u32) -> Option<&T> {
    if descriptor.kind != kind
        || descriptor.version != CUDA_RESOURCE_VERSION
        || descriptor.namespace != CUDA_RESOURCE_NAMESPACE
        || descriptor.reserved != 0
        || (descriptor.size as usize) < size_of::<T>()
        || descriptor.data.is_null()
        || !descriptor.data.cast::<T>().is_aligned()
    {
        return None;
    }
    // SAFETY: kind, version, namespace and size identify this exact schema,
    // whose pointed-to payload the producer keeps live for the borrow.
    Some(unsafe { &*descriptor.data.cast::<T>() })
}

/// # Safety
/// `descriptor.data` must point to the payload its schema names for the borrow.
pub unsafe fn cuda_device_capabilities(
    descriptor: &FrameDescriptor,
) -> Option<&CudaDeviceCapabilities> {
    payload(descriptor, FRAME_RESOURCE_CUDA_DEVICE)
}

/// # Safety
/// `descriptor.data` must point to the payload its schema names for the borrow.
pub unsafe fn cuda_device_frame(descriptor: &FrameDescriptor) -> Option<&CudaDeviceFrame> {
    payload(descriptor, FRAME_RESOURCE_CUDA_DEVICE)
}

/// # Safety
/// `descriptor.data` must point to the payload its schema names for the borrow.
pub unsafe fn cuda_event_capabilities(
    descriptor: &FrameDescriptor,
) -> Option<&CudaEventCapabilities> {
    payload(descriptor, FRAME_SYNC_CUDA_EVENT)
}

/// # Safety
/// `descriptor.data` must point to the payload its schema names for the borrow.
pub unsafe fn cuda_event_sync(descriptor: &FrameDescriptor) -> Option<&CudaEventSync> {
    payload(descriptor, FRAME_SYNC_CUDA_EVENT)
}

/// Whether a producer's CUDA offer can feed a consumer's CUDA offer: same
/// device, same pixel format, overlapping sizes, CUDA-event acquire, and no
/// completion object (the consumer releases only after its reads finish).
///
/// # Safety
/// Both offers' descriptors must point to live payloads for the borrow.
pub unsafe fn cuda_offers_compatible(
    producer: &CaptureResourceOffer,
    consumer: &CaptureResourceOffer,
) -> bool {
    // SAFETY: the caller guarantees both offers' payloads are live.
    let (Some(made), Some(taken), Some(signal), Some(waits)) = (unsafe {
        (
            cuda_device_capabilities(&producer.resource),
            cuda_device_capabilities(&consumer.resource),
            cuda_event_capabilities(&producer.acquire_sync),
            cuda_event_capabilities(&consumer.acquire_sync),
        )
    }) else {
        return false;
    };
    made.device_uuid == taken.device_uuid
        && made.device_uuid != [0; 16]
        && signal.device_uuid == made.device_uuid
        && waits.device_uuid == made.device_uuid
        && made.format == taken.format
        && made.min_width <= taken.max_width
        && taken.min_width <= made.max_width
        && made.min_height <= taken.max_height
        && taken.min_height <= made.max_height
        && producer.completion_sync.kind == crate::FRAME_SYNC_NONE
        && consumer.completion_sync.kind == crate::FRAME_SYNC_NONE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FRAME_ACCESS_GPU, FRAME_OWNERSHIP_RETAINABLE, FRAME_SYNC_NONE};

    const NONE: FrameDescriptor = FrameDescriptor {
        kind: FRAME_SYNC_NONE,
        version: 0,
        size: 0,
        reserved: 0,
        namespace: [0; 16],
        data: std::ptr::null(),
    };

    fn offer(
        id: u64,
        resource: &CudaDeviceCapabilities,
        event: &CudaEventCapabilities,
    ) -> CaptureResourceOffer {
        CaptureResourceOffer {
            id,
            ownership: FRAME_OWNERSHIP_RETAINABLE,
            accessibility: FRAME_ACCESS_GPU,
            reserved: 0,
            resource: cuda_descriptor(FRAME_RESOURCE_CUDA_DEVICE, resource),
            acquire_sync: cuda_descriptor(FRAME_SYNC_CUDA_EVENT, event),
            completion_sync: NONE,
        }
    }

    fn capabilities(uuid: u8, format: u32) -> CudaDeviceCapabilities {
        CudaDeviceCapabilities {
            device_uuid: [uuid; 16],
            format,
            min_width: 16,
            max_width: 8192,
            min_height: 16,
            max_height: 8192,
            reserved: 0,
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn payload_layouts_are_fixed() {
        assert_eq!(size_of::<CudaDeviceCapabilities>(), 40);
        assert_eq!(size_of::<CudaDeviceFrame>(), 40);
        assert_eq!(size_of::<CudaEventCapabilities>(), 16);
        assert_eq!(size_of::<CudaEventSync>(), 8);
        assert_eq!(std::mem::offset_of!(CudaDeviceFrame, device_pointer), 16);
    }

    #[test]
    fn same_device_and_format_are_compatible() {
        let resource = capabilities(7, CUDA_FORMAT_X2B10G10R10);
        let event = CudaEventCapabilities {
            device_uuid: [7; 16],
        };
        let producer = offer(1, &resource, &event);
        let consumer = offer(2, &resource, &event);
        assert!(unsafe { cuda_offers_compatible(&producer, &consumer) });
    }

    #[test]
    fn device_format_size_and_completion_mismatches_are_rejected() {
        let event = CudaEventCapabilities {
            device_uuid: [7; 16],
        };
        let resource = capabilities(7, CUDA_FORMAT_X2B10G10R10);
        let producer = offer(1, &resource, &event);
        let other_device = capabilities(8, CUDA_FORMAT_X2B10G10R10);
        let other_event = CudaEventCapabilities {
            device_uuid: [8; 16],
        };
        assert!(!unsafe {
            cuda_offers_compatible(&producer, &offer(2, &other_device, &other_event))
        });
        let other_format = capabilities(7, 99);
        assert!(!unsafe { cuda_offers_compatible(&producer, &offer(2, &other_format, &event)) });
        let mut small = capabilities(7, CUDA_FORMAT_X2B10G10R10);
        small.max_width = 8;
        small.min_width = 1;
        assert!(!unsafe { cuda_offers_compatible(&producer, &offer(2, &small, &event)) });
        let unknown = capabilities(0, CUDA_FORMAT_X2B10G10R10);
        let unknown_event = CudaEventCapabilities {
            device_uuid: [0; 16],
        };
        let unknown_offer = offer(3, &unknown, &unknown_event);
        assert!(!unsafe { cuda_offers_compatible(&unknown_offer, &unknown_offer) });
        let mut fenced = offer(2, &resource, &event);
        fenced.completion_sync = cuda_descriptor(FRAME_SYNC_CUDA_EVENT, &event);
        assert!(!unsafe { cuda_offers_compatible(&producer, &fenced) });
    }

    #[test]
    fn foreign_or_malformed_descriptors_are_not_this_schema() {
        let resource = capabilities(7, CUDA_FORMAT_X2B10G10R10);
        let mut descriptor = cuda_descriptor(FRAME_RESOURCE_CUDA_DEVICE, &resource);
        assert!(unsafe { cuda_device_capabilities(&descriptor) }.is_some());
        descriptor.namespace[0] ^= 1;
        assert!(unsafe { cuda_device_capabilities(&descriptor) }.is_none());
        let mut descriptor = cuda_descriptor(FRAME_RESOURCE_CUDA_DEVICE, &resource);
        descriptor.version = 2;
        assert!(unsafe { cuda_device_capabilities(&descriptor) }.is_none());
        let mut descriptor = cuda_descriptor(FRAME_RESOURCE_CUDA_DEVICE, &resource);
        descriptor.size -= 1;
        assert!(unsafe { cuda_device_capabilities(&descriptor) }.is_none());
        let mut descriptor = cuda_descriptor(FRAME_RESOURCE_CUDA_DEVICE, &resource);
        descriptor.data = std::ptr::null();
        assert!(unsafe { cuda_device_capabilities(&descriptor) }.is_none());
    }
}
