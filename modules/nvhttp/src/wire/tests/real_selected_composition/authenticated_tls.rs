//! Loopback TLS client and NVHTTP listener for selected-module qualification.
//! The pinned host certificate and both CertificateVerify signatures are checked.
use crate::{server, settings::Settings, tls::Identity};
use rustls::{
    DigitallySignedStruct, Error, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use std::{
    io::{ErrorKind, Read, Write},
    net::TcpStream,
    sync::Arc,
};

const CLIENT_KEY: &str = include_str!("../../../../../pairing/tests/fixtures/client-key.pem");

#[derive(Debug)]
struct PinnedHost(Vec<u8>);
impl ServerCertVerifier for PinnedHost {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if end_entity.as_ref() != self.0 || !intermediates.is_empty() {
            return Err(Error::General("unexpected host certificate".into()));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        crate::tls::verify_test_server_signature(message, cert, signature, false)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        crate::tls::verify_test_server_signature(message, cert, signature, true)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

pub(super) struct Connection {
    pub server: server::Server,
    socket: TcpStream,
    tls: rustls::ClientConnection,
}
impl Connection {
    pub fn new(identity: &Identity, host_cert: &[u8], client_cert: &[u8], target: &str) -> Self {
        let settings = Settings {
            http_bind: "127.0.0.1:0".parse().unwrap(),
            https_bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        };
        let mut server = server::Server::bind(&settings).unwrap();
        server.replace_tls(identity.configuration(host_cert.to_vec()).unwrap());
        let (_, key) = pem_rfc7468::decode_vec(CLIENT_KEY.as_bytes()).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key));
        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key).unwrap();
        let client = rustls::sign::CertifiedKey::new(
            vec![CertificateDer::from(client_cert.to_vec())],
            signing_key,
        );
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedHost(host_cert.to_vec())))
        .with_client_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(client)));
        let mut tls = rustls::ClientConnection::new(
            Arc::new(config),
            ServerName::try_from("qualification.invalid").unwrap(),
        )
        .unwrap();
        write!(
            tls.writer(),
            "GET {target} HTTP/1.1\r\nHost: qualification.invalid\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let socket = TcpStream::connect(server.https_address()).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self {
            server,
            socket,
            tls,
        }
    }
    pub fn tick(&mut self, identity: &Identity) -> server::Events {
        if self.tls.wants_write() {
            match self.tls.write_tls(&mut self.socket) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("client TLS write: {error}"),
            }
        }
        let events = self.server.tick_checked(|cert, permission| {
            permission.map_or_else(
                || identity.trusted(cert),
                |permission| identity.allowed(cert, permission),
            )
        });
        if self.tls.wants_read() {
            match self.tls.read_tls(&mut self.socket) {
                Ok(0) => {}
                Ok(_) => {
                    self.tls
                        .process_new_packets()
                        .expect("authenticated TLS handshake");
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("client TLS read: {error}"),
            }
        }
        events
    }
    pub fn read_plaintext(&mut self, output: &mut Vec<u8>) {
        match self.tls.reader().read_to_end(output) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("TLS plaintext: {error}"),
        }
    }
}
