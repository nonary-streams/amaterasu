//! Trusted startup selection for coherent control/native provider roles.
//!
//! These are service contracts, not module IDs. The signed composition supplies
//! matching call/native-use grants and provider initialization dependencies.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderServices {
    pub session: String,
    pub video: String,
    pub lifecycle: Option<String>,
    pub audio_capture: String,
    pub audio_encoder: String,
    pub audio_processor: Option<String>,
    pub media_transport: String,
    pub control_transport: String,
}

impl Default for ProviderServices {
    fn default() -> Self {
        Self {
            session: "amaterasu.session/1".into(),
            video: "amaterasu.video/1".into(),
            lifecycle: None,
            audio_capture: "amaterasu.audio/1".into(),
            audio_encoder: "amaterasu.audio-encode/1".into(),
            audio_processor: None,
            media_transport: "amaterasu.media-transport/1".into(),
            control_transport: "amaterasu.control-transport/1".into(),
        }
    }
}

impl ProviderServices {
    pub fn validate(&self) -> Result<(), &'static str> {
        let services = [
            &self.session,
            &self.video,
            &self.audio_capture,
            &self.audio_encoder,
            &self.media_transport,
            &self.control_transport,
        ];
        let mut unique = std::collections::BTreeSet::new();
        for service in services
            .into_iter()
            .chain(self.audio_processor.iter())
            .chain(self.lifecycle.iter())
        {
            let Some((name, version)) = service.split_once('/') else {
                return Err("invalid_provider_services");
            };
            let version_is_valid = version
                .parse::<u32>()
                .is_ok_and(|number| number != 0 && number.to_string() == version);
            if !amaterasu_contracts::valid_name(service)
                || name.is_empty()
                || name.contains('/')
                || !version_is_valid
                || !unique.insert(service)
            {
                return Err("invalid_provider_services");
            }
        }
        Ok(())
    }

    /// A provider cannot also be this instance's own broker endpoint.
    pub fn validate_for_owner(&self, owner_service: &str) -> Result<(), &'static str> {
        self.validate()?;
        if [
            Some(&self.session),
            Some(&self.video),
            self.lifecycle.as_ref(),
            Some(&self.audio_capture),
            Some(&self.audio_encoder),
            self.audio_processor.as_ref(),
            Some(&self.media_transport),
            Some(&self.control_transport),
        ]
        .into_iter()
        .flatten()
        .any(|service| service == owner_service)
        {
            return Err("provider_service_is_stream_owner");
        }
        Ok(())
    }

    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_compatible_and_selected_services_are_bounded_contracts() {
        assert!(ProviderServices::default().validate().is_ok());
        for service in [
            "",
            "Acme.media/1",
            "acme.media/0",
            "acme.media/02",
            "acme.media/not-a-number",
            "acme.media/4294967296",
            "acme/media/1",
        ] {
            let roles = ProviderServices {
                media_transport: service.into(),
                ..Default::default()
            };
            assert!(roles.validate().is_err(), "{service}");
        }
        let roles = ProviderServices {
            media_transport: "acme.media/1".into(),
            ..Default::default()
        };
        assert!(roles.validate().is_ok());
        let newer = ProviderServices {
            audio_encoder: "acme.audio/2".into(),
            ..Default::default()
        };
        assert!(newer.validate().is_ok());
        let duplicate = ProviderServices {
            control_transport: roles.media_transport.clone(),
            ..roles
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn selected_owner_identity_is_used_to_reject_self_calls() {
        let roles = ProviderServices {
            video: "replacement.owner/1".into(),
            ..Default::default()
        };
        assert!(roles.validate_for_owner("replacement.owner/1").is_err());
        assert!(roles.validate_for_owner("replacement.owner/2").is_ok());
        let other = ProviderServices {
            video: "amaterasu.stream-session/1".into(),
            ..Default::default()
        };
        assert!(other.validate_for_owner("replacement.owner/1").is_ok());
    }
}
