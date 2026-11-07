//! Capture intent and independently observed desktop/texture color facts.
use crate::model::{CaptureError, CaptureResult};
use amaterasu_module_sdk::{
    DynamicRange,
    capture::{SourceFormat, high_depth::RgbEncoding},
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureFormat {
    pub ten_bit: bool,
    pub hdr: bool,
    pub explicit: bool,
}
impl CaptureFormat {
    pub fn requested(format: Option<&SourceFormat>) -> CaptureResult<Self> {
        match format.map(|f| (f.bit_depth, f.dynamic_range)) {
            None => Ok(Self::default()),
            Some((8, DynamicRange::Sdr)) => Ok(Self {
                explicit: true,
                ..Self::default()
            }),
            Some((10, DynamicRange::Sdr)) => Ok(Self {
                ten_bit: true,
                hdr: false,
                explicit: true,
            }),
            Some((10, DynamicRange::Hdr)) => Ok(Self {
                ten_bit: true,
                hdr: true,
                explicit: true,
            }),
            _ => Err(CaptureError::new(
                "unsupported_format",
                "DXGI supports eight-bit SDR and ten-bit SDR or HDR",
            )),
        }
    }
    pub fn validate_dimensions(self, width: u32, height: u32) -> CaptureResult<()> {
        if self.ten_bit {
            let raw_bytes = u64::from(width) * u64::from(height) * 8;
            if raw_bytes > 2 * amaterasu_media::MAX_FRAME_BYTES as u64 {
                return Err(CaptureError::new(
                    "unsupported_format",
                    "high-depth source exceeds the bounded RGB readback budget",
                ));
            }
            amaterasu_media::frame_size(
                width,
                height,
                amaterasu_media::PIXEL_P010,
                width.checked_mul(2).ok_or_else(|| {
                    CaptureError::new("unsupported_format", "capture width overflow")
                })?,
            )
            .map_err(|_| {
                CaptureError::new(
                    "unsupported_format",
                    "P010 requires bounded even source dimensions",
                )
            })?;
        } else {
            crate::model::frame_layout(width, height)?;
        }
        Ok(())
    }
}
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopColor {
    pub color_space: i32,
    pub bits_per_color: u32,
}
#[cfg(any(windows, test))]
impl DesktopColor {
    pub fn hdr(self) -> CaptureResult<bool> {
        match (self.color_space, self.bits_per_color) {
            (0, 8..=16) => Ok(false),
            (12, 10..=16) => Ok(true),
            _ => Err(CaptureError::new(
                "source_color_unavailable",
                "native scan-out color space or precision is not verified",
            )),
        }
    }
    pub fn verify(self, format: CaptureFormat) -> CaptureResult<()> {
        if self.hdr()? != format.hdr && format.explicit {
            return Err(CaptureError::new(
                "source_range_mismatch",
                "capture source dynamic range differs from requested range",
            ));
        }
        Ok(())
    }
    pub fn encoding(self, format: u32, requested: CaptureFormat) -> CaptureResult<RgbEncoding> {
        self.verify(requested)?;
        let encoding = match format {
            10 => RgbEncoding::Rgba16FloatScRgb,
            87 if !requested.ten_bit => RgbEncoding::Bgra8Srgb,
            _ => {
                return Err(CaptureError::new(
                    "unsupported_format",
                    "DXGI texture does not preserve the requested source precision",
                ));
            }
        };
        Ok(encoding)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub width: u32,
    pub height: u32,
    pub format: CaptureFormat,
    pub encoding: RgbEncoding,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_texture_precision_and_native_range_are_both_required() {
        let sdr = CaptureFormat {
            ten_bit: true,
            hdr: false,
            explicit: true,
        };
        let hdr = CaptureFormat {
            ten_bit: true,
            hdr: true,
            explicit: true,
        };
        let ordinary = DesktopColor {
            color_space: 0,
            bits_per_color: 8,
        };
        let advanced = DesktopColor {
            color_space: 12,
            bits_per_color: 10,
        };
        assert_eq!(
            ordinary.encoding(10, sdr).unwrap(),
            RgbEncoding::Rgba16FloatScRgb
        );
        assert_eq!(
            advanced.encoding(10, hdr).unwrap(),
            RgbEncoding::Rgba16FloatScRgb
        );
        // An original RGB10 fullscreen buffer's transfer cannot be inferred
        // from the scan-out alone; DXGI must convert it to explicit scRGB.
        assert!(advanced.encoding(24, hdr).is_err());
        assert!(ordinary.encoding(87, sdr).is_err());
        assert!(ordinary.encoding(10, hdr).is_err());
        assert!(advanced.encoding(24, sdr).is_err());
        assert!(
            DesktopColor {
                color_space: 1,
                bits_per_color: 16
            }
            .encoding(10, hdr)
            .is_err()
        );
        assert!(sdr.validate_dimensions(1920, 1080).is_ok());
        assert!(sdr.validate_dimensions(1921, 1080).is_err());
        assert!(
            CaptureFormat::requested(Some(&SourceFormat {
                bit_depth: 8,
                dynamic_range: DynamicRange::Hdr
            }))
            .is_err()
        );
    }
}
