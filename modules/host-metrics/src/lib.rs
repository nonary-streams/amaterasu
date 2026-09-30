//! Demand-driven host telemetry and its revisioned management preferences.
mod collector;
mod model;
mod product_settings;
mod provider;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use collector::Collector;
use model::Settings;
use product_settings::{Completion, ProductSettings};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.host-metrics/1";
const MANAGEMENT: &str = "amaterasu.management";
const STREAM_SESSION: &str = "amaterasu.stream-session";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    SettingsGet {},
    SettingsUpdate {
        settings: Settings,
        expected_revision: u64,
    },
    StatsRead {},
    StreamActivityBegin {
        session_id: String,
    },
    StreamActivityEnd {
        session_id: String,
    },
}

struct HostMetricsModule {
    runtime: RuntimeClient,
    product_settings: ProductSettings,
    collector: Collector,
    deferred_settings: VecDeque<(u64, Operation)>,
}

impl ManagedModule for HostMetricsModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let startup_id = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let settings = Settings::default();
        Ok(Self {
            runtime,
            product_settings: ProductSettings::new(&startup_id),
            collector: Collector::new(settings),
            deferred_settings: VecDeque::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Response::Request {
            id,
            caller,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        {
            match serde_json::from_value::<Operation>(payload) {
                Ok(operation) => self.handle_request(id, &caller, operation)?,
                Err(_) => self.complete(
                    id,
                    failure(
                        "invalid_request",
                        "request does not match the host metrics schema",
                    ),
                )?,
            }
        }

        let runtime = &mut self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command))?;
        while let Some(completion) = self.product_settings.take_completion() {
            match completion {
                Completion::Update { request, response } => {
                    if response.get("ok").and_then(Value::as_bool) == Some(true) {
                        let (settings, _) = self.product_settings.settings();
                        self.collector
                            .apply_settings(settings.clone(), Instant::now());
                    }
                    self.complete(request, response)?;
                }
            }
        }

        if self.product_settings.is_ready() {
            let (settings, _) = self.product_settings.settings();
            if self.collector.settings() != settings {
                self.collector
                    .apply_settings(settings.clone(), Instant::now());
            }
        }
        self.collector.tick(Instant::now());
        self.drain_deferred_settings()?;
        Ok(())
    }
}

impl HostMetricsModule {
    fn handle_request(&mut self, id: u64, caller: &str, operation: Operation) -> ModuleResult<()> {
        match operation {
            Operation::SettingsGet {} if caller == MANAGEMENT => {
                if !self.product_settings.is_ready() {
                    if self.deferred_settings.len() >= 32 {
                        return self.complete(
                            id,
                            failure(
                                "settings_busy",
                                "too many host metrics settings reads are queued",
                            ),
                        );
                    }
                    self.deferred_settings
                        .push_back((id, Operation::SettingsGet {}));
                    return Ok(());
                }
                let (settings, revision) = self.product_settings.settings();
                self.complete(
                    id,
                    json!({"ok":true,"settings":settings,"revision":revision}),
                )
            }
            Operation::SettingsGet {} => self.complete(
                id,
                failure(
                    "permission_denied",
                    "caller is not authorized for host metrics settings",
                ),
            ),
            Operation::SettingsUpdate {
                settings,
                expected_revision,
            } if caller == MANAGEMENT => {
                match self
                    .product_settings
                    .begin_update(id, expected_revision, settings)
                {
                    Ok(()) => Ok(()),
                    Err(response) => self.complete(id, response),
                }
            }
            Operation::SettingsUpdate { .. } => self.complete(
                id,
                failure(
                    "permission_denied",
                    "caller is not authorized for host metrics settings",
                ),
            ),
            Operation::StatsRead {} if caller == MANAGEMENT => {
                let (available, sampled_at_unix, host, samples) =
                    self.collector.read(Instant::now());
                self.complete(
                    id,
                    json!({
                        "ok":true,
                        "available":available,
                        "platform":self.collector.platform(),
                        "sampled_at_unix_ms":sampled_at_unix,
                        "host":host,
                        "samples":samples
                    }),
                )
            }
            Operation::StatsRead {} => self.complete(
                id,
                failure(
                    "permission_denied",
                    "caller is not authorized to read host metrics",
                ),
            ),
            Operation::StreamActivityBegin { session_id } if caller == STREAM_SESSION => {
                match self.collector.stream_activity_begin(&session_id) {
                    Ok(()) => self.complete(id, json!({"ok":true,"active":true})),
                    Err(message) => self.complete(id, failure("invalid_request", message)),
                }
            }
            Operation::StreamActivityBegin { .. } => self.complete(
                id,
                failure(
                    "permission_denied",
                    "caller is not authorized for stream telemetry",
                ),
            ),
            Operation::StreamActivityEnd { session_id } if caller == STREAM_SESSION => {
                match self.collector.stream_activity_end(&session_id) {
                    Ok(()) => self.complete(id, json!({"ok":true,"active":false})),
                    Err(message) => self.complete(id, failure("invalid_request", message)),
                }
            }
            Operation::StreamActivityEnd { .. } => self.complete(
                id,
                failure(
                    "permission_denied",
                    "caller is not authorized for stream telemetry",
                ),
            ),
        }
    }

    fn drain_deferred_settings(&mut self) -> ModuleResult<()> {
        if !self.product_settings.is_ready() {
            return Ok(());
        }
        while let Some((request, operation)) = self.deferred_settings.pop_front() {
            self.handle_request(request, MANAGEMENT, operation)?;
        }
        Ok(())
    }

    fn complete(&mut self, request: u64, payload: Value) -> ModuleResult<()> {
        self.runtime
            .require_success(Command::Complete { request, payload })
    }
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"message":message})
}

amaterasu_module_sdk::export_module!(HostMetricsModule);
