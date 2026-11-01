//! Global host settings, revisioned through the host-owned state provider.
mod path;
mod pipeline;
mod protocol;
mod settings;

#[cfg(any(not(feature = "test-support"), test))]
use amaterasu_module_sdk::ModuleResult;
#[cfg(not(feature = "test-support"))]
use amaterasu_module_sdk::{Command, ManagedModule, Response, RuntimeClient};
use pipeline::Pipeline;

pub const SERVICE: &str = "amaterasu.host-configuration/1";
const STATE_SERVICE: &str = "amaterasu.state/1";

#[cfg(feature = "test-support")]
pub mod test_support {
    use amaterasu_module_sdk::{Command, ModuleResult, Response};
    use std::path::PathBuf;

    pub struct TestHostConfiguration(super::Pipeline);

    impl TestHostConfiguration {
        pub fn new(root: PathBuf, effect_authority_enabled: bool) -> Self {
            let mut pipeline = super::Pipeline::new(Ok(root));
            if effect_authority_enabled {
                pipeline.enable_effect_authority("amaterasu.pairing".into());
            }
            Self(pipeline)
        }

        pub fn tick(
            &mut self,
            send: impl FnMut(Command) -> ModuleResult<Response>,
        ) -> ModuleResult<()> {
            self.0.tick(send)
        }
    }
}

#[cfg(not(feature = "test-support"))]
struct HostConfigurationModule {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}

#[cfg(any(not(feature = "test-support"), test))]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectAuthoritySelection {
    principal: String,
}

#[cfg(any(not(feature = "test-support"), test))]
fn effect_authority_selection(settings: &serde_json::Value) -> ModuleResult<Option<String>> {
    let Some(value) = settings.get("pairing_effect_authority") else {
        return Ok(None);
    };
    let selection: EffectAuthoritySelection = serde_json::from_value(value.clone())
        .map_err(|_| "invalid pairing effect authority selection")?;
    if selection.principal.is_empty()
        || selection.principal.len() > 128
        || selection.principal.chars().any(char::is_control)
    {
        return Err("invalid pairing effect authority principal".into());
    }
    Ok(Some(selection.principal))
}

#[cfg(not(feature = "test-support"))]
impl ManagedModule for HostConfigurationModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings = match runtime.send(Command::GetSettings {})? {
            Response::Result { payload } => payload,
            response => return Err(format!("host configuration settings failed: {response:?}")),
        };
        let trusted_root = path::trusted_root_from_settings(&settings);
        let effect_authority_owner = effect_authority_selection(&settings)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let mut pipeline = Pipeline::new(trusted_root);
        if let Some(owner) = effect_authority_owner {
            pipeline.enable_effect_authority(owner);
        }
        Ok(Self { runtime, pipeline })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline.tick(|command| self.runtime.send(command))
    }
}

#[cfg(not(feature = "test-support"))]
amaterasu_module_sdk::export_module!(HostConfigurationModule);

#[cfg(test)]
mod selection_tests {
    use super::effect_authority_selection;
    use serde_json::json;

    #[test]
    fn pairing_authority_requires_an_explicit_bounded_principal() {
        assert_eq!(effect_authority_selection(&json!({})).unwrap(), None);
        assert_eq!(
            effect_authority_selection(
                &json!({"pairing_effect_authority":{"principal":"custom.pairing"}})
            )
            .unwrap(),
            Some("custom.pairing".into())
        );
        for invalid in [
            json!(true),
            json!({"principal":""}),
            json!({"principal":"bad\nname"}),
            json!({"principal":"x","extra":true}),
        ] {
            assert!(
                effect_authority_selection(&json!({"pairing_effect_authority":invalid})).is_err()
            );
        }
    }
}
