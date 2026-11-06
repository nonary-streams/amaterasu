//! Requested delivery and the actual WGC texture format are separate facts.
use crate::model::{CaptureError, CaptureResult};
use amaterasu_module_sdk::DynamicRange;
use amaterasu_module_sdk::capture::SourceFormat;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceEncoding {
    #[default]
    Bgra8,
    /// WGC's FP16 frame pool produces linear scRGB pixels; they remain float
    /// until the selected public resource format is converted for delivery.
    Rgba16FloatScRgb,
}

impl SourceEncoding {
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Bgra8 => 4,
            Self::Rgba16FloatScRgb => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureFormat {
    pub ten_bit: bool,
    pub hdr: bool,
    /// Explicit source-format admission requires native range proof even at
    /// eight bits. Omitted legacy requests keep their unknown-color contract.
    pub color_proof: bool,
}

impl CaptureFormat {
    pub fn requested(format: Option<&SourceFormat>) -> CaptureResult<Self> {
        let Some(format) = format else {
            return Ok(Self::default());
        };
        match (format.bit_depth, format.dynamic_range) {
            (8, DynamicRange::Sdr) => Ok(Self {
                color_proof: true,
                ..Self::default()
            }),
            (10, DynamicRange::Sdr) => Ok(Self {
                ten_bit: true,
                hdr: false,
                color_proof: true,
            }),
            (10, DynamicRange::Hdr) => Ok(Self {
                ten_bit: true,
                hdr: true,
                color_proof: true,
            }),
            _ => Err(CaptureError::new(
                "unsupported_format",
                "WGC supports eight-bit SDR and ten-bit SDR or HDR",
            )),
        }
    }

    pub fn encoding(self) -> SourceEncoding {
        if self.ten_bit {
            SourceEncoding::Rgba16FloatScRgb
        } else {
            SourceEncoding::Bgra8
        }
    }

    pub fn validate_dimensions(self, width: u32, height: u32) -> CaptureResult<()> {
        crate::model::source_frame_layout(width, height, self.encoding())?;
        if self.ten_bit {
            let stride = width
                .checked_mul(2)
                .ok_or_else(|| CaptureError::new("unsupported_format", "P010 width overflow"))?;
            amaterasu_media::frame_size(width, height, amaterasu_media::PIXEL_P010, stride)
                .map_err(|_| {
                    CaptureError::new(
                        "unsupported_format",
                        "ten-bit capture requires bounded even source dimensions",
                    )
                })?;
        } else {
            crate::model::frame_layout(width, height)?;
        }
        Ok(())
    }
}

/// Native source facts established independently of the encoder's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub width: u32,
    pub height: u32,
    pub encoding: SourceEncoding,
    pub desktop_hdr: bool,
}

/// A native DXGI scan-out color observation tied to the exact selected monitor.
/// FP16 texture provenance comes from WGC's frame-pool format, independently of
/// this observation. Unknown scan-out encodings never become a guessed range.
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
            (0, 8..=16) => Ok(false),  // RGB full-range G22 BT.709 scan-out.
            (12, 10..=16) => Ok(true), // RGB full-range ST.2084 BT.2020 scan-out.
            _ => Err(CaptureError::new(
                "source_color_unavailable",
                "native source color space or precision is not verified",
            )),
        }
    }
    pub fn verify(self, format: CaptureFormat) -> CaptureResult<()> {
        let hdr = self.hdr()?;
        if format.color_proof && format.hdr != hdr {
            return Err(CaptureError::new(
                "source_range_mismatch",
                "requested source range differs from native scan-out; explicit processing is required",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_depth_sdr_and_hdr_require_fp16_and_never_select_legacy_bgra() {
        for hdr in [false, true] {
            let format = SourceFormat {
                bit_depth: 10,
                dynamic_range: if hdr {
                    DynamicRange::Hdr
                } else {
                    DynamicRange::Sdr
                },
            };
            let capture = CaptureFormat::requested(Some(&format)).unwrap();
            assert_eq!(capture.encoding(), SourceEncoding::Rgba16FloatScRgb);
            assert_eq!(capture.hdr, hdr);
        }
        assert_eq!(
            CaptureFormat::requested(None).unwrap().encoding(),
            SourceEncoding::Bgra8
        );
        let invalid = SourceFormat {
            bit_depth: 8,
            dynamic_range: DynamicRange::Hdr,
        };
        assert!(CaptureFormat::requested(Some(&invalid)).is_err());
    }

    #[test]
    fn display_flags_alone_do_not_prove_hdr_and_range_changes_require_reopen() {
        let hdr = CaptureFormat {
            ten_bit: true,
            hdr: true,
            color_proof: true,
        };
        let sdr = CaptureFormat {
            ten_bit: true,
            hdr: false,
            color_proof: true,
        };
        let pq = DesktopColor {
            color_space: 12,
            bits_per_color: 10,
        };
        let ordinary = DesktopColor {
            color_space: 0,
            bits_per_color: 8,
        };
        assert!(pq.verify(hdr).is_ok());
        assert!(ordinary.verify(sdr).is_ok());
        assert!(pq.verify(sdr).is_err());
        assert!(ordinary.verify(hdr).is_err());
        assert!(
            DesktopColor {
                color_space: 1,
                bits_per_color: 16
            }
            .hdr()
            .is_err()
        );
        assert!(
            DesktopColor {
                color_space: 12,
                bits_per_color: 8
            }
            .hdr()
            .is_err()
        );
        assert!(sdr.validate_dimensions(1920, 1080).is_ok());
        assert!(sdr.validate_dimensions(1921, 1080).is_err());
        assert!(sdr.validate_dimensions(8192, 8192).is_err());
    }
}
