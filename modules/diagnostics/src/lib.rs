//! Caller-scoped diagnostics with bounded, in-memory retention and no automatic collection.
mod host_logging;
mod journal;
mod logging;
mod protocol;
pub use journal::Journal;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use host_logging::HostLoggingConfigurationReader;
use std::path::PathBuf;
use std::time::Instant;

pub const SERVICE: &str = "amaterasu.diagnostics/1";

pub struct DiagnosticsModule {
    runtime: RuntimeClient,
    journal: Journal,
    host_logging: HostLoggingConfigurationReader,
    logging_state_root: Option<PathBuf>,
}

impl ManagedModule for DiagnosticsModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("diagnostics settings request did not return a result".into());
        };
        let logging_settings = logging::LoggingSettings::from_value(
            payload
                .get("host_logging")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )?;
        let logging_state_root = logging_settings.state_root.clone();
        let (journal, logging_warning) = Journal::with_logging(logging_settings);
        if let Some(warning) = logging_warning {
            eprintln!("amaterasu diagnostics: file logging unavailable: {warning}");
        }
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            journal,
            host_logging: HostLoggingConfigurationReader::default(),
            logging_state_root,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(configuration) = self
            .host_logging
            .tick(Instant::now(), |command| self.runtime.send(command))
        {
            match logging::LoggingSettings::from_host_configuration(
                &configuration,
                self.logging_state_root.as_deref(),
            ) {
                Ok(settings) => {
                    if let Some(warning) = self.journal.configure_logging(settings) {
                        eprintln!(
                            "amaterasu diagnostics: persisted file logging unavailable: {warning}"
                        );
                    }
                }
                Err(warning) => {
                    eprintln!("amaterasu diagnostics: persisted logging policy ignored: {warning}");
                }
            }
        }

        match self.runtime.send(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let payload = self.journal.handle(&caller, payload);
                // A cancellation after dispatch does not undo an accepted record/counter.
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload,
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!("unexpected diagnostics completion: {response:?}")),
                }
            }
            Response::Pending => Ok(()),
            response => Err(format!("unexpected diagnostics poll: {response:?}")),
        }
    }
}

amaterasu_module_sdk::export_module!(DiagnosticsModule);
