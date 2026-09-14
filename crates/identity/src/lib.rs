//! Pairing-owned TLS identity. All methods are thread-safe while the binding is
//! live. Consumers must finish concurrent calls before releasing its context.
//! Reply bytes are borrowed only for the synchronous ReplySink callback; no key
//! material crosses this interface. No provider lock is held during sink callbacks.
use amaterasu_abi::{BorrowedBytes, NativeInterface, ReplySink, Status};
use std::ffi::c_void;

pub const SERVICE: &str = "amaterasu.identity/1";
pub const INTERFACE_MAJOR: u32 = 1;
pub const INTERFACE_MINOR: u32 = 0;
pub const MAX_MESSAGE_BYTES: usize = 65536;
pub const MAX_CERTIFICATE_BYTES: usize = 8192;
pub const MAX_SIGNATURE_BYTES: usize = 512;
pub const TLS_RSA_PKCS1_SHA256: u32 = 0x0401;
pub const TLS_RSA_PKCS1_SHA384: u32 = 0x0501;
pub const TLS_RSA_PKCS1_SHA512: u32 = 0x0601;
pub const TLS_RSA_PSS_RSAE_SHA256: u32 = 0x0804;
pub const TLS_RSA_PSS_RSAE_SHA384: u32 = 0x0805;
pub const TLS_RSA_PSS_RSAE_SHA512: u32 = 0x0806;
pub const IDENTITY_UNAVAILABLE: Status = 20;
pub const IDENTITY_FORBIDDEN: Status = 21;
pub const IDENTITY_UNSUPPORTED: Status = 22;
pub const IDENTITY_UNTRUSTED: Status = 23;

#[repr(C)]
pub struct IdentityApi {
    pub base: NativeInterface,
    /// Returns one DER host certificate, at most MAX_CERTIFICATE_BYTES.
    pub certificate: unsafe extern "C" fn(*mut c_void, ReplySink) -> Status,
    /// Signs the full TLS message (not a supplied digest). PKCS1v1.5 or RSA-PSS
    /// with SHA256/384/512, PSS salt length equal to the hash length. Output is at
    /// most MAX_SIGNATURE_BYTES. Unsupported schemes produce no callback.
    pub sign: unsafe extern "C" fn(*mut c_void, u32, BorrowedBytes, ReplySink) -> Status,
    /// Exact committed paired-client DER membership and certificate validity.
    /// Returns STATUS_OK only when currently trusted. TLS adapters must separately
    /// verify handshake proof of possession and apply their TLS protocol policy.
    pub verify_client: unsafe extern "C" fn(*mut c_void, BorrowedBytes) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn table_has_stable_c_prefix_and_callback_offsets() {
        assert_eq!(size_of::<IdentityApi>(), 56);
        assert_eq!(std::mem::offset_of!(IdentityApi, certificate), 32);
        assert_eq!(std::mem::offset_of!(IdentityApi, sign), 40);
        assert_eq!(std::mem::offset_of!(IdentityApi, verify_client), 48);
    }
}
