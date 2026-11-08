//! Default independently replaceable Display Settings SDK provider.
mod engine;
#[cfg(target_os = "linux")]
mod native;
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    display_settings::{self, Operation},
    operation_api,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::mpsc::{self, Receiver, SyncSender},
    thread::JoinHandle,
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    enabled: bool,
    registrars: BTreeSet<String>,
    consumers: BTreeSet<String>,
}
struct FileStore {
    path: std::path::PathBuf,
    _lock: amaterasu_storage::RootLock,
}
impl engine::Store for FileStore {
    fn save(&mut self, state: &engine::State) -> engine::Result<()> {
        amaterasu_storage::write_json_atomically(&self.path, state)
    }
}
struct Work {
    request: u64,
    caller: String,
    operation: Operation,
}
struct Provider {
    runtime: RuntimeClient,
    work: Option<SyncSender<Work>>,
    results: Receiver<(u64, Value)>,
    worker: Option<JoinHandle<()>>,
    pending: Option<u64>,
}
impl ManagedModule for Provider {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("display settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload).map_err(|e| e.to_string())?;
        if !settings.enabled
            || settings.registrars.len() > 16
            || settings.consumers.len() > 32
            || settings
                .registrars
                .iter()
                .chain(&settings.consumers)
                .any(|id| !display_settings::valid_id(id))
        {
            return Err("invalid_display_settings_selection".into());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = settings;
            Err("display_settings_backend_unavailable".into())
        }
        #[cfg(target_os = "linux")]
        {
            let directory = runtime.runtime_state_root()?.join("display-settings");
            amaterasu_storage::create_private_directory(&directory)?;
            let lock = amaterasu_storage::RootLock::acquire(&directory)?;
            let path = directory.join("adjustments.json");
            let state: engine::State = if path.exists() {
                amaterasu_storage::read_json(&path)?
            } else {
                Default::default()
            };
            let mut engine = engine::Engine::new(
                native::NativeBackend,
                FileStore { path, _lock: lock },
                state,
                settings.registrars,
                settings.consumers,
            )?;
            let methods = display_settings::methods(display_settings::SERVICE);
            if operation_api::register_selected(&runtime, &methods)? != methods.len() {
                return Err("display_settings_api_not_approved".into());
            }
            let (tx, rx) = mpsc::sync_channel::<Work>(1);
            let (done, results) = mpsc::channel();
            let worker = std::thread::Builder::new()
                .name("display-settings".into())
                .spawn(move || {
                    loop {
                        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                            Ok(work) => {
                                let result = engine
                                    .execute(&work.caller, work.operation)
                                    .unwrap_or_else(|code| json!({"ok":false,"code":code}));
                                if done.send((work.request, result)).is_err() {
                                    break;
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                let _ = engine.reconcile();
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(Self {
                runtime,
                work: Some(tx),
                results,
                worker: Some(worker),
                pending: None,
            })
        }
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Ok((request, payload)) = self.results.try_recv() {
            self.pending = None;
            match self.runtime.send(Command::Complete { request, payload })? {
                Response::Ok => {}
                Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {}
                _ => return Err("display_completion_failed".into()),
            }
        }
        if self.pending.is_some() || self.work.is_none() {
            return Ok(());
        }
        if let Response::Request {
            id,
            caller,
            service,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        {
            let operation = if service == display_settings::SERVICE {
                display_settings::decode(payload)
            } else {
                Err("unknown_service".into())
            };
            match operation {
                Ok(operation)
                    if matches!(
                        self.runtime.send(Command::CheckRequest { request: id })?,
                        Response::Ok
                    ) =>
                {
                    self.work
                        .as_ref()
                        .ok_or("display_worker_closed")?
                        .send(Work {
                            request: id,
                            caller,
                            operation,
                        })
                        .map_err(|_| "display_worker_closed")?;
                    self.pending = Some(id);
                }
                _ => {
                    self.runtime.send(Command::Complete {
                        request: id,
                        payload: json!({"ok":false,"code":"invalid_request"}),
                    })?;
                }
            }
        }
        Ok(())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.work.take();
        self.tick()?;
        Ok(self.pending.is_none() && self.worker.as_ref().is_none_or(|w| w.is_finished()))
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.work.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
amaterasu_module_sdk::export_module!(Provider);
