//! Neutral independently loaded participant and stream-service fixture.
//! It performs no device, application, network, or feature effects.
use amaterasu_module_sdk::{
    ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema, Command, ManagedModule, ModuleResult,
    Response, RuntimeClient,
    extension_api::{SchemaRef, schemas::SchemaCatalog},
    launch_options::{LaunchOption, LaunchOptions},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Fixture {
    runtime: RuntimeClient,
}

fn shape(fields: &[(&str, ApiFieldKind)]) -> ApiSchema {
    ApiSchema {
        required: fields
            .iter()
            .map(|(name, kind)| ((*name).into(), kind.clone()))
            .collect(),
        optional: BTreeMap::new(),
        allow_extra: false,
    }
}

fn method(service: &str) -> ApiMethod {
    ApiMethod {
        service: service.into(),
        method: "project".into(),
        interface_major: 1,
        interface_minor: 0,
        request: shape(&[
            ("version", ApiFieldKind::Integer),
            ("resume", ApiFieldKind::Boolean),
            ("fields", ApiFieldKind::Object),
        ]),
        response: shape(&[
            ("namespace", ApiFieldKind::String),
            ("version", ApiFieldKind::Integer),
            ("value", ApiFieldKind::Object),
        ]),
        privilege: ApiPrivilege::User,
    }
}

// This fixture declares exactly these neutral option schemas; it does not
// claim any production capability or infer support from the request.
fn accepts_options(value: &Value) -> bool {
    const DOCUMENT: &str = r#"{"$id":"org.example.option","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false}"#;
    let schema = SchemaRef {
        id: "org.example.option".into(),
        major: 1,
        minor: 0,
        digest: "e020a99a8ed4ec086459efba9b8f0c73420143d452a894200c54b112560cc84d".into(),
    };
    let Ok(catalog) = SchemaCatalog::compile([(schema.clone(), DOCUMENT)]) else {
        return false;
    };
    let Ok(entries) = serde_json::from_value::<Vec<LaunchOption>>(value.clone()) else {
        return false;
    };
    let Ok(options) = LaunchOptions::validate(entries, |name, version| {
        version == 1 && matches!(name, "org.example.alpha" | "org.example.beta")
    }) else {
        return false;
    };
    options.entries().len() == 2
        && options
            .entries()
            .iter()
            .all(|entry| catalog.validate(&schema, &entry.value).is_ok())
}

fn handle(service: &str, payload: &Value) -> Value {
    if service == "amaterasu.rtsp/1" {
        if payload["action"] == "launch_option_capabilities" {
            let schema = json!({"id":"org.example.option","major":1,"minor":0,"digest":"e020a99a8ed4ec086459efba9b8f0c73420143d452a894200c54b112560cc84d"});
            return json!({"version":1,"options":[{"namespace":"org.example.alpha","version":1,"schema":schema},{"namespace":"org.example.beta","version":1,"schema":schema}]});
        }
        if payload["action"] != "open" || !accepts_options(&payload["launch_options"]) {
            return json!({"ok":false,"code":"invalid_fixture_request"});
        }
        return json!({"ok":true,"ticket":1,"encoder":payload["capture"]["encoder"],"sw_preset":payload["sw_preset"],"sw_tune":payload["sw_tune"],"min_threads":payload["min_threads"],"qp":payload["qp"],"nvenc_settings":payload["nvenc_settings"],"nvenc_spatial_aq":payload["nvenc_spatial_aq"],"rtsp_url":"rtspenc://127.0.0.1:48010","state":"ready","frame_limiter_settings":payload["frame_limiter_settings"],"frame_limiter_executable_path":payload["frame_limiter_executable_path"],"display_policy_snapshot":payload["display_policy_snapshot"],"display_request":payload["display_request"],"failure":null,"cleanup_error":null});
    }
    let (field, namespace) = match service {
        "org.example.alpha/1" => ("alpha", "org.example.alpha"),
        "org.example.beta/1" => ("beta", "org.example.beta"),
        _ => return json!({"invalid":true}),
    };
    let args = &payload["arguments"];
    if payload["method"] != "project"
        || payload["interface_major"] != 1
        || payload["interface_minor"] != 0
        || args["version"] != 1
        || args["resume"] != false
        || args["fields"]
            .as_object()
            .is_none_or(|fields| fields.len() != 1 || !fields.contains_key(field))
    {
        return json!({"invalid":true});
    }
    json!({"namespace":namespace,"version":1,"value":{"enabled":true}})
}

impl ManagedModule for Fixture {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        for service in ["org.example.alpha/1", "org.example.beta/1"] {
            runtime.require_success(Command::RegisterApi {
                method: method(service),
            })?;
        }
        runtime.require_success(Command::Register {
            service: "amaterasu.rtsp/1".into(),
        })?;
        Ok(Self { runtime })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Response::Request {
            id,
            service,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        {
            self.runtime.require_success(Command::Complete {
                request: id,
                payload: handle(&service, &payload),
            })?;
        }
        Ok(())
    }
    fn health(&self) -> ModuleResult<()> {
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(Fixture);
