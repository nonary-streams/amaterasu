//! Typed access to the host-selected frame-processing composition.
//!
//! The host supplies a view for the calling principal. Receiving a selection
//! does not grant calls or native access; the broker and native binder enforce
//! those grants independently.
use crate::{Command, ModuleResult, Response, RuntimeClient, extension_api};
pub use amaterasu_contracts::processing_composition::{
    CapabilityBinding, PROCESSING_COMPOSITION_VERSION, PROCESSING_INTENT_CONTRACT,
    ProcessingComposition, ProcessingLifecycleGrant, ProcessingRegistration, ProcessingSelection,
    SOURCE_PREPARATION_CONTRACT, STREAM_OWNER_CONTRACT, SettingsAccessGrant, SettingsScopeGrant,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Public lifecycle-participant declaration derived from plugin metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleParticipant {
    pub provider: String,
    pub contract_version: u32,
    pub depends_on: Vec<String>,
    pub plan_method: extension_api::MethodRef,
    pub callback_method: extension_api::MethodRef,
    pub status_method: extension_api::MethodRef,
    pub binding_validate_method: Option<extension_api::MethodRef>,
    pub capabilities: BTreeSet<String>,
    pub cleanup_methods: Vec<extension_api::MethodRef>,
    pub settings_revision: String,
    pub settings: Value,
}

impl RuntimeClient {
    pub fn processing_composition(&self) -> ModuleResult<Option<ProcessingComposition>> {
        let payload = match self.send(Command::GetProcessingComposition {})? {
            Response::Result { payload } => payload,
            Response::Error { code, .. } if code == "not_found" => return Ok(None),
            Response::Error { code, message } => return Err(format!("{code}: {message}")),
            response => return Err(format!("unexpected composition response: {response:?}")),
        };
        let composition: ProcessingComposition =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        validate(&composition)?;
        Ok(Some(composition))
    }
}

/// Validate an interface family before using a selected alias. Native tables
/// must still be bound with the runtime's typed native-contract check.
pub fn require_contract<'a>(
    binding: &'a CapabilityBinding,
    contract: &str,
    major: u32,
    min_minor: u32,
) -> ModuleResult<&'a str> {
    binding.validate().map_err(str::to_owned)?;
    if binding.contract != contract
        || binding.interface_major != major
        || binding.interface_minor < min_minor
    {
        return Err("processing_contract_mismatch".into());
    }
    Ok(&binding.service)
}

/// Validate both the composition envelope and its plugin-owned descriptors.
pub fn validate(composition: &ProcessingComposition) -> ModuleResult<()> {
    composition.validate().map_err(str::to_owned)?;
    if let Some(processor) = &composition.processor {
        require_contract(
            processor,
            crate::media::native_contracts::FRAME_PROCESSOR,
            1,
            0,
        )?;
    }
    if let Some(consumer) = &composition.frame_consumer {
        if consumer.contract != crate::video::CONTRACT
            && consumer.contract != crate::media::native_contracts::FRAME_RESOURCE_CONSUMER
        {
            return Err("processing_consumer_contract_mismatch".into());
        }
        require_contract(consumer, &consumer.contract, 1, 0)?;
    }
    if let Some(control) = &composition.processor_control {
        require_contract(control, PROCESSING_INTENT_CONTRACT, 1, 1)?;
        if control.interface_minor != 1 {
            return Err("processing_contract_mismatch".into());
        }
    }
    for (binding, contract) in [
        (&composition.source_preparation, SOURCE_PREPARATION_CONTRACT),
        (&composition.stream_owner, STREAM_OWNER_CONTRACT),
        (
            &composition.lifecycle,
            extension_api::SESSION_LIFECYCLE_SERVICE,
        ),
        (
            &composition.settings_registry,
            extension_api::EXTENSION_REGISTRY_SERVICE,
        ),
        (
            &composition.display_settings,
            crate::display_settings::SERVICE,
        ),
        (&composition.foreground, crate::foreground_context::SERVICE),
    ] {
        if let Some(binding) = binding {
            require_contract(binding, contract, 1, 0)?;
        }
    }
    if composition.frame_settings.is_some()
        && (composition.processor.is_none()
            || composition.frame_consumer.is_none()
            || composition.lifecycle.is_some())
    {
        return Err("invalid_processing_frame_settings".into());
    }
    if !composition.source_preflight_consumers.is_empty()
        && !composition
            .frame_consumer
            .as_ref()
            .is_some_and(|consumer| consumer.contract == crate::video::CONTRACT)
    {
        return Err("source_preflight_requires_video_contract".into());
    }
    for registration in &composition.registrations {
        let descriptor: extension_api::ExtensionDescriptor =
            serde_json::from_value(registration.descriptor.clone())
                .map_err(|_| "invalid_processing_descriptor")?;
        extension_api::validate_descriptor(&descriptor)?;
        if descriptor.provider != registration.provider {
            return Err("processing_descriptor_provider_mismatch".into());
        }
        let reference = &descriptor.settings_validate;
        if !registration.methods.iter().any(|method| {
            method.service == reference.service
                && method.method == reference.method
                && method.interface_major == reference.interface_major
                && method.interface_minor == reference.interface_minor
        }) {
            return Err("processing_descriptor_method_missing".into());
        }
    }
    Ok(())
}

/// Construct the same participant declaration for any lifecycle provider.
pub fn lifecycle_participant(
    registration: &ProcessingRegistration,
) -> ModuleResult<LifecycleParticipant> {
    registration.validate().map_err(str::to_owned)?;
    let descriptor: extension_api::ExtensionDescriptor =
        serde_json::from_value(registration.descriptor.clone())
            .map_err(|_| "invalid_processing_descriptor")?;
    extension_api::validate_descriptor(&descriptor)?;
    if descriptor.provider != registration.provider {
        return Err("processing_descriptor_provider_mismatch".into());
    }
    let plan = registration_method(registration, "resource_plan")?;
    let callback = registration_method(registration, "lifecycle_callback")?;
    let status = registration_method(registration, "lifecycle_status")?;
    if plan.interface_major != callback.interface_major
        || plan.interface_major != status.interface_major
    {
        return Err("processing_lifecycle_contract_mismatch".into());
    }
    let binding_validate_method = registration
        .methods
        .iter()
        .any(|method| method.method == "binding_validate")
        .then(|| registration_method(registration, "binding_validate"))
        .transpose()?;
    Ok(LifecycleParticipant {
        provider: registration.provider.clone(),
        contract_version: plan.interface_major,
        depends_on: registration.depends_on.clone(),
        plan_method: plan,
        callback_method: callback.clone(),
        status_method: status,
        binding_validate_method,
        capabilities: descriptor.capabilities.into_iter().collect(),
        cleanup_methods: vec![callback],
        settings_revision: registration.settings_revision.clone(),
        settings: registration.initial_settings.clone(),
    })
}

pub fn registration_method(
    registration: &ProcessingRegistration,
    name: &str,
) -> ModuleResult<extension_api::MethodRef> {
    let mut matches = registration
        .methods
        .iter()
        .filter(|method| method.method == name);
    let method = matches.next().ok_or("processing_method_missing")?;
    if matches.next().is_some() {
        return Err("ambiguous_processing_method".into());
    }
    Ok(extension_api::MethodRef {
        service: method.service.clone(),
        method: method.method.clone(),
        interface_major: method.interface_major,
        interface_minor: method.interface_minor,
        schema_revision: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registration() -> ProcessingRegistration {
        let methods = [
            "settings_validate",
            "resource_plan",
            "lifecycle_callback",
            "lifecycle_status",
            "processing_intent",
        ]
        .into_iter()
        .map(|name| crate::operation_api::method("example.effect/1", name, &[], &[]))
        .collect();
        ProcessingRegistration {
            provider: "example.effect".into(),
            descriptor: json!({"provider":"example.effect","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"example.effect/1","method":"settings_validate","interface_major":1,"interface_minor":0,"schema_revision":1}}),
            methods,
            consumers: ["example.consumer".into()].into(),
            depends_on: vec![],
            lifecycle_grants: vec![],
            settings_revision: "r1".into(),
            initial_settings: json!({}),
            settings_grants: vec![],
            migrations: vec![],
        }
    }

    #[test]
    fn sdk_registration_produces_a_provider_neutral_lifecycle_participant() {
        let mut registration = registration();
        let declaration = lifecycle_participant(&registration).unwrap();
        assert_eq!(declaration.provider, "example.effect");
        assert_eq!(declaration.plan_method.service, "example.effect/1");
        assert_eq!(declaration.callback_method.method, "lifecycle_callback");
        assert_eq!(
            declaration.cleanup_methods,
            vec![declaration.callback_method]
        );
        registration
            .methods
            .retain(|method| method.method != "lifecycle_status");
        assert!(lifecycle_participant(&registration).is_err());
    }

    #[test]
    fn service_alias_cannot_substitute_for_native_contract_identity() {
        let mut binding = CapabilityBinding {
            principal: "example.effect".into(),
            service: "example.effect/1".into(),
            contract: "amaterasu.frame-processor.native/1".into(),
            interface_major: 1,
            interface_minor: 2,
        };
        assert_eq!(
            require_contract(&binding, "amaterasu.frame-processor.native/1", 1, 1).unwrap(),
            "example.effect/1"
        );
        binding.service = "replacement.effect/1".into();
        assert!(require_contract(&binding, "amaterasu.frame-processor.native/1", 1, 2).is_ok());
        assert!(require_contract(&binding, "amaterasu.capture.native/1", 1, 1).is_err());
        assert!(require_contract(&binding, "amaterasu.frame-processor.native/1", 2, 1).is_err());
        assert!(require_contract(&binding, "amaterasu.frame-processor.native/1", 1, 3).is_err());
    }

    #[test]
    fn processing_intent_requires_the_current_contract_version() {
        let mut composition = ProcessingComposition {
            processor_control: Some(CapabilityBinding {
                principal: "example.effect".into(),
                service: "example.effect/1".into(),
                contract: PROCESSING_INTENT_CONTRACT.into(),
                interface_major: 1,
                interface_minor: 1,
            }),
            ..Default::default()
        };
        assert!(validate(&composition).is_ok());
        for minor in [0, 2] {
            composition
                .processor_control
                .as_mut()
                .unwrap()
                .interface_minor = minor;
            assert!(validate(&composition).is_err());
        }
    }
}
