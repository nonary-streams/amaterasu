use super::*;
use crate::{
    crypto,
    protocol::{Client, Document, Identity},
};
use rsa::{Pkcs1v15Sign, Pss, RsaPublicKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use x509_cert::der::Encode;
const HOST_CERT: &str = include_str!("../../tests/fixtures/host-cert.pem");
const HOST_KEY: &str = include_str!("../../tests/fixtures/host-key.pem");
const CLIENT_CERT: &str = include_str!("../../tests/fixtures/client-cert.pem");
fn document() -> Document {
    let peer = crypto::parse_certificate(CLIENT_CERT, None).unwrap();
    Document {
        identity: Some(Identity {
            host_id: "00000000-0000-4000-8000-000000000001".into(),
            certificate_pem: HOST_CERT.into(),
            private_key_pem: HOST_KEY.into(),
        }),
        clients: vec![Client {
            certificate_id: peer.fingerprint,
            client_id: "client".into(),
            name: "Client".into(),
            certificate_pem: CLIENT_CERT.into(),
            paired_at: 0,
        }],
    }
}
fn time() -> u64 {
    [HOST_CERT, CLIENT_CERT]
        .iter()
        .map(|pem| {
            crypto::parse_certificate(pem, None)
                .unwrap()
                .certificate
                .tbs_certificate
                .validity
                .not_before
                .to_unix_duration()
                .as_secs()
        })
        .max()
        .unwrap()
        + 1
}
unsafe extern "C" fn copy(context: *mut c_void, bytes: BorrowedBytes) {
    // SAFETY: test owns the vector and reply bytes are borrowed during the callback.
    unsafe { &mut *context.cast::<Vec<u8>>() }.extend_from_slice(unsafe { bytes.read() }.unwrap());
}
fn sink(output: &mut Vec<u8>) -> ReplySink {
    ReplySink {
        context: (output as *mut Vec<u8>).cast(),
        write: copy,
    }
}
fn bind_as(factory: &Factory, caller: &str) -> Result<*const IdentityApi, Status> {
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: 1,
        min_minor: 0,
        min_size: size_of::<IdentityApi>() as u32,
    };
    let mut output = std::ptr::null();
    let status = unsafe {
        bind(
            factory.api.context,
            BorrowedBytes::new(caller.as_bytes()),
            &query,
            &mut output,
        )
    };
    if status == STATUS_OK {
        Ok(output.cast())
    } else {
        assert!(output.is_null());
        Err(status)
    }
}
fn verify(key: &RsaPublicKey, scheme: u32, message: &[u8], signature: &[u8]) -> bool {
    match scheme {
        TLS_RSA_PKCS1_SHA256 => key.verify(
            Pkcs1v15Sign::new::<Sha256>(),
            &Sha256::digest(message),
            signature,
        ),
        TLS_RSA_PKCS1_SHA384 => key.verify(
            Pkcs1v15Sign::new::<Sha384>(),
            &Sha384::digest(message),
            signature,
        ),
        TLS_RSA_PKCS1_SHA512 => key.verify(
            Pkcs1v15Sign::new::<Sha512>(),
            &Sha512::digest(message),
            signature,
        ),
        TLS_RSA_PSS_RSAE_SHA256 => {
            key.verify(Pss::new::<Sha256>(), &Sha256::digest(message), signature)
        }
        TLS_RSA_PSS_RSAE_SHA384 => {
            key.verify(Pss::new::<Sha384>(), &Sha384::digest(message), signature)
        }
        TLS_RSA_PSS_RSAE_SHA512 => {
            key.verify(Pss::new::<Sha512>(), &Sha512::digest(message), signature)
        }
        _ => return false,
    }
    .is_ok()
}
#[test]
fn all_tls_schemes_sign_full_message_and_verify_with_paired_host_certificate() {
    let document = document();
    let snapshot = Snapshot::prepare(&document, time()).unwrap().unwrap();
    let key = crypto::parse_certificate(HOST_CERT, None)
        .unwrap()
        .public_key;
    let message = b"TLS 1.3, server CertificateVerify transcript";
    for scheme in [
        TLS_RSA_PKCS1_SHA256,
        TLS_RSA_PKCS1_SHA384,
        TLS_RSA_PKCS1_SHA512,
        TLS_RSA_PSS_RSAE_SHA256,
        TLS_RSA_PSS_RSAE_SHA384,
        TLS_RSA_PSS_RSAE_SHA512,
    ] {
        let signature = snapshot.sign(scheme, message).unwrap();
        assert!(signature.len() <= MAX_SIGNATURE_BYTES);
        assert!(verify(&key, scheme, message, &signature));
        assert!(!verify(&key, scheme, b"tampered", &signature));
    }
    let first = snapshot.sign(TLS_RSA_PSS_RSAE_SHA256, message).unwrap();
    let second = snapshot.sign(TLS_RSA_PSS_RSAE_SHA256, message).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        snapshot.sign(0x0201, message).unwrap_err(),
        IDENTITY_UNSUPPORTED
    );
}
#[test]
fn bound_api_keeps_private_material_internal_and_observes_revocation() {
    let shared = Arc::new(Shared::default());
    let factory = Factory::new(shared.clone(), ["transport".into()].into());
    assert_eq!(
        bind_as(&factory, "administrator").unwrap_err(),
        IDENTITY_FORBIDDEN
    );
    let api = unsafe { &*bind_as(&factory, "transport").unwrap() };
    let mut reply = vec![];
    assert_eq!(
        unsafe { (api.certificate)(api.base.context, sink(&mut reply)) },
        IDENTITY_UNAVAILABLE
    );
    assert!(reply.is_empty());
    let mut document = document();
    shared
        .publish(1, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(
        unsafe { (api.certificate)(api.base.context, sink(&mut reply)) },
        STATUS_OK
    );
    assert_eq!(
        reply,
        crypto::parse_certificate(HOST_CERT, None)
            .unwrap()
            .certificate
            .to_der()
            .unwrap()
    );
    let client = crypto::parse_certificate(CLIENT_CERT, None)
        .unwrap()
        .certificate
        .to_der()
        .unwrap();
    assert_eq!(
        unsafe { (api.verify_client)(api.base.context, BorrowedBytes::new(&client)) },
        STATUS_OK
    );
    assert_eq!(
        unsafe { (api.verify_client)(api.base.context, BorrowedBytes::new(&reply)) },
        IDENTITY_UNTRUSTED
    );
    let mut signature = vec![];
    assert_eq!(
        unsafe {
            (api.sign)(
                api.base.context,
                TLS_RSA_PSS_RSAE_SHA256,
                BorrowedBytes::new(b"handshake"),
                sink(&mut signature),
            )
        },
        STATUS_OK
    );
    assert!(verify(
        &crypto::parse_certificate(HOST_CERT, None)
            .unwrap()
            .public_key,
        TLS_RSA_PSS_RSAE_SHA256,
        b"handshake",
        &signature
    ));
    shared.invalidate();
    assert_eq!(
        unsafe { (api.verify_client)(api.base.context, BorrowedBytes::new(&client)) },
        IDENTITY_UNAVAILABLE
    );
    document.clients.clear();
    shared
        .publish(2, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(
        unsafe { (api.verify_client)(api.base.context, BorrowedBytes::new(&client)) },
        IDENTITY_UNTRUSTED
    );
    unsafe { (api.base.release.unwrap())(api.base.context) };
}
#[test]
fn same_revision_refresh_preserves_inflight_snapshot_but_changes_and_expiry_do_not() {
    let shared = Shared::default();
    let document = document();
    shared
        .publish(1, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    let active = shared.get(time()).unwrap();
    shared
        .publish(1, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert!(shared.current(&active, time()));
    shared
        .publish(2, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert!(!shared.current(&active, time()));
    let active = shared.get(time()).unwrap();
    shared.invalidate();
    assert!(!shared.current(&active, time()));
    shared.publish(3, Some(active)).unwrap();
    assert!(shared.get(u64::MAX).is_err());
}
#[test]
fn invalid_lengths_and_unsupported_scheme_never_call_reply_sink() {
    let shared = Arc::new(Shared::default());
    shared
        .publish(1, Snapshot::prepare(&document(), time()).unwrap())
        .unwrap();
    let factory = Factory::new(shared, ["transport".into()].into());
    let api = unsafe { &*bind_as(&factory, "transport").unwrap() };
    let mut reply = vec![];
    let invalid = BorrowedBytes {
        ptr: std::ptr::null(),
        len: MAX_MESSAGE_BYTES + 1,
    };
    assert_eq!(
        unsafe {
            (api.sign)(
                api.base.context,
                TLS_RSA_PKCS1_SHA256,
                invalid,
                sink(&mut reply),
            )
        },
        STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe {
            (api.sign)(
                api.base.context,
                0,
                BorrowedBytes::new(b"message"),
                sink(&mut reply),
            )
        },
        IDENTITY_UNSUPPORTED
    );
    assert_eq!(
        unsafe {
            (api.verify_client)(
                api.base.context,
                BorrowedBytes {
                    ptr: std::ptr::null(),
                    len: MAX_CERTIFICATE_BYTES + 1,
                },
            )
        },
        STATUS_INVALID_ARGUMENT
    );
    assert!(reply.is_empty());
    unsafe { (api.base.release.unwrap())(api.base.context) };
}
#[test]
fn reply_callbacks_may_invalidate_snapshot_without_provider_lock_deadlock() {
    unsafe extern "C" fn invalidate(context: *mut c_void, _: BorrowedBytes) {
        // SAFETY: test shared state outlives synchronous callback.
        unsafe { &*context.cast::<Shared>() }.invalidate();
    }
    let shared = Arc::new(Shared::default());
    shared
        .publish(1, Snapshot::prepare(&document(), time()).unwrap())
        .unwrap();
    let factory = Factory::new(shared.clone(), ["transport".into()].into());
    let api = unsafe { &*bind_as(&factory, "transport").unwrap() };
    let sink = ReplySink {
        context: Arc::as_ptr(&shared).cast_mut().cast(),
        write: invalidate,
    };
    assert_eq!(
        unsafe { (api.certificate)(api.base.context, sink) },
        STATUS_OK
    );
    assert!(shared.get(time()).is_err());
    unsafe { (api.base.release.unwrap())(api.base.context) };
}

#[test]
fn concurrent_readers_pin_memory_but_cannot_commit_results_after_invalidation() {
    let shared = Arc::new(Shared::default());
    shared
        .publish(1, Snapshot::prepare(&document(), time()).unwrap())
        .unwrap();
    let acquired = Arc::new(std::sync::Barrier::new(3));
    let resume = Arc::new(std::sync::Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let shared = shared.clone();
            let acquired = acquired.clone();
            let resume = resume.clone();
            std::thread::spawn(move || {
                let snapshot = shared.get(time()).unwrap();
                acquired.wait();
                resume.wait();
                let signature = snapshot
                    .sign(TLS_RSA_PSS_RSAE_SHA256, b"threaded handshake")
                    .unwrap();
                assert!(!signature.is_empty());
                assert!(!shared.current(&snapshot, time()));
            })
        })
        .collect::<Vec<_>>();
    acquired.wait();
    shared.invalidate();
    resume.wait();
    for worker in workers {
        worker.join().unwrap();
    }
}
