//! Moonlight Gen7 host pairing cryptographic state machine.
mod crypto;
pub mod effect_gate;
mod engine;
mod identity;
mod identity_generation;
pub mod migration;
mod pipeline;
mod protocol;
mod transcript;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use pipeline::Pipeline;
use protocol::Settings;
pub const SERVICE: &str = "amaterasu.pairing/1";
const STATE_SERVICE: &str = "amaterasu.state/1";
pub const EFFECT_SERVICE: &str = amaterasu_module_sdk::effect_authority::SERVICE;

pub struct PairingModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
    _identity_factory: Box<identity::Factory>,
}
impl ManagedModule for PairingModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("pairing settings unavailable".into());
        };
        let (settings, host_configuration_enabled) =
            parse_settings(payload).map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        if settings.effect_authority.is_some() {
            runtime.require_success(Command::Register {
                service: EFFECT_SERVICE.into(),
            })?;
        }
        let shared = std::sync::Arc::new(identity::Shared::default());
        let factory = identity::Factory::new(shared.clone(), settings.transport_principals.clone());
        // SAFETY: boxed factory context is stable; signed dependency order stops
        // native consumers before pairing destruction. Callbacks share immutable data.
        unsafe { runtime.register_native(amaterasu_identity::SERVICE, &factory.api) }?;
        let pipeline =
            Pipeline::with_host_configuration(settings, shared, host_configuration_enabled);
        if !pipeline.authority_available() {
            return Err("effect authority initialization failed".into());
        }
        Ok(Self {
            runtime,
            pipeline,
            _identity_factory: factory,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline
            .tick(|command| self.runtime.send_ephemeral(command))
    }
}
impl Drop for PairingModule {
    fn drop(&mut self) {
        self.pipeline.invalidate_identity();
    }
}

amaterasu_module_sdk::export_module!(PairingModule);

fn parse_settings(mut payload: serde_json::Value) -> Result<(Settings, bool), &'static str> {
    let host_configuration_enabled = match payload
        .as_object_mut()
        .and_then(|settings| settings.remove("host_configuration"))
    {
        None => false,
        Some(serde_json::Value::Bool(enabled)) => enabled,
        Some(_) => return Err("invalid pairing settings"),
    };
    let settings: Settings =
        serde_json::from_value(payload).map_err(|_| "invalid pairing settings")?;
    settings.validate()?;
    Ok((settings, host_configuration_enabled))
}

#[cfg(test)]
mod settings_tests {
    use super::parse_settings;
    use serde_json::json;

    #[test]
    fn missing_host_configuration_marker_keeps_legacy_settings() {
        let (settings, enabled) = parse_settings(json!({})).unwrap();
        assert!(!enabled);
        settings.validate().unwrap();
    }

    #[test]
    fn platform_host_configuration_marker_is_consumed_outside_protocol_settings() {
        let (settings, enabled) = parse_settings(json!({"host_configuration":true})).unwrap();
        assert!(enabled);
        settings.validate().unwrap();
    }

    #[test]
    fn host_configuration_marker_must_be_boolean() {
        assert!(parse_settings(json!({"host_configuration":"enabled"})).is_err());
    }
}
