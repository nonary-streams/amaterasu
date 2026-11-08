//! Bounded caller-owned UDP transport through borrowed native media admission.
mod controller;
mod datagrams;
mod model;
mod native;
mod opus_packet;
mod stream;

use amaterasu_module_sdk::{
    Command, LogLevel, ManagedModule, ModuleResult, Response, RuntimeClient, network::Network,
};
use controller::{ControlReply, Controller};
use model::{Operation, Settings};
use serde_json::json;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.media-transport/1";

struct MediaTransportModule {
    runtime: RuntimeClient,
    controller: native::Shared,
    latency_window_started: Instant,
    _factory: Box<native::Factory>,
    shutdown_announced: bool,
}

impl ManagedModule for MediaTransportModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        Self::start_configured(runtime, None)
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_announced = true;
        if let Ok(mut controller) = self.controller.try_lock() {
            controller.prepare_shutdown();
        }
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        // The host starts provider quiescence only after its consumers drain.
        // No transport workers outlive this bounded queue/socket destruction.
        self.shutdown_announced = true;
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        controller.shutdown();
        Ok(true)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let _priority = amaterasu_module_sdk::threading::ScopedThreadPriority::best_effort(
            amaterasu_module_sdk::threading::ThreadPriority::High,
        );
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.latency_window_started);
        let due = elapsed >= Duration::from_secs(10);
        let summaries = {
            let mut controller = match self.controller.try_lock() {
                Ok(controller) => controller,
                Err(TryLockError::WouldBlock) => return Ok(()),
                Err(TryLockError::Poisoned(_)) => return Err("transport state unavailable".into()),
            };
            controller.tick();
            if due {
                controller.take_processing_latencies()
            } else {
                Vec::new()
            }
        };
        if due {
            self.latency_window_started = now;
            for (stream, sample) in summaries {
                let message = format!(
                    "stream={stream} window_ms={} samples={} known_samples={} unknown_samples={} total_tenths_ms={} max_tenths_ms={} zero_known={} histogram={} flush_total_us={} flush_max_us={} flush_deferred_datagrams={} scope=capture_start_to_media_admission_not_delivery",
                    elapsed.as_millis(),
                    sample.samples,
                    sample.samples.saturating_sub(sample.unknown_samples),
                    sample.unknown_samples,
                    sample.total_tenths_ms,
                    sample.max_tenths_ms,
                    sample.zero_samples,
                    sample.histogram(),
                    sample.flush_total_us,
                    sample.flush_max_us,
                    sample.flush_deferred_datagrams
                );
                let _ = self
                    .runtime
                    .log(LogLevel::Info, "media.processing_latency", &message);
            }
        }
        // Acquire before polling so a contended native call cannot strand an
        // already-dequeued request or block the host's cooperative tick.
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err("transport state unavailable".into()),
        };
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
                let result = if service == SERVICE {
                    serde_json::from_value::<Operation>(payload)
                        .map_err(|_| "invalid_request")
                        .and_then(|operation| {
                            if self.shutdown_announced && matches!(operation, Operation::Open(_)) {
                                return Err("shutting_down");
                            }
                            controller.handle(&caller, operation)
                        })
                } else {
                    Err("invalid_service")
                };
                let ControlReply { payload, created } =
                    result.unwrap_or_else(|code| ControlReply {
                        payload: json!({"ok":false,"code":code}),
                        created: None,
                    });
                if !matches!(
                    self.runtime.send(Command::Complete {
                        request: id,
                        payload
                    }),
                    Ok(Response::Ok)
                ) && let Some(stream) = created
                {
                    // A lost/cancelled open response cannot leave a hidden UDP sender.
                    let _ = controller.close(&caller, stream);
                }
                Ok(())
            }
            _ => Err("media transport poll failed".into()),
        }
    }
}

impl MediaTransportModule {
    fn start_configured(
        runtime: RuntimeClient,
        selected_network: Option<Network>,
    ) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("media transport settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload.clone())
            .map_err(|_| "invalid media transport settings")?;
        settings.validate()?;
        let network = match selected_network {
            Some(network) => network,
            None => Network::from_settings(&runtime, &payload)?,
        };
        let controller = Arc::new(Mutex::new(Controller::with_network(settings, network)));
        let factory = native::Factory::new(controller.clone());
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        // SAFETY: boxed factory and synchronized controller live until unload.
        // Signed dependency ordering requires consumers to release all native
        // bindings before provider unload; grants are not a native-code sandbox.
        unsafe {
            amaterasu_module_sdk::transport::register_media(&runtime, SERVICE, &factory.api)?;
        }
        Ok(Self {
            runtime,
            controller,
            latency_window_started: Instant::now(),
            _factory: factory,
            shutdown_announced: false,
        })
    }
}

impl Drop for MediaTransportModule {
    fn drop(&mut self) {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown();
    }
}

amaterasu_module_sdk::export_module!(MediaTransportModule);

#[cfg(test)]
mod module_tests;
