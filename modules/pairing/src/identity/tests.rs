use super::*;
use crate::{
    crypto,
    protocol::{Client, Document, Identity},
};
use rsa::{Pkcs1v15Sign, Pss, RsaPublicKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::fs;
use std::path::{Path, PathBuf};
use x509_cert::der::Encode;
const HOST_CERT: &str = include_str!("../../tests/fixtures/host-cert.pem");
const HOST_KEY: &str = include_str!("../../tests/fixtures/host-key.pem");
const CLIENT_CERT: &str = include_str!("../../tests/fixtures/client-cert.pem");
const CLIENT_KEY: &str = include_str!("../../tests/fixtures/client-key.pem");

fn crypto_certificate_der(pem: &str) -> Vec<u8> {
    crypto::parse_certificate(pem, None)
        .unwrap()
        .certificate
        .to_der()
        .unwrap()
}

fn document() -> Document {
    let peer = crypto::parse_certificate(CLIENT_CERT, None).unwrap();
    Document {
        identity: Some(Identity {
            host_id: "00000000-0000-4000-8000-000000000001".into(),
            certificate_pem: HOST_CERT.into(),
            private_key_pem: HOST_KEY.into(),
        }),
        permission_mode_enabled: false,
        clients: vec![Client {
            certificate_id: peer.fingerprint,
            client_id: "client".into(),
            name: "Client".into(),
            certificate_pem: CLIENT_CERT.into(),
            paired_at: 0,
            permissions: crate::protocol::ClientPermissions::default(),
            entitlements: Vec::new(),
            config_overrides: crate::protocol::ClientConfigOverrides::default(),
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
fn current_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn temporary_identity_directory() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("amaterasu-pairing-identity-{nonce}"));
    fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}
fn write_identity_files(
    directory: &Path,
    label: &str,
    certificate: &str,
    key: &str,
) -> (String, String) {
    let certificate_path = directory.join(format!("{label}-certificate.pem"));
    let key_path = directory.join(format!("{label}-key.pem"));
    fs::write(&certificate_path, certificate).unwrap();
    fs::write(&key_path, key).unwrap();
    (
        certificate_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
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
fn configured_identity_files_require_safe_paths_and_a_matching_valid_pair() {
    let directory = temporary_identity_directory();
    let (certificate_path, key_path) =
        write_identity_files(&directory, "valid", HOST_CERT, HOST_KEY);
    let configured = ConfiguredIdentity::load(&certificate_path, &key_path, 4, time()).unwrap();
    assert_eq!(configured.provider_revision, 4);

    let (_, mismatched_key_path) =
        write_identity_files(&directory, "mismatched", HOST_CERT, CLIENT_KEY);
    assert!(ConfiguredIdentity::load(&certificate_path, &mismatched_key_path, 5, time()).is_err());

    let malformed_certificate_path = directory.join("malformed-certificate.pem");
    fs::write(&malformed_certificate_path, b"not a certificate").unwrap();
    assert!(
        ConfiguredIdentity::load(
            malformed_certificate_path.to_str().unwrap(),
            &key_path,
            6,
            time()
        )
        .is_err()
    );
    assert!(
        ConfiguredIdentity::load(
            directory.join("missing-certificate.pem").to_str().unwrap(),
            &key_path,
            7,
            time()
        )
        .is_err()
    );
    assert!(ConfiguredIdentity::load("relative/certificate.pem", &key_path, 8, time()).is_err());
    let parent_escape = directory.join("..").join("valid-certificate.pem");
    assert!(
        ConfiguredIdentity::load(parent_escape.to_str().unwrap(), &key_path, 9, time()).is_err()
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_identity_provisions_both_missing_files_once_with_private_permissions() {
    let directory = temporary_identity_directory();
    let credentials = directory.join("credentials");
    let certificate_path = credentials.join("cacert.pem");
    let key_path = credentials.join("cakey.pem");
    let now = current_time();

    let configured = ConfiguredIdentity::load(
        certificate_path.to_str().unwrap(),
        key_path.to_str().unwrap(),
        10,
        now,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&certificate_path).unwrap(),
        configured.certificate_pem
    );
    assert_eq!(
        fs::read_to_string(&key_path).unwrap(),
        configured.private_key_pem
    );
    assert!(credentials.is_dir());

    let reloaded = ConfiguredIdentity::load(
        certificate_path.to_str().unwrap(),
        key_path.to_str().unwrap(),
        11,
        current_time().max(now),
    )
    .unwrap();
    assert_eq!(reloaded.certificate_pem, configured.certificate_pem);
    assert_eq!(reloaded.private_key_pem, configured.private_key_pem);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&credentials).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in [&certificate_path, &key_path] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_identity_does_not_repair_or_replace_a_partial_pair() {
    let directory = temporary_identity_directory();
    let certificate_path = directory.join("existing-certificate.pem");
    let key_path = directory.join("missing-key.pem");
    fs::write(&certificate_path, HOST_CERT).unwrap();

    assert!(
        ConfiguredIdentity::load(
            certificate_path.to_str().unwrap(),
            key_path.to_str().unwrap(),
            12,
            current_time(),
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&certificate_path).unwrap(), HOST_CERT);
    assert!(!key_path.exists());
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn configured_identity_open_rejects_a_symlinked_effective_file() {
    use std::os::unix::fs::symlink;

    let directory = temporary_identity_directory();
    let (certificate_path, key_path) =
        write_identity_files(&directory, "safe", HOST_CERT, HOST_KEY);
    let link_path = directory.join("certificate-link.pem");
    symlink(&certificate_path, &link_path).unwrap();
    assert!(ConfiguredIdentity::load(link_path.to_str().unwrap(), &key_path, 8, time()).is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_tls_identity_refreshes_on_provider_revision_and_restart_without_pairing_writes() {
    let directory = temporary_identity_directory();
    let (first_certificate_path, first_key_path) =
        write_identity_files(&directory, "first", HOST_CERT, HOST_KEY);
    let first_configuration =
        ConfiguredIdentity::load(&first_certificate_path, &first_key_path, 10, current_time())
            .unwrap();

    let persisted_document = document();
    let persisted_host_id = persisted_document
        .identity
        .as_ref()
        .unwrap()
        .host_id
        .clone();
    let persisted_client_count = persisted_document.clients.len();
    let first_snapshot = Snapshot::prepare_with_configured_identity(
        &persisted_document,
        current_time(),
        &first_configuration,
    )
    .unwrap();
    let paired_client = crypto_certificate_der(CLIENT_CERT);
    assert!(first_snapshot.verify_client(&paired_client, current_time()));

    let shared = Shared::default();
    shared
        .publish_with_configuration_revision(22, 10, Some(first_snapshot.clone()))
        .unwrap();
    let in_flight = shared.get(current_time()).unwrap();

    let replacement_identity = crypto::generate_identity().unwrap();
    let (second_certificate_path, second_key_path) = write_identity_files(
        &directory,
        "second",
        &replacement_identity.certificate_pem,
        &replacement_identity.private_key_pem,
    );
    let second_configuration = ConfiguredIdentity::load(
        &second_certificate_path,
        &second_key_path,
        11,
        current_time(),
    )
    .unwrap();
    let second_snapshot = Snapshot::prepare_with_configured_identity(
        &persisted_document,
        current_time(),
        &second_configuration,
    )
    .unwrap();
    assert_ne!(first_snapshot.certificate(), second_snapshot.certificate());
    assert!(second_snapshot.verify_client(&paired_client, current_time()));
    shared
        .publish_with_configuration_revision(22, 11, Some(second_snapshot.clone()))
        .unwrap();
    assert!(!shared.current(&in_flight, current_time()));
    assert_eq!(
        shared.get(current_time()).unwrap().certificate(),
        second_snapshot.certificate()
    );

    // A fresh module instance reopens the configured paths and loads the new
    // identity while the persisted pairing host ID and client records stay put.
    let restarted_configuration = ConfiguredIdentity::load(
        &second_certificate_path,
        &second_key_path,
        11,
        current_time(),
    )
    .unwrap();
    let restarted_snapshot = Snapshot::prepare_with_configured_identity(
        &persisted_document,
        current_time(),
        &restarted_configuration,
    )
    .unwrap();
    assert_eq!(
        restarted_snapshot.certificate(),
        second_snapshot.certificate()
    );
    assert_eq!(
        persisted_document.identity.as_ref().unwrap().host_id,
        persisted_host_id
    );
    assert_eq!(persisted_document.clients.len(), persisted_client_count);
    fs::remove_dir_all(directory).unwrap();
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

#[test]
fn permission_api_reads_current_committed_certificate_policy() {
    let shared = Arc::new(Shared::default());
    let factory = Factory::new(shared.clone(), ["transport".into()].into());
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: INTERFACE_MAJOR,
        min_minor: INTERFACE_MINOR_PERMISSIONS,
        min_size: size_of::<IdentityApiV1_1>() as u32,
    };
    let mut output = std::ptr::null();
    assert_eq!(
        unsafe {
            bind(
                factory.api.context,
                BorrowedBytes::new(b"transport"),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    let api = unsafe { &*output.cast::<IdentityApiV1_1>() };
    // These are the runtime's table compatibility checks, not merely the
    // factory's own query validation. Version 1.0 remains a valid prefix.
    let header = api.v1_0.base.header;
    assert_eq!(header.major, query.interface_major);
    assert!(header.minor >= query.min_minor);
    assert!(header.size >= query.min_size);
    assert!(header.size as usize >= size_of::<IdentityApi>());
    let context = api.v1_0.base.context;
    let client = crypto_certificate_der(CLIENT_CERT);
    let check = |permission| unsafe {
        (api.check_permission)(context, BorrowedBytes::new(&client), permission)
    };
    assert_eq!(check(PERMISSION_LAUNCH), IDENTITY_UNAVAILABLE);
    let mut document = document();
    document.permission_mode_enabled = true;
    document.clients[0].permissions = crate::protocol::ClientPermissions::subsequent_client();
    shared
        .publish(1, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(check(PERMISSION_LIST), STATUS_OK);
    assert_eq!(check(PERMISSION_VIEW), STATUS_OK);
    assert_eq!(check(PERMISSION_LAUNCH), IDENTITY_FORBIDDEN);
    assert_eq!(check(99), STATUS_INVALID_ARGUMENT);
    assert_eq!(
        unsafe {
            (api.check_permission)(
                context,
                BorrowedBytes::new(&crypto_certificate_der(HOST_CERT)),
                PERMISSION_VIEW,
            )
        },
        IDENTITY_UNTRUSTED
    );
    document.clients[0].permissions.launch_apps = true;
    shared
        .publish(2, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(check(PERMISSION_LAUNCH), STATUS_OK);
    document.clients[0].permissions = crate::protocol::ClientPermissions {
        list_apps: false,
        view_streams: false,
        launch_apps: false,
        terminate_streams: false,
    };
    document.permission_mode_enabled = false;
    shared
        .publish(3, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(check(PERMISSION_LAUNCH), STATUS_OK);
    shared.invalidate();
    assert_eq!(check(PERMISSION_LAUNCH), IDENTITY_UNAVAILABLE);
    unsafe { (api.v1_0.base.release.unwrap())(context) };
}

#[test]
fn entitlement_checks_committed_client_policy_and_revocation() {
    let shared = Arc::new(Shared::default());
    let factory = Factory::new(shared.clone(), ["transport".into()].into());
    let query = NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: INTERFACE_MAJOR,
        min_minor: INTERFACE_MINOR_ENTITLEMENTS,
        min_size: size_of::<IdentityApiV1_2>() as u32,
    };
    let mut output = std::ptr::null();
    assert_eq!(
        unsafe {
            bind(
                factory.api.context,
                BorrowedBytes::new(b"transport"),
                &query,
                &mut output,
            )
        },
        STATUS_OK
    );
    let api = unsafe { &*output.cast::<IdentityApiV1_2>() };
    let context = api.v1_1.v1_0.base.context;
    let client = crypto_certificate_der(CLIENT_CERT);
    let check = |certificate: &[u8], name: &str| unsafe {
        (api.check_entitlement)(
            context,
            BorrowedBytes::new(certificate),
            BorrowedBytes::new(name.as_bytes()),
        )
    };
    assert_eq!(check(&client, "org.alpha/read"), IDENTITY_UNAVAILABLE);
    let mut document = document();
    shared
        .publish(1, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    // The transport's module binding grant is not a client entitlement, even
    // with legacy permission mode disabled.
    assert_eq!(check(&client, "org.alpha/read"), IDENTITY_FORBIDDEN);
    document.clients[0].entitlements = vec!["org.alpha/read".into(), "net.beta/write".into()];
    shared
        .publish(2, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(check(&client, "org.alpha/read"), STATUS_OK);
    assert_eq!(check(&client, "net.beta/write"), STATUS_OK);
    assert_eq!(check(&client, "org.alpha/write"), IDENTITY_FORBIDDEN);
    assert_eq!(check(&client, "INVALID"), STATUS_INVALID_ARGUMENT);
    assert_eq!(
        check(&crypto_certificate_der(HOST_CERT), "org.alpha/read"),
        IDENTITY_UNTRUSTED
    );
    document.clients[0].entitlements.remove(0);
    shared
        .publish(3, Snapshot::prepare(&document, time()).unwrap())
        .unwrap();
    assert_eq!(check(&client, "org.alpha/read"), IDENTITY_FORBIDDEN);
    assert_eq!(check(&client, "net.beta/write"), STATUS_OK);
    shared.invalidate();
    assert_eq!(check(&client, "net.beta/write"), IDENTITY_UNAVAILABLE);
    unsafe { (api.v1_1.v1_0.base.release.unwrap())(context) };
}

// A minimal independent identity provider used only to exercise the same native
// consumer against an implementation that does not use pairing's snapshot.
struct AlternativeAuthority {
    api: IdentityApiV1_2,
    client: Vec<u8>,
    policy: std::sync::Mutex<Option<BTreeSet<String>>>,
}
unsafe extern "C" fn alternative_unused_certificate(_: *mut c_void, _: ReplySink) -> Status {
    IDENTITY_UNSUPPORTED
}
unsafe extern "C" fn alternative_unused_sign(
    _: *mut c_void,
    _: u32,
    _: BorrowedBytes,
    _: ReplySink,
) -> Status {
    IDENTITY_UNSUPPORTED
}
unsafe extern "C" fn alternative_unused_verify(_: *mut c_void, _: BorrowedBytes) -> Status {
    IDENTITY_UNSUPPORTED
}
unsafe extern "C" fn alternative_unused_permission(
    _: *mut c_void,
    _: BorrowedBytes,
    _: u32,
) -> Status {
    IDENTITY_UNSUPPORTED
}
unsafe extern "C" fn alternative_release(_: *mut c_void) {}
unsafe extern "C" fn alternative_check(
    context: *mut c_void,
    certificate: BorrowedBytes,
    name: BorrowedBytes,
) -> Status {
    // SAFETY: fixture retains the authority and both buffers for the call.
    let authority = unsafe { &*context.cast::<AlternativeAuthority>() };
    let (Ok(certificate), Ok(name)) = (unsafe { certificate.read() }, unsafe { name.read() })
    else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(name) = std::str::from_utf8(name) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if !valid_entitlement(name) {
        return STATUS_INVALID_ARGUMENT;
    }
    let policy = authority.policy.lock().unwrap();
    let Some(policy) = policy.as_ref() else {
        return IDENTITY_UNAVAILABLE;
    };
    if certificate != authority.client {
        return IDENTITY_UNTRUSTED;
    }
    if policy.contains(name) {
        STATUS_OK
    } else {
        IDENTITY_FORBIDDEN
    }
}
unsafe extern "C" fn alternative_bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: the fixture retains the authority and the consumer provides live arguments.
    let authority = unsafe { &*context.cast::<AlternativeAuthority>() };
    let caller = unsafe { caller.read() }.unwrap();
    let query = unsafe { &*query };
    unsafe { *output = std::ptr::null() };
    if caller != b"transport" {
        return IDENTITY_FORBIDDEN;
    }
    if query.interface_major != INTERFACE_MAJOR
        || query.min_minor > INTERFACE_MINOR_ENTITLEMENTS
        || query.min_size as usize > size_of::<IdentityApiV1_2>()
    {
        return IDENTITY_UNSUPPORTED;
    }
    unsafe { *output = &authority.api.v1_1.v1_0.base };
    STATUS_OK
}
impl AlternativeAuthority {
    fn new(client: Vec<u8>) -> Box<Self> {
        let mut authority = Box::new(Self {
            api: IdentityApiV1_2 {
                v1_1: IdentityApiV1_1 {
                    v1_0: IdentityApi {
                        base: NativeInterface {
                            header: AbiHeader {
                                minor: INTERFACE_MINOR_ENTITLEMENTS,
                                ..AbiHeader::of::<IdentityApiV1_2>()
                            },
                            context: std::ptr::null_mut(),
                            release: Some(alternative_release),
                        },
                        certificate: alternative_unused_certificate,
                        sign: alternative_unused_sign,
                        verify_client: alternative_unused_verify,
                    },
                    check_permission: alternative_unused_permission,
                },
                check_entitlement: alternative_check,
            },
            client,
            policy: std::sync::Mutex::new(None),
        });
        authority.api.v1_1.v1_0.base.context = (&mut *authority as *mut Self).cast();
        authority
    }
    fn factory(&mut self) -> NativeServiceFactory {
        NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: (self as *mut Self).cast(),
            bind: Some(alternative_bind),
        }
    }
}

// This consumer uses only the published table and never knows which provider it
// is bound to. Each call is a new point-in-time check, not a retained lease.
struct EntitlementConsumer(*const IdentityApiV1_2);
impl EntitlementConsumer {
    fn bind(factory: &NativeServiceFactory, caller: &str) -> Result<Self, Status> {
        let query = NativeInterfaceQuery {
            header: AbiHeader::of::<NativeInterfaceQuery>(),
            interface_major: INTERFACE_MAJOR,
            min_minor: INTERFACE_MINOR_ENTITLEMENTS,
            min_size: size_of::<IdentityApiV1_2>() as u32,
        };
        let mut output = std::ptr::null();
        // SAFETY: factory and query live through the synchronous bind.
        let result = unsafe {
            factory.bind.unwrap()(
                factory.context,
                BorrowedBytes::new(caller.as_bytes()),
                &query,
                &mut output,
            )
        };
        if result != STATUS_OK {
            return Err(result);
        }
        assert!(!output.is_null());
        // SAFETY: provider returned a live interface header for this binding.
        let header = unsafe { (*output).header };
        assert_eq!(header.major, INTERFACE_MAJOR);
        assert!(
            header.minor >= INTERFACE_MINOR_ENTITLEMENTS
                && header.size as usize >= size_of::<IdentityApiV1_2>()
        );
        Ok(Self(output.cast()))
    }
    fn check(&self, client: &[u8], name: &str) -> Status {
        // SAFETY: the bound provider and input bytes remain live for the call.
        let api = unsafe { &*self.0 };
        unsafe {
            (api.check_entitlement)(
                api.v1_1.v1_0.base.context,
                BorrowedBytes::new(client),
                BorrowedBytes::new(name.as_bytes()),
            )
        }
    }
}
impl Drop for EntitlementConsumer {
    fn drop(&mut self) {
        // SAFETY: one release after all calls, while the provider is live.
        let base = unsafe { &(*self.0).v1_1.v1_0.base };
        unsafe { base.release.unwrap()(base.context) };
    }
}

#[test]
fn unchanged_entitlement_consumer_works_with_replacement_authority() {
    let client = crypto_certificate_der(CLIENT_CERT);
    let shared = Arc::new(Shared::default());
    let selected = Factory::new(shared.clone(), ["transport".into()].into());
    let mut alternative = AlternativeAuthority::new(client.clone());
    let replacement = alternative.factory();
    for factory in [&selected.api, &replacement] {
        assert!(matches!(
            EntitlementConsumer::bind(factory, "other-module"),
            Err(IDENTITY_FORBIDDEN)
        ));
    }
    let consumers = [
        EntitlementConsumer::bind(&selected.api, "transport").unwrap(),
        EntitlementConsumer::bind(&replacement, "transport").unwrap(),
    ];
    for consumer in &consumers {
        assert_eq!(
            consumer.check(&client, "org.alpha/read"),
            IDENTITY_UNAVAILABLE
        );
    }
    let mut doc = document();
    shared
        .publish(1, Snapshot::prepare(&doc, time()).unwrap())
        .unwrap();
    *alternative.policy.lock().unwrap() = Some(BTreeSet::new());
    for consumer in &consumers {
        // Binding permission for the module does not grant its remote client.
        assert_eq!(
            consumer.check(&client, "org.alpha/read"),
            IDENTITY_FORBIDDEN
        );
        assert_eq!(
            consumer.check(&client, "net.unknown/act"),
            IDENTITY_FORBIDDEN
        );
    }
    doc.clients[0].entitlements = vec!["org.alpha/read".into(), "net.beta/write".into()];
    shared
        .publish(2, Snapshot::prepare(&doc, time()).unwrap())
        .unwrap();
    *alternative.policy.lock().unwrap() =
        Some(doc.clients[0].entitlements.iter().cloned().collect());
    for consumer in &consumers {
        for _ in 0..2 {
            assert_eq!(consumer.check(&client, "org.alpha/read"), STATUS_OK);
            assert_eq!(consumer.check(&client, "net.beta/write"), STATUS_OK);
            assert_eq!(
                consumer.check(&client, "net.unknown/act"),
                IDENTITY_FORBIDDEN
            );
        }
    }
    doc.clients[0].entitlements.remove(0);
    shared
        .publish(3, Snapshot::prepare(&doc, time()).unwrap())
        .unwrap();
    *alternative.policy.lock().unwrap() =
        Some(doc.clients[0].entitlements.iter().cloned().collect());
    for consumer in &consumers {
        assert_eq!(
            consumer.check(&client, "org.alpha/read"),
            IDENTITY_FORBIDDEN
        );
        assert_eq!(consumer.check(&client, "net.beta/write"), STATUS_OK);
    }
    shared.invalidate();
    *alternative.policy.lock().unwrap() = None;
    for consumer in &consumers {
        assert_eq!(
            consumer.check(&client, "net.beta/write"),
            IDENTITY_UNAVAILABLE
        );
    }
}

#[test]
fn entitlement_check_revalidates_client_time_and_snapshot() {
    let client = crypto_certificate_der(CLIENT_CERT);
    let mut doc = document();
    doc.clients[0].entitlements = vec!["org.alpha/read".into()];
    let initial = time();
    let mut snapshot = Snapshot::prepare(&doc, initial).unwrap().unwrap();
    Arc::get_mut(&mut snapshot)
        .unwrap()
        .set_client_expiry_for_test(initial + 1);
    let shared = Shared::default();
    shared.publish(1, Some(snapshot)).unwrap();
    assert_eq!(
        check_entitlement_current(&shared, &client, "org.alpha/read", initial, || Ok(initial)),
        Ok(())
    );
    assert_eq!(
        check_entitlement_current(&shared, &client, "org.alpha/read", initial, || Ok(
            initial + 1
        )),
        Err(IDENTITY_UNTRUSTED)
    );
    assert_eq!(
        check_entitlement_current(&shared, &client, "org.alpha/read", initial, || {
            shared.invalidate();
            Ok(initial)
        }),
        Err(IDENTITY_UNAVAILABLE)
    );
}

unsafe extern "C" fn old_provider_bind(
    _: *mut c_void,
    _: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: consumer supplies a live output pointer.
    unsafe { *output = std::ptr::null() };
    IDENTITY_UNSUPPORTED
}

#[test]
fn entitlement_consumer_denies_provider_without_version_1_2() {
    let old = NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: std::ptr::null_mut(),
        bind: Some(old_provider_bind),
    };
    assert!(matches!(
        EntitlementConsumer::bind(&old, "transport"),
        Err(IDENTITY_UNSUPPORTED)
    ));
}
