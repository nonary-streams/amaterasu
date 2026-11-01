//! Device-free lifecycle participant for the loopback composition test.
use amaterasu_module_sdk::{Command, RuntimeClient};
use serde_json::{Value, json};

const SERVICE: &str = "fixture.capture/1";
const OWNER: &str = "fixture-capture";
const COORDINATOR: &str = "amaterasu.session-lifecycle";

fn reference(name: &str) -> Value {
    json!({"service":SERVICE,"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1})
}
fn method(name: &str) -> Value {
    json!({"service":SERVICE,"method":name,"interface_major":1,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"})
}
pub(super) fn configure(config: &mut Value) {
    config["settings"][COORDINATOR]["consumer_requirements"]["capture_providers"] =
        json!(["amaterasu.capture-pipewire/1"]);
    config["settings"][COORDINATOR]["consumer_requirements"]["selector_schemas"] =
        json!(["amaterasu.kwin-wl-output/1"]);
    config["settings"][COORDINATOR]["trusted_callers"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.rtsp"));
    config["settings"][COORDINATOR]["session_delegates"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.rtsp"));
    config["settings"][COORDINATOR]["operation_grants"]["amaterasu.rtsp"] =
        json!(["prepare_stream", "lookup", "pause_stream"]);
    config["principals"][OWNER] = json!({"provide":[SERVICE]});
    config["principals"][COORDINATOR]["call"]
        .as_array_mut()
        .unwrap()
        .push(json!(SERVICE));
    config["settings"][COORDINATOR]["participants"] = json!([{"provider":OWNER,"plan_method":reference("plan"),"callback_method":reference("callback"),"status_method":reference("status"),"binding_validate_method":reference("binding_validate"),"capabilities":[],"cleanup_methods":[reference("callback")],"settings_revision":"one","settings":{}}]);
    config["api_methods"] = json!(
        ["plan", "callback", "status", "binding_validate"]
            .iter()
            .map(|name| json!({"provider":OWNER,"callers":[COORDINATOR],"method":method(name)}))
            .collect::<Vec<_>>()
    );
}
pub(super) fn register(client: &RuntimeClient) {
    for name in ["plan", "callback", "status", "binding_validate"] {
        client
            .require_success(Command::RegisterApi {
                method: serde_json::from_value(method(name)).unwrap(),
            })
            .unwrap();
    }
}
pub(super) fn pump(client: &RuntimeClient) {
    while let Some(request) = client.poll_serialized().unwrap() {
        let args = &request.payload["arguments"];
        if request.payload["method"] == "binding_validate" {
            let response = json!({"status":"succeeded","target":args["target"],"binding":args["binding"],"observation":args["observation"],"plan_revision":args["plan_revision"]});
            client
                .require_success(Command::Complete {
                    request: request.id,
                    payload: response,
                })
                .unwrap();
            continue;
        }
        let stream = args["phase"] == "stream.prepare";
        let resource = json!({"provider":OWNER,"resource_id":"output","incarnation":"fixture","expected_revision":1,"execution_domain":"seat","mutation_domains":[],"access":"read"});
        let constraint = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","resource_provider":OWNER,"resource_id":"output","incarnation":"fixture","execution_domain":"seat","mode":args["requested_video"],"plan_revision":1,"geometry_revision":1});
        let candidate = json!({"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-Fixture"},"resource":{"provider":OWNER,"resource_id":"output","incarnation":"fixture","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"mode":args["requested_video"],"plan_revision":1,"geometry_revision":1,"generation":args["target"]["generation"]});
        let response = match request.payload["method"].as_str().unwrap() {
            "plan" => {
                json!({"status":"planned","plan":{"plan_id":"fixture-plan","revision":1,"resources":if stream {json!([resource])} else {json!([])},"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":if stream {constraint} else {Value::Null},"allowed_degradation":[]}})
            }
            "callback" if args["phase"].as_str().unwrap().ends_with("release") => {
                json!({"status":"cleanup","outcomes":[]})
            }
            "callback" => {
                json!({"status":"prepared","candidate":if stream {candidate} else {Value::Null},"obligations":[]})
            }
            other => panic!("unexpected lifecycle method {other}: {}", request.payload),
        };
        client
            .require_success(Command::Complete {
                request: request.id,
                payload: response,
            })
            .unwrap();
    }
}

pub(super) fn configure_input(config: &mut Value) {
    let lifecycle = &mut config["settings"][COORDINATOR];
    lifecycle["operation_grants"]["amaterasu.nvhttp"]
        .as_array_mut()
        .unwrap()
        .push(json!("resolve_effect_sources"));
    lifecycle["trusted_callers"]
        .as_array_mut()
        .unwrap()
        .extend([json!("stream"), json!("input")]);
    lifecycle["session_delegates"]
        .as_array_mut()
        .unwrap()
        .push(json!("stream"));
    lifecycle["consumers"] = json!({"capture":["stream"],"input":["stream","input"]});
    lifecycle["input_provider"] = json!("input");
    lifecycle["operation_grants"]["stream"] = json!([
        "validate_candidate",
        "ack_binding",
        "start_stream",
        "pause_stream",
        "retire_stream",
        "issue_input_binding",
        "binding_validate",
        "capture_invalidated",
        "lookup"
    ]);
    lifecycle["operation_grants"]["input"] = json!(["validate_input_binding"]);
    config["principals"]["input"]["call"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.session-lifecycle/1"));
    config["principals"][COORDINATOR]["call"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.input/1"));
    config["settings"]["stream"]["enable_input"] = json!(true);
    config["settings"]["stream"]["observe_feedback_sources"] = json!(true);
    config["settings"]["input"]["feedback_owner"] = json!("stream");
}
