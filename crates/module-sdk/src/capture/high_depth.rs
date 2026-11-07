//! Bounded CPU conversion of explicitly identified RGB capture surfaces.
//! Float surfaces use Windows scRGB: linear BT.709, with 1.0 equal to reference
//! white for SDR and 80 nits for HDR. This module never guesses a surface's
//! encoding, changes the display, or performs HDR-to-SDR tone mapping.
use crate::abi::{STATUS_INVALID_ARGUMENT, Status};
use amaterasu_media::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgbEncoding {
    Bgra8Srgb,
    Rgba16FloatScRgb,
    Rgb10A2Srgb,
    Rgb10A2Pq2020,
}

impl RgbEncoding {
    pub fn bytes_per_pixel(self) -> u32 {
        if self == Self::Rgba16FloatScRgb { 8 } else { 4 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvRange {
    Full,
    Limited,
}

#[derive(Clone)]
pub struct RgbSurface {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub encoding: RgbEncoding,
    /// Owned initialized rows. Padding is permitted but is never interpreted.
    pub bytes: Arc<Vec<u8>>,
}

pub struct ConvertedPixels {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
    pub color: FrameColorMetadata,
    pub bytes: Arc<Vec<u8>>,
}

pub fn p010_color(hdr: bool, range: YuvRange) -> FrameColorMetadata {
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
        } else {
            FRAME_TRANSFER_BT709
        },
        matrix: if hdr {
            FRAME_MATRIX_BT2020_NCL
        } else {
            FRAME_MATRIX_BT709
        },
        range: if range == YuvRange::Full {
            FRAME_RANGE_FULL
        } else {
            FRAME_RANGE_LIMITED
        },
    }
}

impl RgbSurface {
    pub fn validate(&self) -> Result<(), Status> {
        let row = self
            .width
            .checked_mul(self.encoding.bytes_per_pixel())
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        let minimum = (self.height as usize)
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(self.stride as usize))
            .and_then(|offset| offset.checked_add(row as usize))
            .ok_or(STATUS_INVALID_ARGUMENT)?;
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || self.stride < row
            || self.bytes.len() < minimum
            || self.bytes.len() > 2 * MAX_FRAME_BYTES
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// Return tightly packed 10-bit 4:2:0 pixels, preserving ten meaningful
    /// bits before the standard P010 six-bit storage shift. `hdr` is a proven
    /// source property, not a requested output label.
    pub fn to_p010(&self, hdr: bool, range: YuvRange) -> Result<ConvertedPixels, Status> {
        self.validate()?;
        if !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || (hdr
                && matches!(
                    self.encoding,
                    RgbEncoding::Bgra8Srgb | RgbEncoding::Rgb10A2Srgb
                ))
            || (!hdr && self.encoding == RgbEncoding::Rgb10A2Pq2020)
        {
            return Err(MEDIA_UNSUPPORTED);
        }
        let stride = self.width.checked_mul(2).ok_or(STATUS_INVALID_ARGUMENT)?;
        let length = frame_size(self.width, self.height, PIXEL_P010, stride)?;
        let mut bytes = vec![0; length];
        let chroma_start = (stride as usize) * (self.height as usize);
        for y in (0..self.height).step_by(2) {
            for x in (0..self.width).step_by(2) {
                let mut cb = 0.0;
                let mut cr = 0.0;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let rgb = self.encoded_rgb(x + dx, y + dy, hdr)?;
                        let (luma, u, v) = yuv(rgb, hdr);
                        let position =
                            ((y + dy) as usize) * (stride as usize) + ((x + dx) as usize) * 2;
                        write_sample(&mut bytes, position, quantize_luma(luma, range));
                        cb += u;
                        cr += v;
                    }
                }
                let position =
                    chroma_start + (y as usize / 2) * (stride as usize) + (x as usize) * 2;
                write_sample(&mut bytes, position, quantize_chroma(cb / 4.0, range));
                write_sample(&mut bytes, position + 2, quantize_chroma(cr / 4.0, range));
            }
        }
        Ok(ConvertedPixels {
            width: self.width,
            height: self.height,
            stride,
            pixel_format: PIXEL_P010,
            color: p010_color(hdr, range),
            bytes: Arc::new(bytes),
        })
    }

    /// Legacy SDR representation. Providers must not offer this conversion for
    /// a captured HDR presentation; no tone mapper is selected implicitly.
    pub fn to_bgra8(&self) -> Result<ConvertedPixels, Status> {
        self.validate()?;
        if self.encoding == RgbEncoding::Rgb10A2Pq2020 {
            return Err(MEDIA_UNSUPPORTED);
        }
        let stride = self.width.checked_mul(4).ok_or(STATUS_INVALID_ARGUMENT)?;
        let mut bytes = vec![0; frame_size(self.width, self.height, PIXEL_BGRA8, stride)?];
        for y in 0..self.height {
            for x in 0..self.width {
                let rgb = self.linear_709(x, y)?;
                let offset = (y as usize) * (stride as usize) + (x as usize) * 4;
                bytes[offset] = (srgb(rgb[2]) * 255.0).round() as u8;
                bytes[offset + 1] = (srgb(rgb[1]) * 255.0).round() as u8;
                bytes[offset + 2] = (srgb(rgb[0]) * 255.0).round() as u8;
                bytes[offset + 3] = 255;
            }
        }
        Ok(ConvertedPixels {
            width: self.width,
            height: self.height,
            stride,
            pixel_format: PIXEL_BGRA8,
            color: FrameColorMetadata::default(),
            bytes: Arc::new(bytes),
        })
    }

    /// Edit one initialized pixel in linear BT.709/scRGB units. This supports
    /// small native overlays without quantizing the rest of a high-depth
    /// presentation. HDR scRGB uses 80 nits per unit; the caller supplies any
    /// independently observed SDR reference-white scaling for its overlay.
    pub fn transform_linear_pixel(
        &mut self,
        x: u32,
        y: u32,
        transform: impl FnOnce([f64; 3]) -> [f64; 3],
    ) -> Result<(), Status> {
        self.validate()?;
        if x >= self.width || y >= self.height {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let rgb = transform(self.linear_709(x, y)?);
        if rgb.iter().any(|v| !v.is_finite()) {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let offset = y as usize * self.stride as usize
            + x as usize * self.encoding.bytes_per_pixel() as usize;
        let encoded = match self.encoding {
            RgbEncoding::Rgba16FloatScRgb => {
                let mut pixel = [0u8; 8];
                for (index, value) in rgb.into_iter().chain(std::iter::once(1.0)).enumerate() {
                    pixel[index * 2..index * 2 + 2].copy_from_slice(&to_half(value)?.to_le_bytes());
                }
                pixel.to_vec()
            }
            RgbEncoding::Bgra8Srgb => vec![
                (srgb(rgb[2]) * 255.0).round() as u8,
                (srgb(rgb[1]) * 255.0).round() as u8,
                (srgb(rgb[0]) * 255.0).round() as u8,
                255,
            ],
            RgbEncoding::Rgb10A2Srgb | RgbEncoding::Rgb10A2Pq2020 => {
                let encoded = if self.encoding == RgbEncoding::Rgb10A2Srgb {
                    rgb.map(srgb)
                } else {
                    linear_709_to_2020(rgb).map(|v| pq(v * 80.0))
                };
                let values = encoded.map(|v| (v * 1023.0).round().clamp(0.0, 1023.0) as u32);
                (values[0] | values[1] << 10 | values[2] << 20 | 3 << 30)
                    .to_le_bytes()
                    .to_vec()
            }
        };
        Arc::make_mut(&mut self.bytes)[offset..offset + encoded.len()].copy_from_slice(&encoded);
        Ok(())
    }

    fn encoded_rgb(&self, x: u32, y: u32, hdr: bool) -> Result<[f64; 3], Status> {
        if self.encoding == RgbEncoding::Rgb10A2Pq2020 {
            return Ok(self.packed_rgb(x, y));
        }
        let rgb = self.linear_709(x, y)?;
        if hdr {
            // Linear BT.709 to BT.2020, then absolute ST.2084. Windows HDR
            // scRGB white is 80 nits; SDR scRGB is display-referred instead.
            let wide = linear_709_to_2020(rgb);
            Ok(wide.map(|value| pq(value * 80.0)))
        } else {
            Ok(rgb.map(bt709))
        }
    }

    fn linear_709(&self, x: u32, y: u32) -> Result<[f64; 3], Status> {
        let offset = (y as usize) * (self.stride as usize)
            + (x as usize) * (self.encoding.bytes_per_pixel() as usize);
        match self.encoding {
            RgbEncoding::Bgra8Srgb => Ok([
                self.bytes[offset + 2],
                self.bytes[offset + 1],
                self.bytes[offset],
            ]
            .map(|value| inverse_srgb(f64::from(value) / 255.0))),
            RgbEncoding::Rgba16FloatScRgb => {
                let mut rgb = [0.0; 3];
                for (component, value) in rgb.iter_mut().enumerate() {
                    let index = offset + component * 2;
                    *value = half(u16::from_le_bytes([
                        self.bytes[index],
                        self.bytes[index + 1],
                    ]))?;
                }
                Ok(rgb)
            }
            RgbEncoding::Rgb10A2Srgb => Ok(self.packed_rgb(x, y).map(inverse_srgb)),
            RgbEncoding::Rgb10A2Pq2020 => {
                let rgb = self.packed_rgb(x, y).map(|v| inverse_pq(v) / 80.0);
                Ok([
                    1.660491 * rgb[0] - 0.587641 * rgb[1] - 0.072850 * rgb[2],
                    -0.124550 * rgb[0] + 1.132900 * rgb[1] - 0.008349 * rgb[2],
                    -0.018151 * rgb[0] - 0.100579 * rgb[1] + 1.118730 * rgb[2],
                ])
            }
        }
    }

    fn packed_rgb(&self, x: u32, y: u32) -> [f64; 3] {
        let offset = (y as usize) * (self.stride as usize) + (x as usize) * 4;
        let pixel = u32::from_le_bytes(
            self.bytes[offset..offset + 4]
                .try_into()
                .expect("validated pixel"),
        );
        [
            f64::from(pixel & 1023) / 1023.0,
            f64::from((pixel >> 10) & 1023) / 1023.0,
            f64::from((pixel >> 20) & 1023) / 1023.0,
        ]
    }
}

fn linear_709_to_2020(rgb: [f64; 3]) -> [f64; 3] {
    [
        0.627404 * rgb[0] + 0.329283 * rgb[1] + 0.043313 * rgb[2],
        0.069097 * rgb[0] + 0.919540 * rgb[1] + 0.011362 * rgb[2],
        0.016392 * rgb[0] + 0.088013 * rgb[1] + 0.895595 * rgb[2],
    ]
}
fn inverse_pq(value: f64) -> f64 {
    let powered = value.clamp(0.0, 1.0).powf(32.0 / 2523.0);
    ((powered - 3424.0 / 4096.0).max(0.0) / (2413.0 / 128.0 - 2392.0 / 128.0 * powered))
        .max(0.0)
        .powf(16384.0 / 2610.0)
        * 10_000.0
}
fn to_half(value: f64) -> Result<u16, Status> {
    let f = value as f32;
    if !f.is_finite() {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 255) as i32 - 127 + 15;
    let fraction = bits & 0x7fffff;
    if exponent >= 31 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    if exponent <= 0 {
        if exponent < -10 {
            return Ok(sign);
        }
        let mantissa = fraction | 0x800000;
        let shift = (14 - exponent) as u32;
        let halfway = 1u32 << (shift - 1);
        let rounded = (mantissa + halfway - 1 + ((mantissa >> shift) & 1)) >> shift;
        return Ok(sign | rounded as u16);
    }
    let rounded = fraction + 0xfff + ((fraction >> 13) & 1);
    let packed = ((exponent as u32) << 10) + (rounded >> 13);
    if packed >= 0x7c00 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    Ok(sign | packed as u16)
}

fn half(value: u16) -> Result<f64, Status> {
    let sign = if value & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = (value >> 10) & 31;
    let mantissa = value & 1023;
    match exponent {
        31 => Err(STATUS_INVALID_ARGUMENT),
        0 => Ok(sign * f64::from(mantissa) / 16_777_216.0),
        _ => Ok(sign
            * f64::from(f32::from_bits(
                (u32::from(exponent) + 112) << 23 | u32::from(mantissa) << 13,
            ))),
    }
}
fn inverse_srgb(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}
fn bt709(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if value < 0.018 {
        4.5 * value
    } else {
        1.099 * value.powf(0.45) - 0.099
    }
}
fn pq(nits: f64) -> f64 {
    let normalized = (nits / 10_000.0).clamp(0.0, 1.0).powf(2610.0 / 16384.0);
    ((3424.0 / 4096.0 + (2413.0 / 128.0) * normalized) / (1.0 + (2392.0 / 128.0) * normalized))
        .powf(2523.0 / 32.0)
}
fn yuv(rgb: [f64; 3], hdr: bool) -> (f64, f64, f64) {
    let (kr, kb) = if hdr {
        (0.2627, 0.0593)
    } else {
        (0.2126, 0.0722)
    };
    let y = kr * rgb[0] + (1.0 - kr - kb) * rgb[1] + kb * rgb[2];
    (
        y,
        (rgb[2] - y) / (2.0 * (1.0 - kb)),
        (rgb[0] - y) / (2.0 * (1.0 - kr)),
    )
}
fn quantize_luma(value: f64, range: YuvRange) -> u16 {
    match range {
        YuvRange::Full => (value * 1023.0).round().clamp(0.0, 1023.0) as u16,
        YuvRange::Limited => (64.0 + value * 876.0).round().clamp(64.0, 940.0) as u16,
    }
}
fn quantize_chroma(value: f64, range: YuvRange) -> u16 {
    match range {
        YuvRange::Full => (512.0 + value * 1023.0).round().clamp(0.0, 1023.0) as u16,
        YuvRange::Limited => (512.0 + value * 896.0).round().clamp(64.0, 960.0) as u16,
    }
}
fn write_sample(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&(value << 6).to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn float_surface(value: u16) -> RgbSurface {
        let mut bytes = Vec::new();
        for _ in 0..4 {
            for component in [value, value, value, 0x3c00] {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        RgbSurface {
            width: 2,
            height: 2,
            stride: 16,
            encoding: RgbEncoding::Rgba16FloatScRgb,
            bytes: Arc::new(bytes),
        }
    }
    fn sample(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) >> 6
    }
    #[test]
    fn p010_black_white_and_neutral_chroma_use_exact_10bit_storage() {
        for (raw, limited, full) in [(0, 64, 0), (0x3c00, 940, 1023)] {
            let surface = float_surface(raw);
            for (range, expected) in [(YuvRange::Limited, limited), (YuvRange::Full, full)] {
                let output = surface.to_p010(false, range).unwrap();
                assert_eq!(output.stride, 4);
                assert_eq!(output.bytes.len(), 12);
                for offset in [0, 2, 4, 6] {
                    assert_eq!(sample(&output.bytes, offset), expected);
                }
                assert_eq!(sample(&output.bytes, 8), 512);
                assert_eq!(sample(&output.bytes, 10), 512);
                assert!(
                    output
                        .bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .all(|word| u16::from_le_bytes([word[0], word[1]]) & 63 == 0)
                );
            }
        }
    }
    #[test]
    fn float_precision_survives_ten_bit_sdr_and_hdr_is_not_clipped_to_sdr_white() {
        let levels: (Vec<u16>, Vec<u16>) = (
            (0x3000..0x3400)
                .map(|value| {
                    sample(
                        &float_surface(value)
                            .to_p010(false, YuvRange::Full)
                            .unwrap()
                            .bytes,
                        0,
                    )
                })
                .collect(),
            (0x3000..0x3400)
                .map(|value| {
                    sample(
                        &float_surface(value)
                            .to_p010(true, YuvRange::Limited)
                            .unwrap()
                            .bytes,
                        0,
                    )
                })
                .collect(),
        );
        assert!(levels.0.iter().any(|value| !value.is_multiple_of(4)));
        assert!(levels.0.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(levels.1.windows(2).all(|pair| pair[0] <= pair[1]));
        let reference = sample(
            &float_surface(0x3c00)
                .to_p010(true, YuvRange::Limited)
                .unwrap()
                .bytes,
            0,
        );
        let thousand_nits = sample(
            &float_surface(0x4a40)
                .to_p010(true, YuvRange::Limited)
                .unwrap()
                .bytes,
            0,
        );
        assert!(reference > 400 && reference < 600);
        assert!(thousand_nits > reference);
        assert_eq!(
            float_surface(0x3c00).to_bgra8().unwrap().bytes.as_slice(),
            [255, 255, 255, 255].repeat(4)
        );
    }
    #[test]
    fn invalid_pixels_bounds_padding_and_wrong_range_provenance_fail_closed() {
        assert!(
            float_surface(0x7c00)
                .to_p010(true, YuvRange::Limited)
                .is_err()
        );
        assert!(
            float_surface(0x7e01)
                .to_p010(false, YuvRange::Limited)
                .is_err()
        );
        let mut invalid = float_surface(0);
        invalid.stride = 15;
        assert!(invalid.to_p010(false, YuvRange::Limited).is_err());
        let mut padded = float_surface(0x3c00);
        let mut rows = Vec::new();
        rows.extend_from_slice(&padded.bytes[..16]);
        rows.extend_from_slice(&[0xff; 8]);
        rows.extend_from_slice(&padded.bytes[16..]);
        padded.stride = 24;
        padded.bytes = Arc::new(rows);
        assert_eq!(
            sample(&padded.to_p010(false, YuvRange::Limited).unwrap().bytes, 0),
            940
        );
        let packed = RgbSurface {
            width: 2,
            height: 2,
            stride: 8,
            encoding: RgbEncoding::Rgb10A2Pq2020,
            bytes: Arc::new(vec![0; 16]),
        };
        assert!(packed.to_p010(false, YuvRange::Limited).is_err());
        assert!(packed.to_bgra8().is_err());
    }
}
