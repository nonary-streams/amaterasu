//! Synthetic integration source with optional paced, decodable H.264 video.
mod controller;
mod native;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use controller::{Controller, Kind};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};

const METRICS: &str = "test.stream-source/1";

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    desktop_geometry: bool,
    enable_video: bool,
    sustain_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            desktop_geometry: false,
            enable_video: true,
            sustain_ms: 0,
        }
    }
}

impl Settings {
    fn source_kinds(&self) -> Vec<Kind> {
        if self.enable_video {
            vec![Kind::Video, Kind::Audio]
        } else {
            vec![Kind::Audio]
        }
    }
}

struct StreamSource {
    runtime: RuntimeClient,
    state: native::Shared,
    // Native factory contexts point into their boxes and must keep stable
    // addresses when this collection or the owning module moves.
    #[allow(clippy::vec_box)]
    _factories: Vec<Box<native::Factory>>,
}

impl ManagedModule for StreamSource {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("stream source settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload)
            .map_err(|error| format!("invalid stream source settings: {error}"))?;
        if settings.sustain_ms > 300_000 {
            return Err("fixture sustain_ms exceeds five minutes".into());
        }
        let state = Arc::new(Mutex::new(Controller::with_duration(
            settings.desktop_geometry,
            settings.sustain_ms,
        )));
        let kinds = settings.source_kinds();
        let factories: Vec<_> = kinds
            .iter()
            .map(|kind| native::Factory::new(state.clone(), *kind))
            .collect();
        for (kind, factory) in kinds.into_iter().zip(&factories) {
            runtime.require_success(Command::Register {
                service: kind.service().into(),
            })?;
            // SAFETY: stable boxed factories live until module unload, after all
            // dependent consumers release their native bindings.
            unsafe {
                match kind {
                    Kind::Video => amaterasu_module_sdk::video::register(
                        &runtime,
                        kind.service(),
                        &factory.api,
                    )?,
                    Kind::Audio => amaterasu_module_sdk::audio::register_capture(
                        &runtime,
                        kind.service(),
                        &factory.api,
                    )?,
                }
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

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn existing_startup_settings_register_both_source_interfaces() {
        for value in [json!({}), json!({"desktop_geometry": true})] {
            let settings: Settings = serde_json::from_value(value).unwrap();
            let kinds = settings.source_kinds();
            assert_eq!(kinds.len(), 2);
            assert!(kinds[0] == Kind::Video && kinds[1] == Kind::Audio);
        }
    }

    #[test]
    fn explicit_audio_only_startup_has_no_video_registration() {
        for sustain_ms in [0, 60_000] {
            let settings: Settings = serde_json::from_value(json!({
                "enable_video": false,
                "sustain_ms": sustain_ms,
            }))
            .unwrap();
            let kinds = settings.source_kinds();
            assert_eq!(kinds.len(), 1);
            assert!(kinds[0] == Kind::Audio);
            assert_eq!(settings.sustain_ms, sustain_ms);
        }
        assert!(serde_json::from_value::<Settings>(json!({"enable_video": "false"})).is_err());
    }
}
