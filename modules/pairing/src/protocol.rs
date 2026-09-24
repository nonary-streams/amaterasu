use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_DOCUMENT_BYTES: usize = 12_000;
pub const MAX_CLIENTS: usize = 8;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub administrator_principals: BTreeSet<String>,
    pub transport_principals: BTreeSet<String>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        for principals in [&self.administrator_principals, &self.transport_principals] {
            if principals.len() > 16 || principals.iter().any(|value| !identifier(value, 128)) {
                return Err("invalid_settings");
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Initialize {},
    ImportIdentity {
        certificate_pem: String,
        private_key_pem: String,
        host_id: Option<String>,
    },
    ImportClient {
        client_id: String,
        name: String,
        certificate_pem: String,
    },
    Identity {},
    Begin {
        client_id: String,
        name: String,
        client_certificate_pem: String,
        salt: String,
    },
    ApprovePin {
        pairing_id: String,
        pin: String,
    },
    GetServerCert {
        pairing_id: String,
    },
    ClientChallenge {
        pairing_id: String,
        clientchallenge: String,
    },
    ServerChallengeResponse {
        pairing_id: String,
        serverchallengeresp: String,
    },
    ClientPairingSecret {
        pairing_id: String,
        clientpairingsecret: String,
    },
    Pending {},
    Cancel {
        pairing_id: String,
    },
    ListClients {},
    Unpair {
        certificate_id: String,
    },
    UpdateClientConfigOverrides {
        certificate_id: String,
        config_overrides: ClientConfigOverrides,
        expected_revision: u64,
    },
    CheckClientCertificate {
        certificate_pem: String,
    },
}
impl Operation {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::ImportIdentity {
                certificate_pem,
                private_key_pem,
                host_id,
            } => {
                certificate_pem.len() <= 4096
                    && private_key_pem.len() <= 4096
                    && host_id.as_deref().is_none_or(valid_host_id)
            }
            Self::ImportClient {
                client_id,
                name,
                certificate_pem,
            } => {
                identifier(client_id, 64)
                    && !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                    && certificate_pem.len() <= 4096
            }
            Self::Begin {
                client_id,
                name,
                client_certificate_pem,
                salt,
            } => {
                identifier(client_id, 64)
                    && !name.is_empty()
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                    && client_certificate_pem.len() <= 4096
                    && hex_length(salt, 16)
            }
            Self::ApprovePin { pairing_id, pin } => {
                identifier(pairing_id, 32)
                    && pin.len() == 4
                    && pin.bytes().all(|byte| byte.is_ascii_digit())
            }
            Self::GetServerCert { pairing_id } | Self::Cancel { pairing_id } => {
                identifier(pairing_id, 32)
            }
            Self::ClientChallenge {
                pairing_id,
                clientchallenge,
            } => identifier(pairing_id, 32) && hex_length(clientchallenge, 16),
            Self::ServerChallengeResponse {
                pairing_id,
                serverchallengeresp,
            } => identifier(pairing_id, 32) && hex_length(serverchallengeresp, 32),
            Self::ClientPairingSecret {
                pairing_id,
                clientpairingsecret,
            } => {
                identifier(pairing_id, 32)
                    && clientpairingsecret.len() <= 1056
                    && clientpairingsecret.len() >= 544
                    && hex_length(clientpairingsecret, clientpairingsecret.len() / 2)
            }
            Self::Unpair { certificate_id } => hex_length(certificate_id, 32),
            Self::UpdateClientConfigOverrides {
                certificate_id,
                config_overrides,
                expected_revision: _,
            } => hex_length(certificate_id, 32) && config_overrides.validate().is_ok(),
            Self::CheckClientCertificate { certificate_pem } => certificate_pem.len() <= 4096,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err("invalid_request")
        }
    }
    pub fn may_write(&self) -> bool {
        matches!(
            self,
            Self::Initialize {}
                | Self::ImportIdentity { .. }
                | Self::ImportClient { .. }
                | Self::ClientPairingSecret { .. }
                | Self::Unpair { .. }
                | Self::UpdateClientConfigOverrides { .. }
        )
    }

    pub fn admin_only(&self) -> bool {
        matches!(
            self,
            Self::Initialize {}
                | Self::ImportIdentity { .. }
                | Self::ImportClient { .. }
                | Self::ApprovePin { .. }
                | Self::Pending {}
                | Self::ListClients {}
                | Self::Unpair { .. }
                | Self::UpdateClientConfigOverrides { .. }
        )
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub identity: Option<Identity>,
    pub clients: Vec<Client>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub host_id: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub certificate_id: String,
    pub client_id: String,
    pub name: String,
    pub certificate_pem: String,
    pub paired_at: u64,
    #[serde(default, skip_serializing_if = "ClientConfigOverrides::is_empty")]
    pub config_overrides: ClientConfigOverrides,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfigOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fec_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sink_capture_only: Option<bool>,
}

impl ClientConfigOverrides {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .fec_percentage
            .is_some_and(|percentage| percentage == 0)
        {
            return Err("invalid_config_overrides");
        }
        if self
            .audio_sink
            .as_deref()
            .is_some_and(|endpoint| endpoint.len() > 4096 || endpoint.chars().any(char::is_control))
        {
            return Err("invalid_config_overrides");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.fec_percentage.is_none()
            && self.stream_audio.is_none()
            && self.audio_sink.is_none()
            && self.audio_sink_capture_only.is_none()
    }
}
impl Document {
    pub fn validate(&self, now: u64) -> Result<(), &'static str> {
        if self.clients.len() > MAX_CLIENTS
            || serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_DOCUMENT_BYTES)
        {
            return Err("invalid_state");
        }
        if let Some(identity) = &self.identity {
            if !valid_host_id(&identity.host_id) {
                return Err("invalid_state");
            }
            crate::crypto::validate_identity(identity, now).map_err(|_| "invalid_state")?;
        } else if !self.clients.is_empty() {
            return Err("invalid_state");
        }
        let mut ids = BTreeSet::new();
        for client in &self.clients {
            // Expired paired certificates remain removable/listable. Authentication
            // validates time separately; loading old records never renews trust.
            let certificate = crate::crypto::parse_certificate(&client.certificate_pem, None)
                .map_err(|_| "invalid_state")?;
            if !ids.insert(&client.certificate_id)
                || certificate.fingerprint != client.certificate_id
                || !identifier(&client.client_id, 64)
                || client.name.is_empty()
                || client.name.len() > 128
                || client.config_overrides.validate().is_err()
            {
                return Err("invalid_state");
            }
        }
        Ok(())
    }
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
}
fn hex_length(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn error(code: &str) -> Value {
    json!({"ok":false,"paired":0,"code":code})
}

pub fn valid_host_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_audio_override_is_sparse_and_preserves_legacy_fec_records() {
        let legacy: ClientConfigOverrides =
            serde_json::from_value(json!({"fec_percentage":73})).unwrap();
        assert_eq!(legacy.fec_percentage, Some(73));
        assert_eq!(legacy.stream_audio, None);
        legacy.validate().unwrap();
        assert_eq!(
            serde_json::to_value(legacy).unwrap(),
            json!({"fec_percentage":73})
        );

        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(serde_json::to_value(inherited).unwrap(), json!({}));

        let disabled: ClientConfigOverrides = serde_json::from_value(json!({
            "fec_percentage":73,
            "stream_audio":false
        }))
        .unwrap();
        assert_eq!(disabled.fec_percentage, Some(73));
        assert_eq!(disabled.stream_audio, Some(false));
        assert!(!disabled.is_empty());
        disabled.validate().unwrap();
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"fec_percentage":73,"stream_audio":false})
        );
    }

    #[test]
    fn client_audio_sink_preserves_explicit_automatic_and_rejects_controls() {
        let automatic: ClientConfigOverrides =
            serde_json::from_value(json!({"audio_sink":""})).unwrap();
        automatic.validate().unwrap();
        assert!(!automatic.is_empty());
        assert_eq!(
            serde_json::to_value(automatic).unwrap(),
            json!({"audio_sink":""})
        );
        for endpoint in ["bad\nendpoint".to_owned(), "x".repeat(4097)] {
            let override_value: ClientConfigOverrides =
                serde_json::from_value(json!({"audio_sink":endpoint})).unwrap();
            assert!(override_value.validate().is_err());
        }
    }

    #[test]
    fn client_audio_sink_capture_only_preserves_absence_and_explicit_false() {
        let inherited = ClientConfigOverrides::default();
        assert!(inherited.is_empty());
        assert_eq!(inherited.audio_sink_capture_only, None);

        let disabled: ClientConfigOverrides =
            serde_json::from_value(json!({"audio_sink_capture_only":false})).unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.audio_sink_capture_only, Some(false));
        assert!(!disabled.is_empty());
        assert_eq!(
            serde_json::to_value(disabled).unwrap(),
            json!({"audio_sink_capture_only":false})
        );
        assert!(
            serde_json::from_value::<ClientConfigOverrides>(json!({
                "audio_sink_capture_only":0
            }))
            .is_err()
        );
    }
}
