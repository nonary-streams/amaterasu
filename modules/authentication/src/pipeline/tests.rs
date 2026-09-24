use super::*;
use std::collections::BTreeSet;

fn settings() -> Settings {
    Settings {
        administrator_principals: BTreeSet::from(["console".into(), "other-admin".into()]),
        allowed_scopes: BTreeSet::from(["catalog.read".into(), "catalog.write".into()]),
        ..Settings::default()
    }
}

fn settings_with_ttls(session_ttl: u64, remembered_refresh_ttl: u64) -> Settings {
    Settings {
        session_token_ttl_seconds: session_ttl,
        remember_me_refresh_token_ttl_seconds: remembered_refresh_ttl,
        ..settings()
    }
}

struct StateHarness {
    stored: Option<Value>,
    revision: u64,
    state_response: Option<Response>,
    incoming: Option<Response>,
    output: Option<Value>,
    writes: usize,
    fail_write: bool,
    cancelled: bool,
}
impl StateHarness {
    fn new() -> Self {
        Self {
            stored: None,
            revision: 0,
            state_response: None,
            incoming: None,
            output: None,
            writes: 0,
            fail_write: false,
            cancelled: false,
        }
    }
    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
            Command::Poll {} => self.incoming.take().unwrap_or(Response::Pending),
            Command::Request {
                service,
                parent,
                payload,
                ..
            } => {
                assert_eq!(service, STATE_SERVICE);
                assert_eq!(parent, Some(7));
                assert_eq!(payload["key"], STATE_KEY);
                let response = if payload["action"] == "get" {
                    json!({"ok":true,"revision":self.revision,"found":self.stored.is_some(),"value":self.stored})
                } else {
                    assert_eq!(payload["expected_revision"], self.revision);
                    self.writes += 1;
                    if self.fail_write {
                        json!({"ok":false,"error":{"code":"io_error","message":"sensitive transport failure"}})
                    } else {
                        self.stored = Some(payload["value"].clone());
                        self.revision += 1;
                        json!({"ok":true,"revision":self.revision})
                    }
                };
                self.state_response = Some(Response::Result { payload: response });
                Response::Accepted { id: 50 }
            }
            Command::TakeResult { request: 50 } => {
                self.state_response.take().unwrap_or(Response::Pending)
            }
            Command::Complete {
                request: 7,
                payload,
            } => {
                self.output = Some(payload);
                if self.cancelled {
                    Response::error("request_ended", "cancelled")
                } else {
                    Response::Ok
                }
            }
            _ => panic!("unexpected authentication command"),
        })
    }
    fn request(
        &mut self,
        pipeline: &mut Pipeline,
        caller: &str,
        payload: Value,
        now: u64,
    ) -> Value {
        self.incoming = Some(Response::Request {
            id: 7,
            caller: caller.into(),
            initiator: "untrusted-payload-identity".into(),
            service: SERVICE.into(),
            remaining_ms: 30_000,
            payload,
        });
        self.output = None;
        for _ in 0..5 {
            pipeline.tick_at(now, |command| self.send(command)).unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
        }
        panic!("request did not finish");
    }
    fn setup(&mut self, pipeline: &mut Pipeline) {
        assert_eq!(self.request(pipeline,"console",json!({"action":"setup","username":"admin","password":"correct horse battery staple"}),100)["ok"],true);
    }
}

#[test]
fn credential_persistence_wrong_password_and_frontend_role_boundary() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let setup =
        json!({"action":"setup","username":"admin","password":"correct horse battery staple"});
    assert_eq!(
        state.request(&mut pipeline, "frontend", setup.clone(), 100)["code"],
        "forbidden"
    );
    state.setup(&mut pipeline);
    let persisted = state.stored.as_ref().unwrap().to_string();
    assert!(!persisted.contains("correct horse"));
    assert!(persisted.contains("$argon2id$"));
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(&mut restarted, "console", setup, 100)["code"],
        "already_initialized"
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"login","username":"admin","password":"incorrect"}),
            100
        )["code"],
        "unauthorized"
    );
    let login = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    assert_eq!(login["kind"], "administrator_session");
    let bearer = login["bearer"].clone();
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":["catalog.write"]}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
            101
        )["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":["ungranted"]}),
            101
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":[]}),
            7300
        )["code"],
        "unauthorized"
    );
}

#[test]
fn durable_token_digest_scope_expiry_and_foreign_revoke() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let issued = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
        100,
    );
    assert_eq!(issued["ok"], true);
    let bearer = issued["bearer"].clone();
    let token_id = issued["token_id"].clone();
    assert_eq!(
        token_id,
        crate::crypto::token_digest(bearer.as_str().unwrap())
    );
    assert!(
        !state
            .stored
            .as_ref()
            .unwrap()
            .to_string()
            .contains(bearer.as_str().unwrap())
    );
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":["catalog.read"]}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":["catalog.write"]}),
            101
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "other-admin",
            json!({"action":"revoke_token","token_id":token_id}),
            101
        )["code"],
        "token_not_found"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"revoke_token","token_id":token_id}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":[]}),
            102
        )["code"],
        "unauthorized"
    );
    let issued = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":1}),
        102,
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":issued["bearer"],"required_scopes":[]}),
            103
        )["code"],
        "unauthorized"
    );
}

#[test]
fn no_success_before_persistence_and_cancelled_commit_remains_durable() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.incoming = Some(Response::Request {
        id: 7,
        caller: "console".into(),
        initiator: "console".into(),
        service: SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"setup","username":"admin","password":"correct horse battery staple"}),
    });
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    assert!(state.output.is_none());
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    assert_eq!(state.writes, 1);
    assert!(state.output.is_none());
    state.cancelled = true;
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    assert!(pipeline.pending.is_none());
    assert!(state.stored.as_ref().unwrap()["credential"].is_object());
    state.cancelled = false;
    state.fail_write = true;
    let failed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
        100,
    );
    assert_eq!(failed, error("state_unavailable"));
    assert!(
        state.stored.as_ref().unwrap()["tokens"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn login_budget_is_bounded_and_recovers_after_window() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let login = json!({"action":"login","username":"admin","password":"incorrect"});
    for _ in 0..5 {
        assert_eq!(
            state.request(&mut pipeline, "frontend", login.clone(), 100)["code"],
            "unauthorized"
        );
    }
    assert_eq!(
        state.request(&mut pipeline, "frontend", login.clone(), 100)["code"],
        "rate_limited"
    );
    assert_eq!(
        state.request(&mut pipeline, "frontend", login.clone(), 159)["code"],
        "rate_limited"
    );
    assert_eq!(
        state.request(&mut pipeline, "frontend", login, 160)["code"],
        "unauthorized"
    );
}

#[test]
fn remembered_refresh_rotates_without_sliding_absolute_expiry() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings_with_ttls(120, 400));
    state.setup(&mut pipeline);

    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    assert_eq!(login["expires_at"], 220);
    assert_eq!(login["refresh_expires_at"], 500);
    assert_eq!(login["remember_me"], true);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            219,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            220,
        )["code"],
        "unauthorized"
    );

    let rotated = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        220,
    );
    assert_eq!(rotated["expires_at"], 340);
    assert_eq!(rotated["refresh_expires_at"], 500);
    assert_eq!(rotated["remember_me"], true);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":login["refresh_token"]}),
            220,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":[]}),
            339,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":[]}),
            340,
        )["code"],
        "unauthorized"
    );

    // Refresh remains usable after access expiry, but the new access token is
    // capped at the original absolute refresh deadline.
    let rotated_again = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":rotated["refresh_token"]}),
        420,
    );
    assert_eq!(rotated_again["expires_at"], 500);
    assert_eq!(rotated_again["refresh_expires_at"], 500);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":rotated_again["bearer"],"required_scopes":[]}),
            499,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":rotated_again["refresh_token"]}),
            500,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn default_refresh_lifetime_and_logout_revoke_the_entire_token_pair() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);

    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    assert_eq!(login["expires_at"], 100 + 7200);
    assert_eq!(login["refresh_expires_at"], 100 + 86400);
    assert_eq!(login["remember_me"], false);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"logout","refresh_token":login["refresh_token"]}),
            101,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            101,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":login["refresh_token"]}),
            101,
        )["code"],
        "unauthorized"
    );

    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        102,
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"logout","bearer":login["bearer"]}),
            103,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":login["refresh_token"]}),
            103,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn malformed_password_hash_and_unbounded_cost_fail_closed() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let hash = state.stored.as_ref().unwrap()["credential"]["password_hash"]
        .as_str()
        .unwrap()
        .replace("m=19456", "m=999999999");
    state.stored.as_mut().unwrap()["credential"]["password_hash"] = json!(hash);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
            100
        )["code"],
        "invalid_state"
    );
}

#[test]
fn readiness_status_reports_only_verifier_presence() {
    let mut pipeline = Pipeline::new(settings());
    let mut state = StateHarness::new();
    assert_eq!(
        state.request(
            &mut pipeline,
            "management",
            json!({"action":"status"}),
            1_700_000_000
        ),
        json!({"ok":true,"verifier_available":false})
    );
}

#[test]
fn successful_legacy_password_proof_upgrades_the_durable_verifier() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let salt = "AbCdEfGhIjKlMnOp";
    let password = "correct horse battery staple";
    let legacy_hash = crate::crypto::token_digest(&format!("{password}{salt}"));
    state.stored = Some(json!({
        "credential": {
            "username": "Admin",
            "password_hash": legacy_hash,
            "legacy_salt": salt,
            "case_insensitive_username": true
        },
        "tokens": []
    }));

    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"login","username":"ADMIN","password":"wrong"}),
            100,
        )["code"],
        "unauthorized"
    );
    let response = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password}),
        100,
    );
    assert_eq!(response["ok"], true);
    let credential = &state.stored.as_ref().unwrap()["credential"];
    assert!(credential.get("legacy_salt").is_none());
    assert!(crate::crypto::valid_password_hash(
        credential["password_hash"].as_str().unwrap()
    ));
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":response["bearer"],"required_scopes":[]}),
            101,
        )["ok"],
        true
    );
}

#[test]
fn token_capacity_and_scopes_cannot_be_extended_by_payload_identity() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    assert_eq!(state.request(&mut pipeline, "frontend", json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":1,"caller":"console"}),100)["code"],"invalid_request");
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"issue_token","scopes":["*"],"ttl_seconds":1}),
            100
        )["code"],
        "invalid_request"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"issue_token","scopes":["other.scope"],"ttl_seconds":1}),
            100
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":2592001u64}),
            100
        )["code"],
        "invalid_request"
    );
    for _ in 0..32 {
        assert_eq!(
            state.request(
                &mut pipeline,
                "console",
                json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
                100
            )["ok"],
            true
        );
    }
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
            100
        )["code"],
        "token_limit"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
            160
        )["ok"],
        true
    );
}

#[test]
fn global_login_budget_survives_changed_usernames_and_callers() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    for index in 0..20 {
        assert_eq!(
            state.request(
                &mut pipeline,
                &format!("caller-{index}"),
                json!({"action":"login","username":format!("user-{index}"),"password":"wrong"}),
                100
            )["code"],
            "unauthorized"
        );
    }
    assert_eq!(
        state.request(
            &mut pipeline,
            "new-caller",
            json!({"action":"login","username":"new-user","password":"wrong"}),
            100
        )["code"],
        "rate_limited"
    );
}

#[test]
fn cancelled_state_read_does_not_hash_or_issue_and_releases_pending_request() {
    let mut pipeline = Pipeline::new(settings());
    pipeline.pending = Some(Pending {
        request: Request { id:7, caller:"console".into(), deadline: Instant::now() + Duration::from_secs(30) },
        state_request:50,
        stage: Stage::Loading(serde_json::from_value(json!({"action":"setup","username":"admin","password":"correct horse battery staple"})).unwrap()),
    });
    let mut completed = false;
    pipeline
        .tick_at(100, |command| {
            Ok(match command {
                Command::TakeResult { request: 50 } => Response::error("cancelled", "parent ended"),
                Command::Complete { payload, .. } => {
                    assert_eq!(payload, error("state_unavailable"));
                    completed = true;
                    Response::error("request_ended", "cancelled")
                }
                _ => panic!("cancelled request must not write"),
            })
        })
        .unwrap();
    assert!(completed);
    assert!(pipeline.pending.is_none());
}

#[test]
fn logout_and_restart_invalidate_sessions_without_changing_credential() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let login =
        json!({"action":"login","username":"admin","password":"correct horse battery staple"});
    let session = state.request(&mut pipeline, "frontend", login.clone(), 100);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"logout","bearer":session["bearer"]}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101
        )["code"],
        "unauthorized"
    );
    let session = state.request(&mut pipeline, "frontend", login, 101);
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            102
        )["code"],
        "unauthorized"
    );
    assert_eq!(state.writes, 1);
}

#[test]
fn password_rotation_waits_for_commit_and_cancelled_commit_invalidates_old_sessions() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let login =
        json!({"action":"login","username":"admin","password":"correct horse battery staple"});
    let session = state.request(&mut pipeline, "frontend", login.clone(), 100);
    let rotate = json!({"action":"change_password","password":"new administrator password"});
    assert_eq!(
        state.request(&mut pipeline, "frontend", rotate.clone(), 100)["code"],
        "forbidden"
    );
    state.fail_write = true;
    assert_eq!(
        state.request(&mut pipeline, "console", rotate.clone(), 100)["code"],
        "state_unavailable"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            100
        )["ok"],
        true
    );
    state.fail_write = false;
    state.output = None;
    state.incoming = Some(Response::Request {
        id: 7,
        caller: "console".into(),
        initiator: "console".into(),
        service: SERVICE.into(),
        remaining_ms: 30_000,
        payload: rotate,
    });
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    assert!(state.output.is_none());
    // The durable state changed, but cancellation hides the successful state
    // response. Session credential binding must still invalidate old sessions.
    state.state_response = Some(Response::error("cancelled", "parent ended"));
    pipeline
        .tick_at(100, |command| state.send(command))
        .unwrap();
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(&mut pipeline, "frontend", login, 101)["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"login","username":"admin","password":"new administrator password"}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"change_password","password":"third administrator password"}),
            101
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"list_sessions"}),
            101
        )["sessions"],
        json!([])
    );
}

#[test]
fn administrator_session_metadata_is_not_a_bearer_and_revocation_works() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"list_sessions"}),
            100
        )["code"],
        "forbidden"
    );
    let list = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_sessions","offset":0,"limit":4}),
        100,
    );
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    let session_id = list["sessions"][0]["session_id"].clone();
    assert_eq!(
        session_id,
        crate::crypto::token_digest(session["bearer"].as_str().unwrap())
    );
    assert!(
        !list
            .to_string()
            .contains(session["bearer"].as_str().unwrap())
    );
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"verify","bearer":format!("ama_{}",session_id.as_str().unwrap()),"required_scopes":[]}),100)["code"],"unauthorized");
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"revoke_session","session_id":session_id}),
            100
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":session["refresh_token"]}),
            100
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            100
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"revoke_session","session_id":session_id}),
            100
        )["ok"],
        true
    );
}
