//! Exact cadence helpers for the standard encoder common-config schema.
use super::{FormatId, provider::SessionConfig};
use amaterasu_media::{
    ENCODER_FORMAT_NAMESPACE_STANDARD, ENCODER_FORMAT_SCHEMA_V1, ENCODER_SCHEMA_COMMON_CONFIG,
    EncoderCommonConfigV1, encoder_common_config_valid,
};
use std::{
    mem::{size_of, size_of_val},
    ptr,
};

/// A positive frame rate represented without rounding.
///
/// Use [`FrameCadence::from_fps`] for legacy integer FPS and
/// [`FrameCadence::from_millihz`] when the requested cadence has millihertz
/// precision. The ratio is kept as authored so integer callers remain `n/1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCadence {
    numerator: u32,
    denominator: u32,
}

impl FrameCadence {
    /// Creates a positive rational cadence in frames per second.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, CommonConfigError> {
        if numerator == 0 || denominator == 0 {
            return Err(CommonConfigError::InvalidCadence);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Retains the legacy integer FPS representation as `fps/1`.
    pub fn from_fps(fps: u32) -> Result<Self, CommonConfigError> {
        Self::new(fps, 1)
    }

    /// Represents an exact milliHz value as `milli_hz/1000` frames per second.
    pub fn from_millihz(milli_hz: u32) -> Result<Self, CommonConfigError> {
        Self::new(milli_hz, 1_000)
    }

    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// Returns milliHz only when the rational has an exact u32 milliHz value.
    pub fn millihz(self) -> Option<u32> {
        let scaled = u64::from(self.numerator).checked_mul(1_000)?;
        let denominator = u64::from(self.denominator);
        if !scaled.is_multiple_of(denominator) {
            return None;
        }
        u32::try_from(scaled / denominator).ok()
    }
}

/// A validated authoring and reader view of `EncoderCommonConfigV1`.
///
/// The options bytes use the native ABI layout described by the standard
/// common-config schema. This is not a network serialization format.
#[derive(Clone, Copy)]
pub struct CommonConfigV1 {
    abi: EncoderCommonConfigV1,
}

impl CommonConfigV1 {
    pub fn from_abi(abi: EncoderCommonConfigV1) -> Result<Self, CommonConfigError> {
        if !encoder_common_config_valid(&abi) {
            return Err(CommonConfigError::InvalidConfig);
        }
        Ok(Self { abi })
    }

    pub const fn abi(&self) -> &EncoderCommonConfigV1 {
        &self.abi
    }

    pub fn into_abi(self) -> EncoderCommonConfigV1 {
        self.abi
    }

    pub fn cadence(&self) -> FrameCadence {
        // `from_abi` validated that both values are nonzero.
        FrameCadence {
            numerator: self.abi.fps_numerator,
            denominator: self.abi.fps_denominator,
        }
    }

    pub fn set_cadence(&mut self, cadence: FrameCadence) {
        self.abi.fps_numerator = cadence.numerator();
        self.abi.fps_denominator = cadence.denominator();
    }

    pub const fn options_schema() -> FormatId {
        FormatId {
            namespace_id: ENCODER_FORMAT_NAMESPACE_STANDARD,
            value: ENCODER_SCHEMA_COMMON_CONFIG,
            schema: ENCODER_FORMAT_SCHEMA_V1,
        }
    }

    /// Encodes the validated value for `SessionConfig.options`.
    pub fn to_options_bytes(&self) -> Vec<u8> {
        let size = size_of::<EncoderCommonConfigV1>();
        let mut bytes = vec![0_u8; size];
        macro_rules! write_field {
            ($field:ident) => {{
                let offset = std::mem::offset_of!(EncoderCommonConfigV1, $field);
                let source = ptr::addr_of!(self.abi.$field).cast::<u8>();
                // SAFETY: the ABI offset and field size address the matching
                // initialized field in the source and the zeroed buffer spans
                // the complete ABI object. Field bytes contain no padding.
                unsafe {
                    ptr::copy_nonoverlapping(
                        source,
                        bytes.as_mut_ptr().add(offset),
                        size_of_val(&self.abi.$field),
                    );
                }
            }};
        }
        write_field!(version);
        write_field!(size);
        write_field!(flags);
        write_field!(reserved);
        write_field!(coded_width);
        write_field!(coded_height);
        write_field!(crop_x);
        write_field!(crop_y);
        write_field!(visible_width);
        write_field!(visible_height);
        write_field!(fps_numerator);
        write_field!(fps_denominator);
        write_field!(bitrate_bps);
        write_field!(profile);
        write_field!(level);
        write_field!(chroma);
        write_field!(bit_depth);
        write_field!(color);
        write_field!(output_clock);
        write_field!(output_clock_origin_us);
        bytes
    }

    /// Reads the native v1 ABI value from a standard common-config options body.
    pub fn from_options_bytes(bytes: &[u8]) -> Result<Self, CommonConfigError> {
        if bytes.len() != size_of::<EncoderCommonConfigV1>() {
            return Err(CommonConfigError::InvalidSize);
        }
        // SAFETY: size is exact, all field types accept every bit pattern, and
        // read_unaligned handles byte-slice alignment. Validation checks the
        // version, size and semantic invariants before exposing the value.
        let abi = unsafe {
            bytes
                .as_ptr()
                .cast::<EncoderCommonConfigV1>()
                .read_unaligned()
        };
        Self::from_abi(abi)
    }
}

impl SessionConfig {
    /// Reads standard common-config options when that schema was negotiated.
    /// Unknown schemas remain opaque and return `Ok(None)`.
    pub fn common_config_v1(&self) -> Result<Option<CommonConfigV1>, CommonConfigError> {
        if self.options_schema != CommonConfigV1::options_schema() {
            return Ok(None);
        }
        CommonConfigV1::from_options_bytes(&self.options).map(Some)
    }

    /// Authors the standard common-config options without changing the other
    /// negotiated session fields.
    pub fn set_common_config_v1(&mut self, config: &CommonConfigV1) {
        self.options_schema = CommonConfigV1::options_schema();
        self.options = config.to_options_bytes();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommonConfigError {
    InvalidCadence,
    InvalidConfig,
    InvalidSize,
}

impl std::fmt::Display for CommonConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidCadence => "frame cadence must be a positive rational",
            Self::InvalidConfig => "invalid encoder common configuration",
            Self::InvalidSize => "invalid encoder common configuration size",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CommonConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_media::{
        EncoderCommonConfigV1, FRAME_CLOCK_STREAM_RELATIVE, FrameClockMetadata, FrameColorMetadata,
    };

    fn common_config(cadence: FrameCadence) -> CommonConfigV1 {
        CommonConfigV1::from_abi(EncoderCommonConfigV1 {
            version: 1,
            size: size_of::<EncoderCommonConfigV1>() as u32,
            flags: 0,
            reserved: 0,
            coded_width: 1920,
            coded_height: 1080,
            crop_x: 0,
            crop_y: 0,
            visible_width: 1920,
            visible_height: 1080,
            fps_numerator: cadence.numerator(),
            fps_denominator: cadence.denominator(),
            bitrate_bps: 0,
            profile: 0,
            level: 0,
            chroma: 0,
            bit_depth: 0,
            color: FrameColorMetadata::default(),
            output_clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [7; 16],
            },
            output_clock_origin_us: 0,
        })
        .unwrap()
    }

    #[test]
    fn integer_and_millihz_cadences_keep_exact_legacy_and_fractional_forms() {
        let integer = FrameCadence::from_fps(60).unwrap();
        assert_eq!((integer.numerator(), integer.denominator()), (60, 1));
        assert_eq!(integer.millihz(), Some(60_000));

        let fractional = FrameCadence::from_millihz(59_940).unwrap();
        assert_eq!(
            (fractional.numerator(), fractional.denominator()),
            (59_940, 1_000)
        );
        assert_eq!(fractional.millihz(), Some(59_940));

        let repeating = FrameCadence::new(1, 3).unwrap();
        assert_eq!(repeating.millihz(), None);
        assert!(FrameCadence::new(0, 1).is_err());
        assert!(FrameCadence::new(1, 0).is_err());
    }

    #[test]
    fn common_config_v1_layout_has_no_uninitialized_padding() {
        assert_eq!(size_of::<FrameColorMetadata>(), 24);
        assert_eq!(size_of::<FrameClockMetadata>(), 32);
        assert_eq!(size_of::<EncoderCommonConfigV1>(), 136);
        assert_eq!(
            std::mem::offset_of!(EncoderCommonConfigV1, fps_numerator),
            40
        );
        assert_eq!(std::mem::offset_of!(EncoderCommonConfigV1, bitrate_bps), 48);
        assert_eq!(std::mem::offset_of!(EncoderCommonConfigV1, color), 72);
        assert_eq!(
            std::mem::offset_of!(EncoderCommonConfigV1, output_clock),
            96
        );
        assert_eq!(
            std::mem::offset_of!(EncoderCommonConfigV1, output_clock_origin_us),
            128
        );
    }

    #[test]
    fn session_config_reads_and_authors_standard_common_config() {
        let mut config = SessionConfig {
            mode: super::super::SessionMode::Source,
            codec: FormatId::default(),
            framing: FormatId::default(),
            source_id: 4,
            config_generation: 2,
            options_schema: FormatId::default(),
            options: Vec::new(),
        };
        assert!(config.common_config_v1().unwrap().is_none());

        let authored = common_config(FrameCadence::from_millihz(59_940).unwrap());
        config.set_common_config_v1(&authored);
        let read = config.common_config_v1().unwrap().unwrap();
        assert_eq!(read.cadence(), authored.cadence());
        assert_eq!(read.cadence().millihz(), Some(59_940));
        assert_eq!(read.abi().coded_width, 1920);
    }

    #[test]
    fn malformed_standard_config_is_rejected_without_claiming_unknown_schemas() {
        let mut config = SessionConfig {
            mode: super::super::SessionMode::Source,
            codec: FormatId::default(),
            framing: FormatId::default(),
            source_id: 4,
            config_generation: 2,
            options_schema: CommonConfigV1::options_schema(),
            options: vec![0; size_of::<EncoderCommonConfigV1>() - 1],
        };
        assert!(matches!(
            config.common_config_v1(),
            Err(CommonConfigError::InvalidSize)
        ));
        config.options_schema.value += 1;
        assert!(config.common_config_v1().unwrap().is_none());
    }
}
