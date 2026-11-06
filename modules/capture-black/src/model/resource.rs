//! Stable legacy offers plus explicit color intent for independently selected
//! SDR ranges and HDR. Choosing an offer selects real generated code values.
use amaterasu_media::*;

#[derive(Clone, Copy)]
pub struct ResourceMode {
    pub id: u64,
    pub format: u32,
    pub color: Option<FrameColorMetadata>,
    pub full_range: bool,
}
const fn color(hdr: bool, full: bool, rgb: bool) -> FrameColorMetadata {
    FrameColorMetadata {
        version: 1,
        size: size_of::<FrameColorMetadata>() as u32,
        primaries: if hdr {
            FRAME_PRIMARIES_BT2020
        } else {
            FRAME_PRIMARIES_BT709
        },
        transfer: if hdr {
            FRAME_TRANSFER_PQ
        } else if rgb {
            FRAME_TRANSFER_SRGB
        } else {
            FRAME_TRANSFER_BT709
        },
        matrix: if rgb {
            FRAME_MATRIX_RGB
        } else if hdr {
            FRAME_MATRIX_BT2020_NCL
        } else {
            FRAME_MATRIX_BT709
        },
        range: if full {
            FRAME_RANGE_FULL
        } else {
            FRAME_RANGE_LIMITED
        },
    }
}
pub const MODES: [ResourceMode; 7] = [
    ResourceMode {
        id: 1,
        format: PIXEL_NV12,
        color: None,
        full_range: false,
    },
    ResourceMode {
        id: 2,
        format: PIXEL_P010,
        color: None,
        full_range: false,
    },
    ResourceMode {
        id: 3,
        format: PIXEL_NV12,
        color: Some(color(false, false, false)),
        full_range: false,
    },
    ResourceMode {
        id: 4,
        format: PIXEL_P010,
        color: Some(color(false, false, false)),
        full_range: false,
    },
    ResourceMode {
        id: 5,
        format: PIXEL_P010,
        color: Some(color(false, true, false)),
        full_range: true,
    },
    ResourceMode {
        id: 6,
        format: PIXEL_P010,
        color: Some(color(true, false, false)),
        full_range: false,
    },
    ResourceMode {
        id: 7,
        format: PIXEL_BGRA8,
        color: Some(color(false, true, true)),
        full_range: true,
    },
];
impl ResourceMode {
    pub fn find(id: u64) -> Option<Self> {
        MODES.iter().copied().find(|mode| mode.id == id)
    }
    pub fn schema(self) -> u32 {
        if self.color.is_some() { 2 } else { 1 }
    }
}
