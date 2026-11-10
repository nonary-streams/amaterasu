//! A layer that guards other modules' services.
//!
//! Trusted composition stacks this module on the services named in its
//! settings. For each one it applies a per-caller rule on top of the broker's
//! grants, declines the callers the rule excludes, and passes every other
//! call on unchanged. The services' consumers and providers are not rebuilt
//! and do not know the guard is there.
//!
//! It is an example of the layer helpers in `amaterasu_module_sdk::requests`,
//! and small enough to copy.
use amaterasu_module_sdk::requests::{Answer, CallOutcome, IncomingRequest, Relay};
use amaterasu_module_sdk::{
    DirectCall, DirectReply, ErrorCode, LogLevel, ManagedModule, ModuleResult, RuntimeClient,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Requests taken from the broker in one tick. The rest wait in its queue.
const REQUESTS_PER_TICK: usize = 32;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    /// The services this module wraps, each with the rule it applies.
    services: BTreeMap<String, Rule>,
}

/// Who may use one guarded service.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Rule {
    /// When not empty, only these callers pass.
    allow: Vec<String>,
    /// These callers never pass.
    deny: Vec<String>,
}

impl Rule {
    fn admits(&self, caller: &str) -> bool {
        let listed = |callers: &[String]| callers.iter().any(|listed| listed == caller);
        !listed(&self.deny) && (self.allow.is_empty() || listed(&self.allow))
    }
}

impl Settings {
    /// A service without a rule is one this module was not asked to guard.
    /// The broker only delivers calls for services it wraps, so that is a
    /// composition mistake, and the call is declined.
    fn admits(&self, service: &str, caller: &str) -> bool {
        self.services
            .get(service)
            .is_some_and(|rule| rule.admits(caller))
    }
}

pub struct ServiceGuard {
    runtime: RuntimeClient,
    settings: Settings,
    relay: Relay,
}

impl ManagedModule for ServiceGuard {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: Settings = runtime
            .settings()
            .map_err(|error| format!("invalid service guard settings: {error}"))?;
        for service in settings.services.keys() {
            // Policy must list this module as a layer of each service.
            runtime
                .wrap(service)
                .map_err(|error| format!("cannot guard {service}: {error}"))?;
        }
        Ok(Self {
            runtime,
            settings,
            relay: Relay::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        for _ in 0..REQUESTS_PER_TICK {
            let Some(request) = self.runtime.next_request()? else {
                break;
            };
            self.admit(request)?;
        }
        let runtime = &self.runtime;
        self.relay.poll_with(runtime, |request, outcome| {
            report_outcome(runtime, request, &outcome);
            Answer::relay(outcome)
        })?;
        Ok(())
    }

    /// A direct call is passed on when its caller is admitted. Declining
    /// sends the consumer to an ordinary request, which `tick` then refuses
    /// with a reason; a declined direct call must leave no effect behind.
    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<Value> {
        if !self.settings.admits(call.service, call.caller) {
            return None;
        }
        match self.runtime.call_direct(call.service, call.payload) {
            Ok(DirectReply::Completed(reply)) => Some(reply),
            _ => None,
        }
    }
}

impl ServiceGuard {
    fn admit(&mut self, request: IncomingRequest<Value>) -> ModuleResult<()> {
        if self.settings.admits(&request.service, &request.caller) {
            self.relay.forward(request)?;
            return Ok(());
        }
        log(
            &self.runtime,
            LogLevel::Warning,
            &format!("declined {} from {}", request.service, request.caller),
        );
        request.refuse(
            &self.runtime,
            ErrorCode::PermissionDenied.as_str(),
            "the service guard does not admit this caller",
        )?;
        Ok(())
    }
}

/// One line per call that was passed on. The payload is never logged.
fn report_outcome(
    runtime: &RuntimeClient,
    request: &IncomingRequest<Value>,
    outcome: &CallOutcome,
) {
    let ending = match outcome {
        CallOutcome::Completed(_) => "completed",
        CallOutcome::Refused(_) => "refused",
        CallOutcome::Unconfirmed(_) => "unconfirmed",
    };
    log(
        runtime,
        LogLevel::Debug,
        &format!("{} from {} {ending}", request.service, request.caller),
    );
}

/// Logging is advisory: a full log queue must not fail the guard's tick.
fn log(runtime: &RuntimeClient, level: LogLevel, message: &str) {
    let _ = runtime.log(level, "service-guard", message);
}

amaterasu_module_sdk::export_module!(ServiceGuard);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Value) -> Settings {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_rule_denies_first_and_then_restricts_to_its_allow_list() {
        let open = Rule::default();
        assert!(open.admits("anyone"));

        let settings = settings(json!({"services": {
            "example.notes/1": {"allow": ["console", "tray"], "deny": ["tray"]},
            "example.open/1": {},
        }}));
        assert!(settings.admits("example.notes/1", "console"));
        assert!(!settings.admits("example.notes/1", "tray"));
        assert!(!settings.admits("example.notes/1", "other"));
        assert!(settings.admits("example.open/1", "other"));
        // A service the guard was never asked to wrap admits nobody.
        assert!(!settings.admits("example.unlisted/1", "console"));
    }

    #[test]
    fn unknown_settings_are_rejected() {
        assert!(serde_json::from_value::<Settings>(json!({"service": {}})).is_err());
        assert!(
            serde_json::from_value::<Settings>(json!({"services": {"a/1": {"permit": []}}}))
                .is_err()
        );
    }
}
