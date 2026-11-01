//! Independently built, startup-configured streaming policy authority.
//! All rules deny by default. No service grant is installed by this example.
use amaterasu_module_sdk::application_library::{
    policy::{Identity, Snapshot},
    streaming_policy::{Adapter, Admission, Provider, Request},
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    service: String,
    callers: BTreeMap<String, BTreeSet<String>>,
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    certificates: BTreeSet<String>,
    policy: Snapshot,
    config_overrides: Value,
    application_display_settings: Option<Value>,
    #[serde(default)]
    exclude_global_preparation: bool,
}

struct StaticProvider {
    rules: Vec<Rule>,
}

impl Provider for StaticProvider {
    fn admit(&mut self, _: &Identity<'_>, request: &Request) -> Result<Admission, &'static str> {
        let mut matching = self.rules.iter().filter(|rule| {
            rule.policy.application == request.application
                && rule.policy.entry_revision == request.entry_revision
                && rule.certificates.contains(&request.certificate_id)
        });
        let rule = matching.next().ok_or("not_authorized")?;
        if matching.next().is_some() {
            return Err("ambiguous_policy");
        }
        Ok(Admission {
            certificate_id: request.certificate_id.clone(),
            policy: rule.policy.clone(),
            config_overrides: rule.config_overrides.clone(),
            application_display_settings: rule.application_display_settings.clone(),
            exclude_global_preparation: rule.exclude_global_preparation,
        })
    }
}

struct Module {
    runtime: RuntimeClient,
    adapter: Adapter<StaticProvider>,
}
impl ManagedModule for Module {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("streaming policy settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid policy settings")?;
        if settings.rules.len() > 256
            || settings.rules.iter().any(|rule| {
                rule.certificates.is_empty()
                    || rule.certificates.len() > 64
                    || rule.certificates.iter().any(|certificate| {
                        certificate.len() != 64
                            || !certificate
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                    || !rule.config_overrides.is_object()
                    || rule
                        .application_display_settings
                        .as_ref()
                        .is_some_and(|value| !value.is_object())
            })
        {
            return Err("invalid policy rules".into());
        }
        let service = settings.service;
        let adapter = Adapter::new(
            StaticProvider {
                rules: settings.rules,
            },
            service.clone(),
            settings.callers,
        )
        .map_err(str::to_owned)?;
        runtime.require_success(Command::Register { service })?;
        Ok(Self { runtime, adapter })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let payload = self.adapter.dispatch(&request);
            self.runtime.require_success(Command::Complete {
                request: request.id,
                payload,
            })?;
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Module);

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::application_library::{
        streaming_policy::AdmitAction, v2::ApplicationReference,
    };
    use serde_json::json;

    fn rule(library: &str, revision: &str, certificate: &str) -> Rule {
        Rule {
            certificates: [certificate.into()].into(),
            policy: Snapshot {
                application: ApplicationReference {
                    library: library.into(),
                    id: "opaque/game".into(),
                },
                entry_revision: "entry:one".into(),
                policy_revision: revision.into(),
                working_directory: None,
                exit_timeout_seconds: 10,
                preparation: vec![],
            },
            config_overrides: json!({}),
            application_display_settings: None,
            exclude_global_preparation: false,
        }
    }
    #[test]
    fn independent_configurations_authorize_only_the_exact_certificate_and_source() {
        let certificate = "ab".repeat(32);
        let identity = Identity {
            caller: "amaterasu.launch",
            initiator: "amaterasu.nvhttp",
        };
        for (library, revision) in [("first", "policy:first"), ("second", "policy:second")] {
            let mut provider = StaticProvider {
                rules: vec![rule(library, revision, &certificate)],
            };
            let mut request = Request {
                action: AdmitAction::Admit,
                application: ApplicationReference {
                    library: library.into(),
                    id: "opaque/game".into(),
                },
                entry_revision: "entry:one".into(),
                certificate_id: certificate.clone(),
            };
            let admission = provider.admit(&identity, &request).unwrap();
            assert!(admission.validate_for(&request).is_ok());
            assert_eq!(admission.policy.policy_revision, revision);
            request.certificate_id = "cd".repeat(32);
            assert!(provider.admit(&identity, &request).is_err());
            request.certificate_id = certificate.clone();
            request.application.library = "other".into();
            assert!(provider.admit(&identity, &request).is_err());
        }
    }
}
