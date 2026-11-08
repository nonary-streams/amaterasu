//! Format-neutral audio packet delivery for independently negotiated formats.
use amaterasu_abi::{BorrowedBytes, NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

/// Upper bounds apply independently to packet payload and per-packet metadata.
pub const AUDIO_EXTENSIBLE_MAX_PAYLOAD: usize = 1024 * 1024;
pub const AUDIO_EXTENSIBLE_MAX_METADATA: usize = 64 * 1024;
pub const AUDIO_EXTENSIBLE_DISCONTINUITY: u32 = 1;
pub const AUDIO_EXTENSIBLE_END: u32 = 2;
pub const AUDIO_FORMAT_OPUS: AudioFormatId = AudioFormatId {
    namespace: 1,
    value: 2,
    version: 1,
};

/// All three parts are selected by the participants; unknown IDs are not
/// intrinsically invalid. Zero is reserved for an absent identifier.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioFormatId {
    pub namespace: u64,
    pub value: u64,
    pub version: u64,
}

impl AudioFormatId {
    /// Constructs an identifier in the shared standard namespace.
    pub const fn standard(value: u64) -> Self {
        Self {
            namespace: 1,
            value,
            version: 1,
        }
    }

    pub fn is_present(self) -> bool {
        self.namespace != 0 && self.value != 0 && self.version != 0
    }

    pub fn is_absent(self) -> bool {
        self.namespace == 0 && self.value == 0 && self.version == 0
    }
}

/// One complete negotiated audio unit, borrowed only until the sink returns.
/// `timestamp_us` and `duration_us` use the session's negotiated stream clock.
/// The metadata schema is independently negotiated and must accompany its bytes.
/// END marks the last unit; a format may carry final payload or metadata in it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioUnit {
    pub format: AudioFormatId,
    pub metadata_schema: AudioFormatId,
    pub flags: u32,
    pub timestamp_us: u64,
    pub duration_us: u64,
    pub sequence: u64,
    pub generation: u64,
    pub payload: BorrowedBytes,
    pub metadata: BorrowedBytes,
}

impl AudioUnit {
    /// Checks representation bounds, not pointer validity or negotiated support.
    pub fn validate(&self) -> Result<(), Status> {
        if !self.format.is_present()
            || self.generation == 0
            || self.payload.len > AUDIO_EXTENSIBLE_MAX_PAYLOAD
            || self.metadata.len > AUDIO_EXTENSIBLE_MAX_METADATA
            || (self.payload.len != 0 && self.payload.ptr.is_null())
            || (self.metadata.len != 0 && self.metadata.ptr.is_null())
            || !(self.metadata_schema.is_present() || self.metadata_schema.is_absent())
            || (self.metadata.len != 0 && !self.metadata_schema.is_present())
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
}

/// A successful callback accepts the entire unit locally; BUSY accepts nothing.
/// The callback must copy bytes it needs after returning and may not unwind.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioUnitSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const AudioUnit) -> Status,
}

/// A source of already encoded units. Drain stops new input and eventually
/// produces an END unit after all accepted data. The host invokes this instead
/// of opening an encoder for an explicitly negotiated encoded source.
#[repr(C)]
pub struct EncodedAudioSourceApi {
    pub base: NativeInterface,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, AudioUnitSink) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

impl AudioUnitSink {
    /// # Safety
    /// The callback, context and borrowed payloads remain live for this call.
    /// The callback must copy retained bytes and must not unwind across the ABI.
    pub unsafe fn send(&self, unit: &AudioUnit) -> Status {
        if let Err(status) = unit.validate() {
            return status;
        }
        // SAFETY: the caller guarantees the synchronous callback and buffer lifetimes.
        unsafe { (self.write)(self.context, unit) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn unit_matches_c_layout() {
        assert_eq!(std::mem::size_of::<AudioFormatId>(), 24);
        assert_eq!(std::mem::size_of::<AudioUnit>(), 120);
        assert_eq!(std::mem::offset_of!(AudioUnit, flags), 48);
        assert_eq!(std::mem::offset_of!(AudioUnit, payload), 88);
        assert_eq!(std::mem::size_of::<AudioUnitSink>(), 16);
    }

    #[test]
    fn arbitrary_negotiated_format_and_metadata_have_bounded_shapes() {
        let id = AudioFormatId {
            namespace: 42,
            value: 23,
            version: 1,
        };
        let absent = AudioFormatId {
            namespace: 0,
            value: 0,
            version: 0,
        };
        let bytes = [1, 2, 3];
        let unit = AudioUnit {
            generation: 1,
            format: id,
            metadata_schema: id,
            flags: 0,
            timestamp_us: 20,
            duration_us: 1234,
            sequence: 9,
            payload: BorrowedBytes::new(&bytes),
            metadata: BorrowedBytes::new(&bytes),
        };
        assert_eq!(unit.validate(), Ok(()));
        assert!(
            AudioUnit {
                flags: AUDIO_EXTENSIBLE_END,
                ..unit
            }
            .validate()
            .is_ok()
        );
        assert!(
            AudioUnit {
                payload: BorrowedBytes::new(&[]),
                duration_us: 0,
                ..unit
            }
            .validate()
            .is_ok()
        );
        assert!(
            AudioUnit {
                generation: 0,
                ..unit
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioUnit {
                metadata_schema: absent,
                ..unit
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioUnit {
                duration_us: 0,
                ..unit
            }
            .validate()
            .is_ok()
        );
        assert!(
            AudioUnit {
                payload: BorrowedBytes {
                    ptr: std::ptr::null(),
                    len: 1
                },
                ..unit
            }
            .validate()
            .is_err()
        );
        assert!(
            AudioUnit {
                metadata: BorrowedBytes {
                    ptr: bytes.as_ptr(),
                    len: AUDIO_EXTENSIBLE_MAX_METADATA + 1
                },
                ..unit
            }
            .validate()
            .is_err()
        );
        let end = AudioUnit {
            metadata_schema: absent,
            flags: AUDIO_EXTENSIBLE_END,
            duration_us: 0,
            payload: BorrowedBytes::new(&[]),
            metadata: BorrowedBytes::new(&[]),
            ..unit
        };
        assert_eq!(end.validate(), Ok(()));
        assert!(
            AudioUnit {
                flags: AUDIO_EXTENSIBLE_END | AUDIO_EXTENSIBLE_DISCONTINUITY | 0x80,
                ..end
            }
            .validate()
            .is_ok()
        );
    }
}
