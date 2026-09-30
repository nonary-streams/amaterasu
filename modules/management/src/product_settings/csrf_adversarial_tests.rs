//! A durable reply may activate only the exact requested revision and origin order.
use super::*;
use std::collections::VecDeque;

const FIRST: &str = "https://first.example";
const SECOND: &str = "https://second.example";

#[derive(Default)]
struct RuntimeReplies {
    replies: VecDeque<Response>,
    commands: Vec<Command>,
}

impl RuntimeReplies {
    fn supply(&mut self, replies: impl IntoIterator<Item = Response>) {
        self.replies.extend(replies);
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        self.commands.push(command);
        Ok(self.replies.pop_front().expect("a reply was supplied"))
    }
}

fn stored(revision: u64, origins: Value) -> Response {
    Response::Result {
        payload: json!({"ok":true,"found":true,"version":2,"revision":revision,
            "values":{"origin_web_ui_allowed":"pc","csrf_allowed_origins":origins}}),
    }
}

#[test]
fn stale_or_reordered_configuration_result_closes_ingress_before_reload() {
    for returned in [
        stored(1, json!([FIRST, SECOND])),
        stored(3, json!([FIRST, SECOND])),
        stored(2, json!([SECOND, FIRST])),
        stored(2, json!([FIRST])),
        stored(2, Value::Null),
    ] {
        let mut settings = ProductSettings::default();
        let mut runtime = RuntimeReplies::default();
        runtime.supply([Response::Accepted { id: 10 }]);
        settings
            .tick(Instant::now(), &mut |command| runtime.send(command))
            .unwrap();
        runtime.supply([stored(1, json!([FIRST])), Response::Ok]);
        settings
            .tick(Instant::now(), &mut |command| runtime.send(command))
            .unwrap();
        assert!(settings.csrf_origins().permits(Some(FIRST)));

        runtime.supply([Response::Accepted { id: 20 }]);
        settings.receive("amaterasu.management", 7, 2000,
            json!({"action":"product_update","expected_revision":1,"csrf_allowed_origins":[FIRST,SECOND]}),
            &mut |command| runtime.send(command)).unwrap();
        assert!(settings.csrf_origins().permits(Some(FIRST)));
        assert!(!settings.csrf_origins().permits(Some(SECOND)));
        runtime.supply([
            Response::Ok,
            returned,
            Response::Ok,
            Response::Ok,
            Response::Accepted { id: 30 },
        ]);
        settings
            .tick(Instant::now(), &mut |command| runtime.send(command))
            .unwrap();
        assert_eq!(settings.maximum_peer(), None);
        assert!(settings.csrf_origins().as_slice().is_empty());
        assert_eq!(settings.payload()["code"], "settings_unavailable");
        assert!(runtime.commands.iter().any(|command| matches!(command,
            Command::Complete { request: 7, payload } if payload["code"] == "settings_outcome_unknown")));
        assert!(
            matches!(runtime.commands.last(), Some(Command::Request { payload, .. })
            if payload == &json!({"action":"product_get"}))
        );
        assert!(matches!(
            settings.pending.as_ref().map(|pending| &pending.purpose),
            Some(Purpose::Load)
        ));
    }
}
