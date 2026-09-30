//! Pairing-owned continuity for identities imported from predecessor state.
use crate::{crypto, protocol::Document};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const IMPORTED_IDENTITY_MIGRATION_KEY: &str = "pairing/imported-identity/v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImportedIdentityMarker {
    version: u32,
    pairing_document_sha256: String,
    certificate_sha256: String,
    private_key_sha256: String,
}

pub(crate) enum ImportedIdentityReconciliation {
    NotPending,
    Reconciled,
    Superseded,
}

/// Creates a digest-only marker. It lets runtime distinguish migration intent
/// from ordinary pairing state, whose stored host identity can differ from the
/// configured TLS identity after initialization.
pub(crate) fn imported_identity_migration_marker(
    document: &Document,
) -> Result<Value, &'static str> {
    let identity = document.identity.as_ref().ok_or("invalid_identity")?;
    let document = serde_json::to_value(document).map_err(|_| "invalid_state")?;
    let marker = ImportedIdentityMarker {
        version: 1,
        pairing_document_sha256: sha256_json(&document)?,
        certificate_sha256: sha256(identity.certificate_pem.as_bytes()),
        private_key_sha256: sha256(identity.private_key_pem.as_bytes()),
    };
    serde_json::to_value(marker).map_err(|_| "invalid_state")
}

/// Applies an imported identity only while the exact imported pairing value
/// remains authoritative. A marker mismatch means the destination pairing
/// document won the generic state merge, so imported files must not replace
/// its configured identity.
pub(crate) fn reconcile_imported_identity(
    values: &Value,
    certificate_path: &str,
    private_key_path: &str,
    now: u64,
) -> Result<ImportedIdentityReconciliation, &'static str> {
    let values = values.as_object().ok_or("invalid_state")?;
    let Some(marker_value) = values.get(IMPORTED_IDENTITY_MIGRATION_KEY) else {
        return Ok(ImportedIdentityReconciliation::NotPending);
    };
    let marker: ImportedIdentityMarker =
        serde_json::from_value(marker_value.clone()).map_err(|_| "invalid_state")?;
    validate_marker(&marker)?;

    let Some(pairing_value) = values.get("pairing/v1") else {
        return Err("invalid_state");
    };
    if sha256_json(pairing_value)? != marker.pairing_document_sha256 {
        super::provision::reject_imported_identity_files(
            certificate_path,
            private_key_path,
            &marker.certificate_sha256,
            &marker.private_key_sha256,
        )?;
        return Ok(ImportedIdentityReconciliation::Superseded);
    }

    let document: Document =
        serde_json::from_value(pairing_value.clone()).map_err(|_| "invalid_state")?;
    document.validate(now)?;
    let identity = document.identity.as_ref().ok_or("invalid_state")?;
    if sha256(identity.certificate_pem.as_bytes()) != marker.certificate_sha256
        || sha256(identity.private_key_pem.as_bytes()) != marker.private_key_sha256
    {
        return Err("invalid_state");
    }
    crypto::validate_identity(identity, now)?;
    super::provision::materialize_imported_identity_pair(
        certificate_path,
        private_key_path,
        identity,
        now,
    )?;
    Ok(ImportedIdentityReconciliation::Reconciled)
}

fn validate_marker(marker: &ImportedIdentityMarker) -> Result<(), &'static str> {
    if marker.version != 1
        || !valid_sha256(&marker.pairing_document_sha256)
        || !valid_sha256(&marker.certificate_sha256)
        || !valid_sha256(&marker.private_key_sha256)
    {
        return Err("invalid_state");
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_json(value: &Value) -> Result<String, &'static str> {
    Ok(sha256(
        &serde_json::to_vec(value).map_err(|_| "invalid_state")?,
    ))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Identity;
    use serde_json::json;

    const HOST_CERT: &str = include_str!("../../tests/fixtures/host-cert.pem");
    const HOST_KEY: &str = include_str!("../../tests/fixtures/host-key.pem");

    fn document() -> Document {
        Document {
            identity: Some(Identity {
                host_id: "12345678-1234-4234-8234-123456789abc".into(),
                certificate_pem: HOST_CERT.into(),
                private_key_pem: HOST_KEY.into(),
            }),
            clients: Vec::new(),
        }
    }

    #[test]
    fn marker_contains_only_digests_and_requires_exact_pairing_document() {
        let document = document();
        let value = serde_json::to_value(&document).unwrap();
        let marker = imported_identity_migration_marker(&document).unwrap();
        let serialized = serde_json::to_string(&marker).unwrap();
        assert!(!serialized.contains(HOST_CERT));
        assert!(!serialized.contains(HOST_KEY));

        let values = json!({
            "pairing/v1": value,
            IMPORTED_IDENTITY_MIGRATION_KEY: marker
        });
        let (directory, certificate, private_key) = paths();
        assert!(matches!(
            reconcile_imported_identity(
                &values,
                certificate.to_str().unwrap(),
                private_key.to_str().unwrap(),
                2_000_000_000,
            ),
            Ok(ImportedIdentityReconciliation::Reconciled)
        ));
        assert_eq!(std::fs::read_to_string(certificate).unwrap(), HOST_CERT);
        assert_eq!(std::fs::read_to_string(private_key).unwrap(), HOST_KEY);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn mismatched_destination_document_does_not_install_seed_files() {
        let document = document();
        let marker = imported_identity_migration_marker(&document).unwrap();
        let values = json!({
            "pairing/v1": {"identity": null, "clients": []},
            IMPORTED_IDENTITY_MIGRATION_KEY: marker
        });
        let (directory, certificate, private_key) = paths();
        assert!(matches!(
            reconcile_imported_identity(
                &values,
                certificate.to_str().unwrap(),
                private_key.to_str().unwrap(),
                2_000_000_000,
            ),
            Ok(ImportedIdentityReconciliation::Superseded)
        ));
        assert!(!certificate.exists());
        assert!(!private_key.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn conflicting_seed_material_for_losing_document_fails_closed() {
        let document = document();
        let marker = imported_identity_migration_marker(&document).unwrap();
        let values = json!({
            "pairing/v1": {"identity": null, "clients": []},
            IMPORTED_IDENTITY_MIGRATION_KEY: marker
        });
        let (directory, certificate, private_key) = paths();
        std::fs::write(&certificate, HOST_CERT).unwrap();
        assert!(
            reconcile_imported_identity(
                &values,
                certificate.to_str().unwrap(),
                private_key.to_str().unwrap(),
                2_000_000_000,
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(certificate).unwrap(), HOST_CERT);
        assert!(!private_key.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    fn paths() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-pairing-imported-marker-{}-{}",
            std::process::id(),
            crate::crypto::random_bytes::<8>()
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        (
            directory.clone(),
            directory.join("host-cert.pem"),
            directory.join("host-key.pem"),
        )
    }
}
