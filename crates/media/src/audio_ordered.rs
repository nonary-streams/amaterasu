//! Ordered PCM lanes and fixed-size encoded offers, in distinct v2 families.
use crate::{
    AUDIO_PACKET_END_OF_STREAM, AudioFormatDescriptor, AudioFrame, MAX_ENCODED_AUDIO_BYTES,
    NegotiatedAudioPacket, NegotiatedAudioPacketSink,
};
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::{ffi::c_void, mem::size_of};

pub const AUDIO_ORDERED_VERSION: u32 = 2;

/// Each occupied lane is one speaker bit; the lanes are in interleaved sample
/// order. Unused entries are zero. A set-valued mask alone cannot infer order.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioChannelLayoutV2 {
    pub size: u32,
    pub version: u32,
    pub channels: u32,
    pub reserved: u32,
    pub mask: u64,
    pub lanes: [u64; 8],
}

impl AudioChannelLayoutV2 {
    pub fn validate(&self) -> Result<(), Status> {
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_ORDERED_VERSION
            || !(1..=8).contains(&self.channels)
            || self.reserved != 0
            || self.mask.count_ones() != self.channels
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut observed = 0u64;
        for (index, lane) in self.lanes.iter().copied().enumerate() {
            if index < self.channels as usize {
                if !lane.is_power_of_two() || lane & self.mask == 0 || observed & lane != 0 {
                    return Err(STATUS_INVALID_ARGUMENT);
                }
                observed |= lane;
            } else if lane != 0 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        }
        if observed != self.mask {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

/// A callback-scoped PCM frame and the capture stream generation that owns its
/// lane map. The provider must increment generation whenever that map changes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrderedAudioFrameV2 {
    pub size: u32,
    pub version: u32,
    pub stream: u64,
    pub generation: u64,
    pub frame: AudioFrame,
    pub layout: AudioChannelLayoutV2,
}

impl OrderedAudioFrameV2 {
    pub fn validate(&self) -> Result<(), Status> {
        self.frame.validate()?;
        self.layout.validate()?;
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_ORDERED_VERSION
            || self.stream == 0
            || self.generation == 0
            || self.frame.channels != self.layout.channels
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OrderedAudioSinkV2 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const OrderedAudioFrameV2) -> Status,
}

/// Separate native family. Callback rejection retains the identical frame;
/// pointers and samples are borrowed only during the synchronous callback.
#[repr(C)]
pub struct OrderedAudioCaptureApiV2 {
    pub base: NativeInterface,
    pub read_audio: unsafe extern "C" fn(*mut c_void, u64, OrderedAudioSinkV2) -> Status,
}

/// Exact capture resource and lane map selected for one encoder stream.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioInputSelectionV2 {
    pub size: u32,
    pub version: u32,
    pub stream: u64,
    pub generation: u64,
    pub layout: AudioChannelLayoutV2,
}

impl AudioInputSelectionV2 {
    pub fn validate_frame(&self, frame: &OrderedAudioFrameV2) -> Result<(), Status> {
        frame.validate()?;
        self.layout.validate()?;
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_ORDERED_VERSION
            || self.stream == 0
            || self.generation == 0
            || self.stream != frame.stream
            || self.generation != frame.generation
            || self.layout != frame.layout
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

/// Offer layout is the encoded channel order. `fixed_packet_bytes == 0`
/// denotes variable size, never an implicit fixed-size guarantee. The exact
/// nonzero size and layout are bound to the enumerated offer generation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatDescriptorV2 {
    pub size: u32,
    pub version: u32,
    pub format: AudioFormatDescriptor,
    pub layout: AudioChannelLayoutV2,
    pub fixed_packet_bytes: u32,
    pub reserved: u32,
}

impl AudioFormatDescriptorV2 {
    pub fn validate(&self) -> Result<(), Status> {
        self.format.validate()?;
        self.layout.validate()?;
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_ORDERED_VERSION
            || self.format.channels != self.layout.channels
            || self.format.channel_mask != self.layout.mask
            || self.fixed_packet_bytes as usize > MAX_ENCODED_AUDIO_BYTES
            || self.reserved != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// Check an exact encoded length when the selected offer promises one.
    pub fn validate_packet(
        &self,
        packet: &NegotiatedAudioPacket,
        stream: u64,
        generation: u64,
        offer: u64,
    ) -> Result<(), Status> {
        self.validate()?;
        packet.validate_for(stream, generation, offer, &self.format)?;
        if packet.flags != AUDIO_PACKET_END_OF_STREAM
            && self.fixed_packet_bytes != 0
            && packet.data.len != self.fixed_packet_bytes as usize
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatOfferV2 {
    pub id: u64,
    pub format: AudioFormatDescriptorV2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatOfferSinkV2 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const AudioFormatOfferV2) -> Status,
}

/// Separate encoder family; v1 and legacy Opus tables are unchanged. Configure
/// binds an exact output offer to the supplied capture stream, generation and
/// ordered input layout. The provider must reject unsupported layouts and
/// interpret each lane according to this explicit selection. Submit borrows
/// one ordered capture frame synchronously.
#[repr(C)]
pub struct OrderedAudioEncoderApiV2 {
    pub base: NativeInterface,
    pub enumerate:
        unsafe extern "C" fn(*mut c_void, u64, AudioFormatOfferSinkV2, *mut u64) -> Status,
    pub configure:
        unsafe extern "C" fn(*mut c_void, u64, u64, u64, *const AudioInputSelectionV2) -> Status,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const OrderedAudioFrameV2) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, NegotiatedAudioPacketSink) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

/// The encoder offer and stream-relative timestamp epoch authenticated by
/// trusted session composition. The format and configuration are borrowed
/// during probe/prepare only. The transport copies any retained state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioTransportSelectionV2 {
    pub size: u32,
    pub version: u32,
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub clock_epoch: [u8; 16],
    pub format: AudioFormatDescriptorV2,
}

impl AudioTransportSelectionV2 {
    pub fn validate(&self) -> Result<(), Status> {
        self.format.validate()?;
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_ORDERED_VERSION
            || self.stream == 0
            || self.generation == 0
            || self.offer_id == 0
            || self.clock_epoch == [0; 16]
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

/// Separate negotiated audio transport family. Probe is read-only; prepare
/// commits exactly one preflighted selection before audio, FEC or cipher state
/// changes. Submit is synchronous: BUSY consumes no packet or state and the
/// caller retains its immutable packet for retry. Format support and fixed vs
/// variable length are explicit provider capabilities, never implied by IDs.
#[repr(C)]
pub struct OrderedAudioTransportApiV2 {
    pub base: NativeInterface,
    pub probe: unsafe extern "C" fn(*mut c_void, u64, *const AudioTransportSelectionV2) -> Status,
    pub prepare: unsafe extern "C" fn(*mut c_void, u64, *const AudioTransportSelectionV2) -> Status,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const NegotiatedAudioPacket) -> Status,
}

#[cfg(test)]
mod tests;
