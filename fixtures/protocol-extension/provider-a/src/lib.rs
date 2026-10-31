//! Independently built fake protocol provider; creates no device or network.
use amaterasu_contracts::{ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, RuntimeClient};
use serde_json::json;
fn method(name: &str) -> ApiMethod {
    ApiMethod {
        service: "example.protocol/1".into(),
        method: name.into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [
                ("context".into(), ApiFieldKind::Object),
                ("capability".into(), ApiFieldKind::String),
                ("message_type".into(), ApiFieldKind::Integer),
                ("payload_base64".into(), ApiFieldKind::String),
            ]
            .into(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("provider".into(), ApiFieldKind::String)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    }
}
struct Provider {
    runtime: RuntimeClient,
}
impl ManagedModule for Provider {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        for name in ["receive", "cleanup"] {
            runtime.require_success(Command::RegisterApi {
                method: method(name),
            })?;
        }
        Ok(Self { runtime })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            self.runtime
                .complete_serialized(request.id, &json!({"provider":"a"}))?;
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Provider);
