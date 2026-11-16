use amaterasu_module_sdk::{
    Command, Response,
    drivers::{INSTALLATION_SERVICE, installation_methods},
};
use amaterasu_module_testkit::{ApiMethodApproval, TestHost, grants};
use serde_json::json;

#[test]
fn installation_contract_routes_an_unchanged_consumer_to_either_approved_provider() {
    for provider in ["example.bundled-installer", "example.replacement-installer"] {
        let mut builder = TestHost::builder()
            .principal(provider, grants().provide(INSTALLATION_SERVICE))
            .principal("amaterasu.management", grants().call(INSTALLATION_SERVICE))
            .principal("example.unapproved", grants().call(INSTALLATION_SERVICE));
        for method in installation_methods() {
            builder = builder.api_method(ApiMethodApproval {
                provider: provider.into(),
                callers: ["amaterasu.management".into()].into(),
                method,
            });
        }
        let host = builder.build();
        let owner = host.client(provider);
        for method in installation_methods() {
            owner
                .require_success(Command::RegisterApi { method })
                .unwrap();
        }
        let consumer = host.client("amaterasu.management");
        let request = Command::CallApi {
            service: INSTALLATION_SERVICE.into(),
            method: "status".into(),
            interface_major: 1,
            interface_minor: 0,
            payload: json!({}),
            timeout_ms: 1000,
            operation_id: "driver-status-1".into(),
            parent: None,
        };
        let Response::Accepted { id } = consumer.send(request.clone()).unwrap() else {
            panic!("approved provider must admit status");
        };
        let Response::Request {
            id: delivered,
            caller,
            payload,
            ..
        } = owner.send(Command::Poll {}).unwrap()
        else {
            panic!("selected provider must receive the request");
        };
        assert_eq!(caller, "amaterasu.management");
        assert_eq!(payload["method"], "status");
        owner
            .require_success(Command::Complete {
                request: delivered,
                payload: json!({"ok":true,"supported":false}),
            })
            .unwrap();
        assert!(
            matches!(consumer.send(Command::TakeResult {request:id}).unwrap(),Response::Result {payload} if payload["supported"] == false)
        );
        assert!(
            matches!(host.client("example.unapproved").send(request).unwrap(),Response::Error {code,..} if code == "permission_denied")
        );
    }
}
