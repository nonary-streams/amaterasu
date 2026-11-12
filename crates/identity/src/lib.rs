//! Pairing-owned TLS identity. All methods are thread-safe while the binding is
//! live. Consumers must finish concurrent calls before releasing its context.
//! Reply bytes are borrowed only for the synchronous ReplySink callback; no key
//! material crosses this interface. No provider lock is held during sink callbacks.
use amaterasu_abi::{BorrowedBytes, NativeInterface, ReplySink, Status};
use std::ffi::c_void;

pub const SERVICE: &str = "amaterasu.identity/1";
pub const INTERFACE_MAJOR: u32 = 1;
pub const INTERFACE_MINOR: u32 = 0;
/// Minor version required for the appended permission query.
pub const INTERFACE_MINOR_PERMISSIONS: u32 = 1;
pub const INTERFACE_MINOR_ENTITLEMENTS: u32 = 2;
/// Maximum ASCII bytes in a namespaced entitlement.
pub const MAX_ENTITLEMENT_BYTES: usize = 128;

/// A lower-case reverse-DNS namespace followed by `/` and an operation name.
pub fn valid_entitlement(name: &str) -> bool {
    if name.len() > MAX_ENTITLEMENT_BYTES {
        return false;
    }
    let Some((namespace, operation)) = name.split_once('/') else {
        return false;
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    namespace.split('.').count() >= 2
        && namespace.split('.').all(valid_part)
        && valid_part(operation)
}

pub const PERMISSION_LIST: u32 = 1;
pub const PERMISSION_LAUNCH: u32 = 2;
pub const PERMISSION_VIEW: u32 = 3;
/// Requires the client's explicit stream-termination permission. Providers that
/// predate this selector reject it; consumers must treat that result as denial.
pub const PERMISSION_TERMINATE: u32 = 4;
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

/// Version 1.1 extends the unchanged 1.0 prefix. Request minor version 1 and
/// this table's full size before accessing the appended callback.
#[repr(C)]
pub struct IdentityApiV1_1 {
    pub v1_0: IdentityApi,
    /// Checks exact DER membership, certificate validity and current committed
    /// policy synchronously. List is allowed by list, view or launch; view by
    /// view or launch; termination requires terminate permission. Returns
    /// STATUS_OK only when allowed, IDENTITY_UNTRUSTED
    /// for an invalid or unpaired certificate, IDENTITY_FORBIDDEN for a denied
    /// permission, STATUS_INVALID_ARGUMENT for an unknown selector, and
    /// IDENTITY_UNAVAILABLE when committed policy cannot be consulted. Callers
    /// must deny on every non-OK status and separately verify TLS possession.
    pub check_permission: unsafe extern "C" fn(*mut c_void, BorrowedBytes, u32) -> Status,
}

/// Version 1.2 extends 1.1. Names are exact, administrator-approved operation
/// identifiers, not module grants. Only STATUS_OK authorizes the checked client.
/// TLS proof of possession and operation-to-name binding are the caller's duty.
/// Recheck before effects and delivery; this call does not reserve authorization.
#[repr(C)]
pub struct IdentityApiV1_2 {
    pub v1_1: IdentityApiV1_1,
    pub check_entitlement:
        unsafe extern "C" fn(*mut c_void, BorrowedBytes, BorrowedBytes) -> Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entitlement_names_are_bounded_and_unambiguous() {
        assert!(valid_entitlement("org.example/read"));
        for name in [
            "org/read",
            "Org.example/read",
            "org..example/read",
            "org.example/",
            "org.example/read/write",
            "org.example/read\0",
        ] {
            assert!(!valid_entitlement(name));
        }
        assert!(!valid_entitlement(&format!(
            "org.example/{}",
            "x".repeat(128)
        )));
    }
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn table_has_stable_c_prefix_and_callback_offsets() {
        assert_eq!(size_of::<IdentityApi>(), 56);
        assert_eq!(std::mem::offset_of!(IdentityApi, certificate), 32);
        assert_eq!(std::mem::offset_of!(IdentityApi, sign), 40);
        assert_eq!(std::mem::offset_of!(IdentityApi, verify_client), 48);
        assert_eq!(size_of::<IdentityApiV1_1>(), 64);
        assert_eq!(std::mem::offset_of!(IdentityApiV1_1, v1_0), 0);
        assert_eq!(std::mem::offset_of!(IdentityApiV1_1, check_permission), 56);
        assert_eq!(size_of::<IdentityApiV1_2>(), 72);
        assert_eq!(std::mem::offset_of!(IdentityApiV1_2, check_entitlement), 64);
    }
}
