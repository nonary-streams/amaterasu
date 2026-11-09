#![recursion_limit = "256"]
//! Streaming behavior belongs to a managed library, not the native shell.
mod model;
mod pipeline;
mod plane;
mod providers;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::Settings;
use pipeline::Pipeline;

pub(crate) fn public_api_methods() -> Vec<amaterasu_module_sdk::ApiMethod> {
    use amaterasu_module_sdk::{ApiFieldKind, operation_api};
    vec![
        operation_api::method("nonary.stream-status/1", "snapshot", &[], &[]),
        operation_api::method(
            "nonary.stream-status/1",
            "stop_stream",
            &[("session_id", ApiFieldKind::String)],
            &[],
        ),
    ]
}

const SERVICE: &str = "amaterasu.stream-session/1";
struct StreamSession {
    runtime: RuntimeClient,
    pipeline: Pipeline,
}
impl ManagedModule for StreamSession {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("stream session settings unavailable".into());
        };
        let mut settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid stream session settings")?;
        let service = if let Some(composition) = runtime.processing_composition()? {
            configure_processing(&mut settings, &composition)?
        } else {
            SERVICE.to_owned()
        };
        settings.validate()?;
        let mut pipeline = Pipeline::new(settings, service.clone())?;
        runtime.require_success(Command::Register { service })?;
        amaterasu_module_sdk::operation_api::register_selected(&runtime, &public_api_methods())?;
        // A host may select this provider as its non-disruptive activation
        // barrier. Older hosts and profiles continue without the capability.
        if matches!(
            runtime.send(Command::HostPackageBarrierRegister {}),
            Ok(Response::Ok)
        ) {
            pipeline.enable_package_barrier();
        }
        Ok(Self { runtime, pipeline })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        self.pipeline.tick(&self.runtime)
    }

    fn prepare_shutdown(&mut self, remaining_ms: u64) -> ModuleResult<()> {
        let now = self.pipeline.elapsed_millis();
        self.pipeline.prepare_shutdown(now, remaining_ms);
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        let now = self.pipeline.elapsed_millis();
        Ok(self.pipeline.quiesce(now))
    }
}

fn configure_processing(
    settings: &mut Settings,
    composition: &amaterasu_module_sdk::processing_composition::ProcessingComposition,
) -> ModuleResult<String> {
    use amaterasu_module_sdk::{media::native_contracts, processing_composition as sdk};
    let owner = composition
        .stream_owner
        .as_ref()
        .ok_or("stream owner not selected")?;
    let service = sdk::require_contract(owner, sdk::STREAM_OWNER_CONTRACT, 1, 0)?.to_owned();
    let processor = composition
        .processor
        .as_ref()
        .ok_or("processor not selected")?;
    sdk::require_contract(processor, native_contracts::FRAME_PROCESSOR, 1, 0)?;
    let consumer = composition
        .frame_consumer
        .as_ref()
        .ok_or("frame consumer not selected")?;
    sdk::require_contract(consumer, amaterasu_module_sdk::video::CONTRACT, 1, 0)?;
    let control = composition
        .processor_control
        .as_ref()
        .ok_or("processing intent not selected")?;
    sdk::require_contract(control, sdk::PROCESSING_INTENT_CONTRACT, 1, 1)?;
    if control.principal != processor.principal {
        return Err("processing intent belongs to another provider".into());
    }
    settings.providers.video = consumer.service.clone();
    for caller in &composition.lifecycle_admitters {
        if !settings.allowed_principals.contains(caller) {
            settings.allowed_principals.push(caller.clone());
        }
    }
    settings
        .lifecycle_admitters
        .extend(composition.lifecycle_admitters.iter().cloned());
    settings
        .history_owners
        .extend(composition.history_owners.iter().cloned());
    if let Some(lifecycle) = &composition.lifecycle {
        sdk::require_contract(
            lifecycle,
            amaterasu_module_sdk::extension_api::SESSION_LIFECYCLE_SERVICE,
            1,
            0,
        )?;
        settings.providers.lifecycle = Some(lifecycle.service.clone());
        settings.processing_plan = Some(model::ProcessingPlanProvider {
            provider: processor.principal.clone(),
            control_service: control.service.clone(),
            processor_service: processor.service.clone(),
            interface_minor: control.interface_minor,
            omit_disabled: true,
            live_settings: composition.settings_registry.is_some(),
        });
    }
    if let Some(display) = &composition.display_settings {
        sdk::require_contract(
            display,
            amaterasu_module_sdk::display_settings::SERVICE,
            1,
            0,
        )?;
        if composition.lifecycle.is_none() {
            return Err("source preparation needs lifecycle settings".into());
        }
        let hook = composition
            .source_preparation
            .as_ref()
            .ok_or("source preparation hook not selected")?;
        sdk::require_contract(hook, sdk::SOURCE_PREPARATION_CONTRACT, 1, 0)?;
        if hook.principal != processor.principal {
            return Err("source preparation belongs to another provider".into());
        }
        let preparation =
            settings
                .source_preparation
                .get_or_insert_with(|| model::SourcePreparation {
                    display_settings_service: display.service.clone(),
                    hooks: Vec::new(),
                });
        if preparation.display_settings_service != display.service {
            return Err("display preparation selection conflicts".into());
        }
        if preparation
            .hooks
            .iter()
            .any(|old| old.provider == hook.principal && old.service != hook.service)
        {
            return Err("source preparation hook conflicts".into());
        }
        if !preparation
            .hooks
            .iter()
            .any(|old| old.provider == hook.principal)
        {
            preparation.hooks.push(model::SourceHook {
                provider: hook.principal.clone(),
                service: hook.service.clone(),
            });
        }
    } else if composition.source_preparation.is_some() {
        return Err("source preparation has no display provider".into());
    }
    Ok(service)
}
impl Drop for StreamSession {
    fn drop(&mut self) {
        self.pipeline.shutdown(&self.runtime);
    }
}

amaterasu_module_sdk::export_module!(StreamSession);

#[cfg(test)]
mod migrated_policy_tests {
    use super::*;

    fn migrated_policy() -> serde_json::Value {
        let path = std::env::var("AMATERASU_MIGRATED_REPORT").unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        report["optional_bootstrap"]["policy"].clone()
    }

    #[test]
    #[ignore = "needs AMATERASU_MIGRATED_REPORT from the release review"]
    fn reviewed_installed_policy_starts_stream_session() {
        let policy = migrated_policy();
        let id = "amaterasu.stream-session";
        let mut settings: Settings =
            serde_json::from_value(policy["settings"][id].clone()).unwrap();
        let composition =
            serde_json::from_value(policy["processing_compositions"][id].clone()).unwrap();
        configure_processing(&mut settings, &composition).unwrap();
        settings.validate().unwrap();
        assert!(settings.processing_plan.is_some());
    }
}
