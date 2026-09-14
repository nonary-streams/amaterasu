//! Borrowed logical control messages; wire encryption and sequence numbers stay
//! inside the transport. No ENet or Rust implementation type crosses this ABI.
use amaterasu_abi::{NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::ffi::c_void;

pub const CONTROL_RELIABLE: u32 = 1;
pub const CONTROL_UNRELIABLE: u32 = 2;
pub const CONTROL_UNSEQUENCED: u32 = 3;
pub const MAX_CONTROL_PAYLOAD: usize = 65_511;

#[repr(C)]
pub struct ControlMessage {
    /// GameStream message type, restricted to the 16-bit wire field.
    pub message_type: u32,
    /// ENet routing metadata, outside the control envelope's authentication.
    /// Consumers must authorize the authenticated message type/body, not channel.
    pub channel: u32,
    pub delivery: u32,
    pub reserved: u32,
    pub data: *const u8,
    pub len: usize,
    /// Received authenticated envelope sequence. Ignored on send: the transport
    /// assigns its own sequence and never permits callers to select GCM nonces.
    pub sequence: u64,
}

impl ControlMessage {
    pub fn validate(&self) -> Result<(), Status> {
        if self.message_type > u16::MAX.into()
            || self.channel >= 255
            || !matches!(
                self.delivery,
                CONTROL_RELIABLE | CONTROL_UNRELIABLE | CONTROL_UNSEQUENCED
            )
            || self.reserved != 0
            || self.len > MAX_CONTROL_PAYLOAD
            || (self.len != 0 && self.data.is_null())
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// Nonempty data points to `len` initialized immutable bytes that remain live
    /// for the returned borrow. Metadata validation cannot prove pointer validity.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: allocation validity and lifetime are the caller's obligation.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControlMessageSink {
    pub context: *mut c_void,
    pub write: Option<unsafe extern "C" fn(*mut c_void, *const ControlMessage) -> Status>,
}

/// A trusted session coordinator authorizes peers and provides fresh keys through
/// the control service. The provider binds every native operation to its caller.
#[repr(C)]
pub struct ControlTransportApi {
    pub base: NativeInterface,
    /// Copies one logical message into bounded outgoing storage. Success means
    /// locally accepted, not acknowledged by the peer. BUSY accepts nothing and
    /// consumes no encryption sequence. The caller retains its allocation.
    pub send: unsafe extern "C" fn(*mut c_void, u64, *const ControlMessage) -> Status,
    /// Calls the sink at most once with authenticated plaintext borrowed for the
    /// callback only. A rejected callback retains the same message for retry.
    pub receive: unsafe extern "C" fn(*mut c_void, u64, ControlMessageSink) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_rejects_unrepresentable_fields_without_dereferencing() {
        let mut message = ControlMessage {
            message_type: 1,
            channel: 0,
            delivery: CONTROL_RELIABLE,
            reserved: 0,
            data: std::ptr::null(),
            len: 0,
            sequence: 0,
        };
        assert!(message.validate().is_ok());
        message.len = 1;
        assert_eq!(message.validate(), Err(STATUS_INVALID_ARGUMENT));
        message.len = 0;
        message.message_type = 65_536;
        assert_eq!(message.validate(), Err(STATUS_INVALID_ARGUMENT));
        message.message_type = 1;
        message.delivery = 4;
        assert_eq!(message.validate(), Err(STATUS_INVALID_ARGUMENT));
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn layout_matches_public_c_tables() {
        assert_eq!(size_of::<ControlMessage>(), 40);
        assert_eq!(std::mem::offset_of!(ControlMessage, data), 16);
        assert_eq!(std::mem::offset_of!(ControlMessage, sequence), 32);
        assert_eq!(size_of::<ControlTransportApi>(), 48);
        assert_eq!(std::mem::offset_of!(ControlTransportApi, send), 32);
        assert_eq!(std::mem::offset_of!(ControlTransportApi, receive), 40);
    }
}
