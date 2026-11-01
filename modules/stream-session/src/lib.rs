#![recursion_limit = "256"]
//! Streaming behavior belongs to a managed library, not the native shell.
mod model;
mod pipeline;
mod plane;
mod providers;

use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RetirableModule, RetirementProgress,
    RuntimeClient,
};
use model::Settings;
use pipeline::Pipeline;

const SERVICE: &str = "amaterasu.stream-session/1";
struct StreamSession {
    runtime: RuntimeClient,
    pipeline: Pipeline,
    retiring: bool,
    retirement_complete: bool,
}
impl ManagedModule for StreamSession {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("stream session settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|_| "invalid stream session settings")?;
        settings.validate()?;
        let pipeline = Pipeline::new(settings)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pipeline,
            retiring: false,
            retirement_complete: false,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if self.retiring {
            return Err("stream session is retiring".into());
        }
        self.pipeline.tick(&self.runtime)
    }
}
impl Drop for StreamSession {
    fn drop(&mut self) {
        if !self.retirement_complete {
            let _ = self.pipeline.shutdown(&self.runtime);
        }
    }
}

impl RetirableModule for StreamSession {
    fn poll_retirement(&mut self) -> ModuleResult<RetirementProgress> {
        self.retiring = true;
        if !self.retirement_complete {
            self.retirement_complete = self.pipeline.shutdown(&self.runtime);
        }
        Ok(if self.retirement_complete {
            RetirementProgress::Complete
        } else {
            RetirementProgress::Pending
        })
    }
}

amaterasu_module_sdk::export_retirable_module!(StreamSession);

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use amaterasu_module_sdk::abi::{
        AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK, Status,
    };
    use std::ffi::c_void;

    unsafe extern "C" fn invoke(_: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> Status {
        // SAFETY: the SDK provides readable bytes for this synchronous call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return STATUS_INTERNAL_ERROR;
        };
        let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
            return STATUS_INTERNAL_ERROR;
        };
        let response = match command {
            Command::GetSettings {} => Response::Result {
                payload: serde_json::json!({}),
            },
            Command::Register { .. } => Response::Ok,
            _ => Response::Pending,
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        // SAFETY: reply is borrowed for the duration of this call.
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }

    #[test]
    fn retirement_closes_tick_and_remains_complete_after_shutdown() {
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke,
        };
        // SAFETY: the stateless fixture callback is valid for the test lifetime.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = StreamSession::start(runtime).unwrap();
        assert_eq!(
            module.poll_retirement().unwrap(),
            RetirementProgress::Complete
        );
        assert!(module.tick().is_err());
        assert_eq!(
            module.poll_retirement().unwrap(),
            RetirementProgress::Complete
        );
    }
}
