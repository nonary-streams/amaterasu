use super::*;
use std::path::PathBuf;

struct Fixture {
    coordinator: Option<Coordinator>,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("revocation-fairness-{}", identifier().unwrap()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            coordinator: Some(Coordinator::open(&root.join("journal.json")).unwrap()),
            root,
        }
    }

    fn coordinator(&mut self) -> &mut Coordinator {
        self.coordinator.as_mut().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.coordinator.take();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn pending_revocations_do_not_starve_ninth_binding_or_replace_requests() {
    let mut fixture = Fixture::new();
    let coordinator = fixture.coordinator();
    let mut next = coordinator.shared.borrow().value.clone();
    for index in 0..9 {
        let binding = crate::input_authority::ActiveBinding {
            host_epoch: next.host_epoch.clone(),
            session_id: "session".into(),
            stream_id: format!("stream-{index}"),
            generation: 1,
            geometry_revision: 1,
            input_consumer: "input-provider".into(),
            allowed_actions: Default::default(),
        };
        let token = next.inputs.issue_input_binding(&binding).unwrap();
        next.inputs.fence(&binding);
        next.input_bindings.insert(token.clone(), binding);
        next.revoked_inputs.insert(token);
    }
    let sorted_tokens: Vec<_> = next.revoked_inputs.iter().cloned().collect();
    save(&coordinator.shared, next).unwrap();

    let mut requests = BTreeMap::<String, (u64, String)>::new();
    let mut polled = Vec::new();
    for turn in 0..3 {
        coordinator
            .advance_validations(&mut |command| {
                Ok(match command {
                    Command::Request { payload, .. } => {
                        assert_eq!(payload["action"], "revoke_binding");
                        let token = payload["token"].as_str().unwrap().to_owned();
                        let operation = payload["operation_id"].as_str().unwrap().to_owned();
                        let id = requests.len() as u64 + 1;
                        assert!(requests.insert(token, (id, operation)).is_none());
                        Response::Accepted { id }
                    }
                    Command::TakeResult { request } => {
                        polled.push(request);
                        Response::Pending
                    }
                    _ => panic!("unexpected command {command:?}"),
                })
            })
            .unwrap();
        if turn == 0 {
            assert_eq!(requests.len(), 8);
            assert!(!requests.contains_key(&sorted_tokens[8]));
        } else {
            assert_eq!(requests.len(), 9);
            assert!(requests.contains_key(&sorted_tokens[8]));
        }
    }
    for token in &sorted_tokens[..8] {
        assert!(polled.contains(&requests[token].0));
    }
    assert_eq!(coordinator.revocations.len(), 9);
}
