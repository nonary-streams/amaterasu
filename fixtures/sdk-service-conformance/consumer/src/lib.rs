//! Small real module whose State requests use the public SDK adapter.
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    state::{self, StateDeletion, StateLookup, StateMutation, StateRequest},
};
use serde_json::{Value, json};

const SERVICE: &str = "example.state-consumer/1";
struct Pending {
    parent: u64,
    child: u64,
    kind: &'static str,
}
struct Consumer {
    runtime: RuntimeClient,
    pending: Option<Pending>,
}

impl ManagedModule for Consumer {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            pending: None,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(pending) = self.pending.take() {
            match self.runtime.send(Command::TakeResult {
                request: pending.child,
            })? {
                Response::Pending => self.pending = Some(pending),
                Response::Result { payload } => {
                    let valid = match pending.kind {
                        "get" => state::decode_response::<StateLookup>(payload.clone()).is_ok(),
                        "set" => state::decode_response::<StateMutation>(payload.clone()).is_ok(),
                        _ => state::decode_response::<StateDeletion>(payload.clone()).is_ok(),
                    };
                    let response = if valid {
                        payload
                    } else {
                        json!({"fixture_error":"invalid State response"})
                    };
                    let completion = self.runtime.send(Command::Complete {
                        request: pending.parent,
                        payload: response,
                    })?;
                    if !matches!(completion, Response::Ok | Response::Error { .. }) {
                        return Err(format!("unexpected completion: {completion:?}"));
                    }
                    self.runtime.require_success(Command::ReleaseResult {
                        request: pending.child,
                    })?;
                }
                Response::Error { code, .. }
                    if code == "cancelled" || code == "deadline_exceeded" =>
                {
                    self.runtime.require_success(Command::ReleaseResult {
                        request: pending.child,
                    })?;
                }
                Response::Error { code, .. } if code == "not_found" || code == "request_ended" => {}
                other => return Err(format!("unexpected state result: {other:?}")),
            }
            return Ok(());
        }
        if let Some(request) = self.runtime.poll_serialized()? {
            let payload: Value = request.payload;
            let parsed = serde_json::from_value::<StateRequest>(payload);
            let (operation, kind) = match parsed {
                Ok(operation) => {
                    let kind = match &operation {
                        StateRequest::Get { .. } => "get",
                        StateRequest::Set { .. } => "set",
                        StateRequest::Delete { .. } => "delete",
                    };
                    (operation, kind)
                }
                Err(error) => {
                    self.runtime.complete_serialized(
                        request.id,
                        &json!({"fixture_error":error.to_string()}),
                    )?;
                    return Ok(());
                }
            };
            match operation.submit(
                &self.runtime,
                5_000,
                &format!("state.{}", request.id),
                Some(request.id),
            ) {
                Ok(child) => {
                    self.pending = Some(Pending {
                        parent: request.id,
                        child,
                        kind,
                    })
                }
                Err(error) => self
                    .runtime
                    .complete_serialized(request.id, &json!({"fixture_error":error}))?,
            }
        }
        Ok(())
    }
}
impl Drop for Consumer {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            let _ = self.runtime.send(Command::Cancel {
                request: pending.child,
            });
            let _ = self.runtime.send(Command::ReleaseResult {
                request: pending.child,
            });
        }
    }
}
amaterasu_module_sdk::export_module!(Consumer);
