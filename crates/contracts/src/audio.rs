//! Decoder configuration supplied by the selected audio encoder.

/// Opus multistream configuration for a 5.1 or 7.1 GameStream session.
///
/// Each mapping entry selects the coded channel for the corresponding WAVE
/// speaker: FL, FR, FC, LFE, BL, BR, then SL and SR for 7.1.
/// Encoders must keep the layout stable for the lifetime of an open stream,
/// including recovery of the same handle. These fields appear at the top level
/// of the legacy Opus open response alongside codec, channels and packet sizing.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpusSurroundLayout {
    pub streams: u8,
    pub coupled_streams: u8,
    pub wave_channel_mapping: Vec<u8>,
}

impl OpusSurroundLayout {
    /// GameStream needs one distinct coded channel per speaker. Stream counts
    /// and mapping indices must also fit Moonlight's single-digit SDP fields.
    pub fn validate(&self, channels: u32) -> Result<(), &'static str> {
        if !matches!(channels, 6 | 8)
            || self.streams == 0
            || self.coupled_streams > self.streams
            || u32::from(self.streams) + u32::from(self.coupled_streams) != channels
            || self.wave_channel_mapping.len() != channels as usize
        {
            return Err("invalid_opus_surround_layout");
        }
        let mut seen = 0u16;
        for &index in &self.wave_channel_mapping {
            if u32::from(index) >= channels || seen & (1 << index) != 0 {
                return Err("invalid_opus_surround_layout");
            }
            seen |= 1 << index;
        }
        Ok(())
    }
}
