//! Fixed Moonlight Gen7 challenge transcript; AES-ECB is wire compatibility only.
use crate::crypto;
use crate::protocol::{Client, Identity};
use subtle::ConstantTimeEq;

pub enum Phase {
    AwaitingPin,
    Approved {
        key: [u8; 16],
    },
    CertificateSent {
        key: [u8; 16],
    },
    Challenged {
        key: [u8; 16],
        secret: [u8; 16],
        challenge: [u8; 16],
    },
    AwaitingSecret {
        expected_hash: [u8; 32],
        challenge: [u8; 16],
    },
}

pub struct Transcript {
    pub owner: String,
    pub client: Client,
    pub salt: [u8; 16],
    pub expires_at: u64,
    pub phase: Phase,
}

impl Transcript {
    pub fn approve(&mut self, pin: &str) -> Result<(), &'static str> {
        if !matches!(self.phase, Phase::AwaitingPin) {
            return Err("invalid_phase");
        }
        let key = crypto::digest(&[&self.salt, pin.as_bytes()]);
        self.phase = Phase::Approved {
            key: key[..16].try_into().map_err(|_| "pairing_failed")?,
        };
        Ok(())
    }

    pub fn server_certificate(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<String>, &'static str> {
        match &self.phase {
            Phase::AwaitingPin => Ok(None),
            Phase::Approved { key } => {
                self.phase = Phase::CertificateSent { key: *key };
                Ok(Some(crypto::hex(identity.certificate_pem.as_bytes())))
            }
            _ => Err("invalid_phase"),
        }
    }

    pub fn client_challenge(
        &mut self,
        identity: &Identity,
        encrypted: &str,
    ) -> Result<String, &'static str> {
        let Phase::CertificateSent { key } = &self.phase else {
            return Err("invalid_phase");
        };
        let challenge = crypto::crypt(key, &crypto::unhex(encrypted)?, false)?;
        if challenge.len() != 16 {
            return Err("invalid_request");
        }
        let host_certificate = crypto::parse_certificate(&identity.certificate_pem, None)?;
        let secret = crypto::random_bytes::<16>()?;
        let server_challenge = crypto::random_bytes::<16>()?;
        let digest = crypto::digest(&[
            &challenge,
            crypto::signature_bytes(&host_certificate.certificate)?,
            &secret,
        ]);
        let mut response = digest.to_vec();
        response.extend_from_slice(&server_challenge);
        let encrypted = crypto::crypt(key, &response, true)?;
        self.phase = Phase::Challenged {
            key: *key,
            secret,
            challenge: server_challenge,
        };
        Ok(crypto::hex(&encrypted))
    }

    pub fn server_challenge_response(
        &mut self,
        identity: &Identity,
        encrypted: &str,
    ) -> Result<String, &'static str> {
        let Phase::Challenged {
            key,
            secret,
            challenge,
        } = &self.phase
        else {
            return Err("invalid_phase");
        };
        let expected_hash: [u8; 32] = crypto::crypt(key, &crypto::unhex(encrypted)?, false)?
            .try_into()
            .map_err(|_| "invalid_request")?;
        let mut proof = secret.to_vec();
        proof.extend_from_slice(&crypto::sign(identity, secret)?);
        self.phase = Phase::AwaitingSecret {
            expected_hash,
            challenge: *challenge,
        };
        Ok(crypto::hex(&proof))
    }

    pub fn finish(self, proof: &str, now: u64) -> Result<Client, &'static str> {
        let Phase::AwaitingSecret {
            expected_hash,
            challenge,
        } = self.phase
        else {
            return Err("invalid_phase");
        };
        let certificate = crypto::parse_certificate(&self.client.certificate_pem, Some(now))?;
        let proof = crypto::unhex(proof)?;
        if proof.len() < 16 {
            return Err("pairing_failed");
        }
        let (secret, signature) = proof.split_at(16);
        let calculated = crypto::digest(&[
            &challenge,
            crypto::signature_bytes(&certificate.certificate)?,
            secret,
        ]);
        // Check both cryptographic commitments; no ordinary string/byte equality
        // is used for the PIN-derived hash comparison.
        let hash_matches = bool::from(calculated.ct_eq(&expected_hash));
        let signature_valid = crypto::verify(&certificate.public_key, secret, signature).is_ok();
        if !hash_matches || !signature_valid {
            return Err("pairing_failed");
        }
        Ok(Client {
            paired_at: now,
            ..self.client
        })
    }
}
