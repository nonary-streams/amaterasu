//! Separate fake protocol provider with the identical approved API contract.
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
struct OtherProvider {
    client: RuntimeClient,
}
impl ManagedModule for OtherProvider {
    fn start(client: RuntimeClient) -> ModuleResult<Self> {
        client.require_success(Command::RegisterApi {
            method: method("receive"),
        })?;
        client.require_success(Command::RegisterApi {
            method: method("cleanup"),
        })?;
        Ok(Self { client })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.client.poll_serialized()? {
            self.client
                .complete_serialized(request.id, &json!({"provider":"b"}))?;
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(OtherProvider);
