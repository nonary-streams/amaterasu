//! Dedicated browser TLS material. Moonlight pairing identity is never used here.
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{fs::File, io::Read, path::Path, sync::Arc};

const MAX_CHAIN_BYTES: u64 = 64 * 1024;
const MAX_KEY_BYTES: u64 = 16 * 1024;

pub fn configuration(
    certificate_path: &str,
    private_key_path: &str,
) -> Result<Arc<ServerConfig>, String> {
    let certificate_path = Path::new(certificate_path);
    let private_key_path = Path::new(private_key_path);
    if !certificate_path.is_absolute()
        || !private_key_path.is_absolute()
        || certificate_path == private_key_path
    {
        return Err("management TLS requires distinct absolute certificate and key paths".into());
    }
    let chain = bounded_read(certificate_path, MAX_CHAIN_BYTES)?;
    let key = bounded_read(private_key_path, MAX_KEY_BYTES)?;
    let certificates: Vec<_> = CertificateDer::pem_slice_iter(&chain)
        .collect::<Result<_, _>>()
        .map_err(|_| "management TLS certificate chain invalid")?;
    if certificates.is_empty() || certificates.len() > 8 {
        return Err("management TLS certificate chain invalid".into());
    }
    let private_key =
        PrivateKeyDer::from_pem_slice(&key).map_err(|_| "management TLS private key invalid")?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| "management TLS versions unavailable")?
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|_| "management TLS certificate and key mismatch")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.max_early_data_size = 0;
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    Ok(Arc::new(config))
}

fn bounded_read(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "management TLS file unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "management TLS file unavailable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err("management TLS file size invalid".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "management TLS file unreadable")?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err("management TLS file size invalid".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ClientConfig, ClientConnection, RootCertStore, ServerConnection, pki_types::ServerName,
    };
    use std::fs;

    #[test]
    fn dedicated_pem_pair_loads_and_mismatched_key_fails_closed() {
        let directory =
            std::env::temp_dir().join(format!("amaterasu-management-tls-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let certificate = directory.join("certificate.pem");
        let private_key = directory.join("private-key.pem");
        fs::write(
            &certificate,
            include_bytes!("../tests/fixtures/browser-cert.pem"),
        )
        .unwrap();
        fs::write(
            &private_key,
            include_bytes!("../tests/fixtures/browser-key.pem"),
        )
        .unwrap();
        let server_configuration =
            configuration(certificate.to_str().unwrap(), private_key.to_str().unwrap()).unwrap();
        let certificate_der =
            CertificateDer::from_pem_slice(include_bytes!("../tests/fixtures/browser-cert.pem"))
                .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(certificate_der).unwrap();
        let client_configuration =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        let mut client = ClientConnection::new(
            Arc::new(client_configuration),
            ServerName::try_from("127.0.0.1".to_owned()).unwrap(),
        )
        .unwrap();
        let mut server = ServerConnection::new(server_configuration).unwrap();
        for _ in 0..8 {
            let mut wire = Vec::new();
            client.write_tls(&mut wire).unwrap();
            if !wire.is_empty() {
                server.read_tls(&mut wire.as_slice()).unwrap();
                server.process_new_packets().unwrap();
            }
            wire.clear();
            server.write_tls(&mut wire).unwrap();
            if !wire.is_empty() {
                client.read_tls(&mut wire.as_slice()).unwrap();
                client.process_new_packets().unwrap();
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                break;
            }
        }
        assert!(!client.is_handshaking() && !server.is_handshaking());
        fs::write(
            &private_key,
            include_bytes!("../../pairing/tests/fixtures/client-key.pem"),
        )
        .unwrap();
        assert!(
            configuration(certificate.to_str().unwrap(), private_key.to_str().unwrap()).is_err()
        );
        assert!(configuration("relative-cert.pem", private_key.to_str().unwrap()).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
