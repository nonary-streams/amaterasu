//! Synthetic integration source. The H.264-shaped bytes are not decodable video.
mod controller;
mod native;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::{Controller, Kind};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};

const METRICS: &str = "test.stream-source/1";

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    desktop_geometry: bool,
}

struct StreamSource {
    runtime: RuntimeClient,
    state: native::Shared,
    _factories: [Box<native::Factory>; 2],
}

impl ManagedModule for StreamSource {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("stream source settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid stream source settings: {error}"))?;
        let state = Arc::new(Mutex::new(Controller::new(settings.desktop_geometry)));
        let factories =
            [Kind::Video, Kind::Audio].map(|kind| native::Factory::new(state.clone(), kind));
        for (kind, factory) in [Kind::Video, Kind::Audio].into_iter().zip(&factories) {
            runtime.require_success(Command::Register {
                service: kind.service().into(),
            })?;
            // SAFETY: stable boxed factories live until module unload, after all
            // dependent consumers release their native bindings.
            unsafe {
                runtime.register_native(kind.service(), &factory.api)?;
            }
        }
        runtime.require_success(Command::Register {
            service: METRICS.into(),
        })?;
        Ok(Self {
            runtime,
            state,
            _factories: factories,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let Response::Request {
            id,
            caller,
            service,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        else {
            return Ok(());
        };
        if !matches!(
            self.runtime.send(Command::CheckRequest { request: id })?,
            Response::Ok
        ) {
            return Ok(());
        }
        let reply = self
            .state
            .lock()
            .map_err(|_| "fixture lock poisoned")?
            .handle(&caller, &service, payload);
        let (payload, created) =
            reply.unwrap_or_else(|code| (json!({"ok": false, "code": code}), None));
        let completed = self.runtime.send(Command::Complete {
            request: id,
            payload,
        });
        if !matches!(completed, Ok(Response::Ok))
            && let Some((kind, stream)) = created
        {
            let mut controller = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = controller.close(&caller, kind, stream);
        }
        Ok(())
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown();
    }
}

amaterasu_module_sdk::export_module!(StreamSource);
