//! Read-only integration fixture for a third-party registered API provider.
use amaterasu_contracts::{ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema};
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};
use serde_json::{Value, json};

fn method() -> ApiMethod {
    ApiMethod {
        service: "example.extension/1".into(),
        method: "greet".into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: [("name".into(), ApiFieldKind::String)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: [("message".into(), ApiFieldKind::String)].into(),
            optional: Default::default(),
            allow_extra: false,
        },
        privilege: ApiPrivilege::User,
    }
}

fn greet(payload: &Value) -> Value {
    let name = payload
        .get("arguments")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str);
    if payload.get("method").and_then(Value::as_str) != Some("greet")
        || payload.get("interface_major").and_then(Value::as_u64) != Some(1)
        || payload.get("interface_minor").and_then(Value::as_u64) != Some(0)
        || name.is_none()
    {
        return json!({"message": "Invalid request"});
    }
    // Bound the output independently of the broker's input limit.
    let name: String = name.unwrap_or_default().chars().take(128).collect();
    json!({"message": format!("Hello, {name}!")})
}

struct ExtensionExample {
    runtime: RuntimeClient,
}

impl ManagedModule for ExtensionExample {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.provide_api(method())?;
        Ok(Self { runtime })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.next_request()? {
            request.reply(&self.runtime, &greet(&request.payload))?;
        }
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(ExtensionExample);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_registered_request_and_bounds_output() {
        let request = |name: &str| json!({"method":"greet", "interface_major":1, "interface_minor":0, "arguments":{"name":name}});
        assert_eq!(greet(&request("Ada")), json!({"message":"Hello, Ada!"}));
        assert_eq!(
            greet(&request(&"x".repeat(200)))["message"]
                .as_str()
                .unwrap()
                .len(),
            136
        );
        assert_eq!(
            greet(&json!({"method":"other"})),
            json!({"message":"Invalid request"})
        );
    }
}
