//! Trusted foreground-window observation. Missing evidence is reported as unavailable.
mod observer;
mod platform;

use amaterasu_module_sdk::foreground_context::{Request as Operation, SERVICE, Snapshot};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use observer::Observer;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    allowed_principals: Vec<String>,
}

impl Settings {
    fn validate(&self) -> Result<(), &'static str> {
        if self.allowed_principals.len() > 16
            || self
                .allowed_principals
                .iter()
                .any(|p| p.is_empty() || p.len() > 128 || p.chars().any(char::is_control))
        {
            return Err("invalid_settings");
        }
        Ok(())
    }
}

fn new_snapshot(executable: String, fullscreen: Option<bool>) -> Option<Snapshot> {
    let snapshot = Snapshot {
        executable,
        fullscreen,
    };
    snapshot.validate().ok()?;
    Some(snapshot)
}

struct ForegroundModule {
    runtime: RuntimeClient,
    settings: Settings,
    observer: Observer,
}

fn authorize(settings: &Settings, caller: &str, payload: Value) -> Result<(), Value> {
    if !settings
        .allowed_principals
        .iter()
        .any(|principal| principal == caller)
    {
        return Err(json!({"ok":false,"code":"forbidden"}));
    }
    if payload != json!({"action":"snapshot"})
        || serde_json::from_value::<Operation>(payload).is_err()
    {
        return Err(json!({"ok":false,"code":"invalid_request"}));
    }
    Ok(())
}

fn snapshot_response(snapshot: Option<Snapshot>) -> Value {
    json!({"ok":true,"snapshot":snapshot})
}

#[cfg(test)]
fn handle(
    settings: &Settings,
    caller: &str,
    payload: Value,
    observe: impl FnOnce() -> Option<Snapshot>,
) -> Value {
    match authorize(settings, caller, payload) {
        Ok(()) => snapshot_response(observe()),
        Err(response) => response,
    }
}

impl ManagedModule for ForegroundModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("foreground settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid foreground settings")?;
        settings.validate()?;
        platform::reconcile();
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            settings,
            observer: Observer::start(platform::observe)?,
        })
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.deliver_finished_observation()?;
        Ok(self.observer.retire())
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.deliver_finished_observation()?;
        // Requests stay queued in the broker while an observation is running.
        if self.observer.busy() {
            return Ok(());
        }
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                ..
            } => {
                if !matches!(
                    self.runtime.send(Command::CheckRequest { request: id })?,
                    Response::Ok
                ) {
                    return Ok(());
                }
                if service != SERVICE {
                    return self.complete(id, json!({"ok":false,"code":"invalid_service"}));
                }
                match authorize(&self.settings, &caller, payload) {
                    Ok(()) if self.observer.submit(id) => Ok(()),
                    Ok(()) => self.complete(id, snapshot_response(None)),
                    Err(response) => self.complete(id, response),
                }
            }
            _ => Err("foreground poll failed".into()),
        }
    }
}

impl ForegroundModule {
    fn deliver_finished_observation(&mut self) -> ModuleResult<()> {
        match self.observer.poll() {
            Some(observation) => {
                self.complete(observation.request, snapshot_response(observation.snapshot))
            }
            None => Ok(()),
        }
    }

    fn complete(&mut self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            _ => Err("foreground completion failed".into()),
        }
    }
}

amaterasu_module_sdk::export_module!(ForegroundModule);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_precedes_observation_and_request_validation() {
        let settings = Settings {
            allowed_principals: vec!["consumer".into()],
        };
        let result = handle(&settings, "other", json!({"action":"snapshot"}), || {
            panic!("unauthorized observation")
        });
        assert_eq!(result["code"], "forbidden");
        let result = handle(
            &settings,
            "consumer",
            json!({"action":"snapshot","extra":1}),
            || panic!("invalid observation"),
        );
        assert_eq!(result["code"], "invalid_request");
        assert_eq!(
            handle(&settings, "consumer", json!({"action":"snapshot"}), || None),
            json!({"ok":true,"snapshot":null})
        );
    }
    #[test]
    fn sdk_consumer_accepts_actual_dispatch_and_independent_replacement() {
        use amaterasu_module_sdk::foreground_context::{Request, SnapshotResponse};

        fn consume(provider: impl FnOnce(Value) -> Value) -> Option<Snapshot> {
            let request = serde_json::to_value(Request::Snapshot).unwrap();
            SnapshotResponse::parse(&provider(request))
                .unwrap()
                .snapshot
        }
        let settings = Settings {
            allowed_principals: vec!["consumer".into()],
        };
        let first = consume(|request| {
            handle(&settings, "consumer", request, || {
                new_snapshot("/games/example".into(), Some(true))
            })
        });
        // A separately implemented provider handles the same SDK request and
        // serializes its own response; it does not use this module's dispatch.
        let second = consume(|request| {
            let _: Request = serde_json::from_value(request).unwrap();
            serde_json::to_value(SnapshotResponse {
                ok: true,
                snapshot: Some(Snapshot {
                    executable: "/games/example".into(),
                    fullscreen: Some(true),
                }),
            })
            .unwrap()
        });
        assert_eq!(first, second);
        assert!(first.unwrap().fullscreen.unwrap());
    }

    #[test]
    fn executable_identity_is_bounded() {
        assert!(new_snapshot("".into(), None).is_none());
        assert!(
            new_snapshot(
                "x".repeat(amaterasu_module_sdk::foreground_context::MAX_EXECUTABLE_BYTES + 1),
                None
            )
            .is_none()
        );
        assert!(new_snapshot("bad\nname".into(), None).is_none());
        assert!(new_snapshot("/usr/bin/game".into(), Some(true)).is_some());
    }
}
