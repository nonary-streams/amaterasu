use crate::lease::{LeaseManager, handle_runtime_operation};
use crate::rtss::RtssProvider;
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::path::PathBuf;

const SERVICE: &str = "amaterasu.frame-limiter/1";

/// Registers the policy and verified runtime service for frame-limiter requests.
pub struct FrameLimiterModule {
    runtime: RuntimeClient,
    leases: LeaseManager<RtssProvider>,
}

impl ManagedModule for FrameLimiterModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let journal_path = default_journal_path()?;
        let mut leases = LeaseManager::open(RtssProvider::new(), journal_path)
            .map_err(|error| format!("frame-limiter lease journal failed: {error}"))?;
        leases
            .recover()
            .map_err(|error| format!("frame-limiter lease recovery failed: {error}"))?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, leases })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request {
                id,
                caller,
                service,
                payload,
                ..
            } => {
                if !self.request_is_active(id)? {
                    return Ok(());
                }

                let response = if service != SERVICE {
                    json!({
                        "ok": false,
                        "applied": false,
                        "error": {
                            "code": "invalid_service",
                            "message": "unsupported frame-limiter service"
                        }
                    })
                } else {
                    handle_runtime_operation(&caller, payload, &mut self.leases)
                };

                self.complete(id, response)
            }
            response => Err(format!("frame-limiter poll failed: {response:?}")),
        }
    }
}

impl FrameLimiterModule {
    fn request_is_active(&self, id: u64) -> ModuleResult<bool> {
        match self.runtime.send(Command::CheckRequest { request: id })? {
            Response::Ok => Ok(true),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(false)
            }
            response => Err(format!("frame-limiter request check failed: {response:?}")),
        }
    }

    fn complete(&self, id: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete {
            request: id,
            payload,
        })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!("frame-limiter completion failed: {response:?}")),
        }
    }
}

fn default_journal_path() -> ModuleResult<PathBuf> {
    #[cfg(target_os = "windows")]
    let data_root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .ok_or_else(|| "user data directory is unavailable".to_owned())?;

    #[cfg(target_os = "macos")]
    let data_root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "user data directory is unavailable".to_owned())?
        .join("Library")
        .join("Application Support");

    #[cfg(target_os = "linux")]
    let data_root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local").join("share"))
        })
        .ok_or_else(|| "user data directory is unavailable".to_owned())?;

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    return Err("user data directory is unsupported on this platform".into());

    if !data_root.is_absolute() {
        return Err("user data directory must be absolute".into());
    }
    Ok(data_root
        .join("Amaterasu")
        .join("frame-limiter")
        .join("leases.json"))
}
