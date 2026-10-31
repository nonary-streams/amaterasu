//! Independent, deliberately non-decodable encoder provider for integration tests.
mod consumer;
mod native;
mod source;
mod state;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde_json::json;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "example.encode/1";
const CONSUMER_SERVICE: &str = "example.encode-consumer/1";
const PROVIDER_SERVICE: &str = "example.encode-provider/1";

struct EncoderSource {
    runtime: RuntimeClient,
    shared: native::Shared,
    _registration: Box<native::Registration>,
    _source_registration: Box<source::Registration>,
    _consumer_registration: Box<consumer::Registration>,
}

impl ManagedModule for EncoderSource {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("encoder fixture settings unavailable".into());
        };
        if payload != json!({}) {
            return Err("encoder fixture accepts only empty settings".into());
        }
        let shared = Arc::new(Mutex::new(state::State::default()));
        let registration = Box::new(native::Registration::new(shared.clone()));
        let source_registration = Box::new(source::Registration::new(shared.clone()));
        let consumer_registration = Box::new(consumer::Registration::new(shared.clone()));
        for service in [SERVICE, PROVIDER_SERVICE, CONSUMER_SERVICE] {
            runtime.require_success(Command::Register {
                service: service.into(),
            })?;
        }
        // SAFETY: registration and its factory context remain alive for the module lifetime.
        unsafe {
            runtime.register_native(SERVICE, &registration.factory)?;
            runtime.register_native(PROVIDER_SERVICE, &source_registration.factory)?;
            runtime.register_native(CONSUMER_SERVICE, &consumer_registration.factory)?;
        }
        Ok(Self {
            runtime,
            shared,
            _registration: registration,
            _source_registration: source_registration,
            _consumer_registration: consumer_registration,
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
        let (reply, created) = if service == SERVICE {
            self.shared
                .lock()
                .map_err(|_| "encoder fixture lock poisoned")?
                .control(&caller, payload)
        } else {
            (json!({"ok":false,"code":"invalid_service"}), None)
        };
        if !matches!(
            self.runtime.send(Command::Complete {
                request: id,
                payload: reply
            }),
            Ok(Response::Ok)
        ) && let Some(session) = created
        {
            let _ = self
                .shared
                .lock()
                .map(|mut state| state.close(&caller, session));
        }
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(EncoderSource);
