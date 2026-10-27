use super::*;
use crate::protocol::SESSION_DOCUMENT_VERSION;
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
    session_stored: Option<Value>,
    session_revision: u64,
    legacy_stored: Option<Value>,
    legacy_revision: u64,
    state_response: Option<Response>,
    product_response: Option<Response>,
    incoming: Option<Response>,
    output: Option<Value>,
    writes: usize,
    product_revision: u64,
    product_values: Option<Value>,
    product_writes: usize,
    released_results: usize,
    release_failures_remaining: usize,
    fail_write: bool,
    fail_session_write: bool,
    cancelled: bool,
}
impl StateHarness {
    fn new() -> Self {
        Self {
            stored: None,
            revision: 0,
            session_stored: None,
            session_revision: 0,
            legacy_stored: None,
            legacy_revision: 0,
            state_response: None,
            product_response: None,
            incoming: None,
            output: None,
            writes: 0,
            product_revision: 0,
            product_values: None,
            product_writes: 0,
            released_results: 0,
            release_failures_remaining: 0,
            fail_write: false,
            fail_session_write: false,
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
                assert_eq!(parent, Some(7));
                if service == STATE_SERVICE {
                    let key = payload["key"].as_str().unwrap();
                    let response = if key == STATE_KEY {
                        if payload["action"] == "get" {
                            json!({"ok":true,"revision":self.revision,"found":self.stored.is_some(),"value":self.stored})
                        } else {
                            assert_eq!(payload["action"], "set");
                            assert_eq!(payload["expected_revision"], self.revision);
                            self.writes += 1;
                            if self.fail_write {
                                json!({"ok":false,"error":{"code":"io_error","message":"sensitive transport failure"}})
                            } else {
                                self.stored = Some(payload["value"].clone());
                                self.revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                        }
                    } else if key == SESSION_STATE_KEY {
                        if payload["action"] == "get" {
                            json!({"ok":true,"revision":self.revision,"found":self.session_stored.is_some(),"value":self.session_stored})
                        } else {
                            assert_eq!(payload["action"], "set");
                            assert_eq!(payload["expected_revision"], self.revision);
                            self.writes += 1;
                            if self.fail_write || self.fail_session_write {
                                json!({"ok":false,"error":{"code":"io_error","message":"sensitive transport failure"}})
                            } else {
                                self.session_stored = Some(payload["value"].clone());
                                self.revision += 1;
                                self.session_revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                        }
                    } else {
                        assert_eq!(key, LEGACY_API_TOKEN_STATE_KEY);
                        if payload["action"] == "get" {
                            json!({"ok":true,"revision":self.revision,"found":self.legacy_stored.is_some(),"value":self.legacy_stored})
                        } else {
                            assert_eq!(payload["action"], "set");
                            assert_eq!(payload["expected_revision"], self.revision);
                            self.writes += 1;
                            if self.fail_write {
                                json!({"ok":false,"error":{"code":"io_error","message":"sensitive transport failure"}})
                            } else {
                                self.legacy_stored = Some(payload["value"].clone());
                                self.revision += 1;
                                self.legacy_revision += 1;
                                json!({"ok":true,"revision":self.revision})
                            }
                        }
                    };
                    self.state_response = Some(Response::Result { payload: response });
                    Response::Accepted { id: 50 }
                } else {
                    assert_eq!(service, CONFIGURATION_SERVICE);
                    assert_eq!(payload["version"].as_u64().unwrap_or(1), 1);
                    let response = if payload["action"] == "product_get" {
                        match &self.product_values {
                            Some(values) => json!({
                                "ok":true,"found":true,"version":1,
                                "revision":self.product_revision,"values":values
                            }),
                            None => json!({
                                "ok":true,"found":false,"version":null,
                                "revision":0,"values":{}
                            }),
                        }
                    } else {
                        assert_eq!(payload["action"], "product_update");
                        let expected_revision = payload["expected_revision"].as_u64().unwrap();
                        if expected_revision != self.product_revision {
                            json!({"ok":false,"error":{"code":"revision_conflict","message":"revision differs","current_revision":self.product_revision}})
                        } else {
                            self.product_writes += 1;
                            self.product_revision += 1;
                            self.product_values = Some(payload["values"].clone());
                            json!({
                                "ok":true,"found":true,"version":payload["version"],
                                "revision":self.product_revision,"values":self.product_values
                            })
                        }
                    };
                    self.product_response = Some(Response::Result { payload: response });
                    Response::Accepted { id: 60 }
                }
            }
            Command::TakeResult { request: 50 } => {
                self.state_response.take().unwrap_or(Response::Pending)
            }
            Command::TakeResult { request: 60 } => {
                self.product_response.take().unwrap_or(Response::Pending)
            }
            Command::ReleaseResult { request: 50 | 60 } => {
                if self.release_failures_remaining > 0 {
                    self.release_failures_remaining -= 1;
                    Response::error("temporary", "release temporarily failed")
                } else {
                    self.released_results += 1;
                    Response::Ok
                }
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
        for _ in 0..12 {
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
            7299
        )["ok"],
        true
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
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":bearer,"required_scopes":[]}),
            7301
        )["code"],
        "unauthorized"
    );
}

#[test]
fn administrator_ttl_settings_use_cas_persist_and_apply_to_later_logins_after_restart() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);

    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"settings_get"}),
            100,
        )["code"],
        "forbidden"
    );
    let defaults = state.request(
        &mut pipeline,
        "console",
        json!({"action":"settings_get"}),
        100,
    );
    assert_eq!(defaults["revision"], 0);
    assert_eq!(defaults["values"]["session_token_ttl_seconds"], 7200);
    assert_eq!(
        defaults["values"]["remember_me_refresh_token_ttl_seconds"],
        604800
    );

    let original_session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    assert_eq!(original_session["expires_at"], 7_300);
    assert_eq!(original_session["refresh_expires_at"], 604_900);

    state.release_failures_remaining = 1;
    let saved = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"settings_update",
            "session_token_ttl_seconds":3600,
            "remember_me_refresh_token_ttl_seconds":259200,
            "expected_revision":0
        }),
        101,
    );
    assert_eq!(saved["ok"], true);
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["values"]["session_token_ttl_seconds"], 3600);
    assert_eq!(
        saved["values"]["remember_me_refresh_token_ttl_seconds"],
        259200
    );
    assert_eq!(
        state.product_writes, 1,
        "release retry must not repeat the mutation"
    );

    let stale = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"settings_update",
            "session_token_ttl_seconds":60,
            "remember_me_refresh_token_ttl_seconds":60,
            "expected_revision":0
        }),
        102,
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert_eq!(stale["error"]["current_revision"], 1);
    assert_eq!(state.product_writes, 1);

    let still_live = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"verify","bearer":original_session["bearer"],"required_scopes":[]}),
        103,
    );
    assert_eq!(still_live["ok"], true);
    assert_eq!(still_live["expires_at"], 7_300);

    let refreshed = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":original_session["refresh_token"]}),
        103,
    );
    assert_eq!(refreshed["expires_at"], 3_703);
    assert_eq!(refreshed["refresh_expires_at"], 604_900);
    assert_eq!(refreshed["remember_me"], true);

    let new_session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        104,
    );
    assert_eq!(new_session["expires_at"], 3_704);
    assert_eq!(new_session["refresh_expires_at"], 259_304);

    let mut restarted = Pipeline::new(settings());
    let after_restart = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        200,
    );
    assert_eq!(after_restart["expires_at"], 3_800);
    assert_eq!(after_restart["refresh_expires_at"], 259_400);
}

#[test]
fn release_failure_retries_terminal_result_without_repeating_state_write() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.release_failures_remaining = 1;
    state.setup(&mut pipeline);
    assert_eq!(state.writes, 1);
    assert!(state.released_results >= 2);
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
fn verifier_context_is_stable_across_refresh_and_distinct_for_sessions_and_tokens() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);

    let first = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    let first_verified = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"verify","bearer":first["bearer"],"required_scopes":[]}),
        101,
    );
    assert_eq!(
        first_verified["auth_context_id"].as_str().unwrap().len(),
        64
    );

    let refreshed = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":first["refresh_token"]}),
        102,
    );
    let refreshed_verified = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":[]}),
        103,
    );
    assert_eq!(
        first_verified["auth_context_id"],
        refreshed_verified["auth_context_id"]
    );

    let second = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        104,
    );
    let second_verified = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"verify","bearer":second["bearer"],"required_scopes":[]}),
        105,
    );
    assert_ne!(
        refreshed_verified["auth_context_id"],
        second_verified["auth_context_id"]
    );

    let token = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":60}),
        106,
    );
    let token_verified = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"verify","bearer":token["bearer"],"required_scopes":["catalog.read"]}),
        107,
    );
    assert_eq!(token_verified["auth_context_id"], token["token_id"]);
    assert_eq!(
        token_verified["kind"], "api_token",
        "token={token:?} verification={token_verified:?}"
    );

    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"logout","bearer":refreshed["bearer"]}),
            108,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":[]}),
            109,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn legacy_session_context_migrates_before_refresh_and_survives_reload() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    let legacy_context = state.session_stored.as_ref().unwrap()["sessions"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sessions = state.session_stored.as_mut().unwrap()["sessions"]
        .as_array_mut()
        .unwrap();
    sessions[0]
        .as_object_mut()
        .unwrap()
        .remove("ownership_context_id");
    state.session_stored.as_mut().unwrap()["version"] =
        json!(crate::protocol::POLICY_GENERATION_SESSION_DOCUMENT_VERSION);

    let mut restarted = Pipeline::new(settings());
    let refreshed = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        101,
    );
    assert_eq!(refreshed["ok"], true);
    assert_eq!(
        state.session_stored.as_ref().unwrap()["version"],
        SESSION_DOCUMENT_VERSION
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["ownership_context_id"],
        legacy_context
    );

    let verified = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":[]}),
        102,
    );
    assert_eq!(verified["auth_context_id"], legacy_context);
}

#[test]
fn api_tokens_do_not_revive_when_an_administrator_principal_is_restored() {
    let mut state = StateHarness::new();
    let mut original_settings = settings();
    original_settings.authorization_policy_generation = 41;
    let mut pipeline = Pipeline::new(original_settings.clone());
    state.setup(&mut pipeline);
    let issued = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":600}),
        100,
    );
    assert_eq!(issued["ok"], true);
    assert_eq!(
        state.stored.as_ref().unwrap()["tokens"][0]["policy_generation"],
        41
    );

    let verify =
        json!({"action":"verify","bearer":issued["bearer"],"required_scopes":["catalog.read"]});
    let mut removed_settings = original_settings.clone();
    removed_settings.administrator_principals.remove("console");
    removed_settings.authorization_policy_generation = 42;
    let mut after_removal = Pipeline::new(removed_settings);
    assert_eq!(
        state.request(&mut after_removal, "frontend", verify.clone(), 101),
        error("unauthorized")
    );

    let mut restored_settings = original_settings;
    restored_settings.authorization_policy_generation = 43;
    let mut after_restoration = Pipeline::new(restored_settings);
    assert_eq!(
        state.request(&mut after_restoration, "frontend", verify, 102),
        error("unauthorized")
    );
    assert_eq!(
        state.request(
            &mut after_restoration,
            "console",
            json!({"action":"list_tokens"}),
            102,
        )["tokens"],
        json!([])
    );
    let replacement = state.request(
        &mut after_restoration,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":600}),
        102,
    );
    assert_eq!(replacement["ok"], true);
    assert_eq!(
        state.stored.as_ref().unwrap()["tokens"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut restarted = Pipeline::new(Settings {
        authorization_policy_generation: 43,
        ..settings()
    });
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":replacement["bearer"],"required_scopes":["catalog.read"]}),
            103,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":issued["bearer"],"required_scopes":[]}),
            103,
        ),
        error("unauthorized")
    );
}

#[test]
fn api_tokens_without_a_policy_generation_are_not_trusted_after_restart() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let issued = state.request(
        &mut pipeline,
        "console",
        json!({"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":600}),
        100,
    );
    state.stored.as_mut().unwrap()["tokens"][0]
        .as_object_mut()
        .unwrap()
        .remove("policy_generation");
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":issued["bearer"],"required_scopes":[]}),
            101,
        ),
        error("unauthorized")
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "console",
            json!({"action":"list_tokens"}),
            101
        )["tokens"],
        json!([])
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "console",
            json!({"action":"revoke_token","token_id":issued["token_id"]}),
            101,
        )["ok"],
        true
    );
    assert_eq!(state.stored.as_ref().unwrap()["tokens"], json!([]));
}

fn seeded_legacy_api_token_store(bearer: &str) -> Value {
    json!({
        "version": 1,
        "tokens": [{
            "token_id": crate::crypto::token_digest(bearer),
            "record_version": 1,
            "owner": "legacy-admin",
            "created_at": 1_700_000_000i64,
            "scopes": [{"path":"/v1/apps/[0-9]+","methods":["POST"]}]
        }]
    })
}

#[test]
fn legacy_api_tokens_need_both_route_and_named_policy_proofs_before_upgrade() {
    const LEGACY_BEARER: &str = "0123456789abcdefghijklmnopqrstuv";
    let verify = |path: &str| {
        json!({
            "action":"verify",
            "bearer":LEGACY_BEARER,
            "required_scopes":["catalog.read"],
            "legacy_request":{"path":path,"method":"post"}
        })
    };

    let mut wrong_path_state = StateHarness::new();
    wrong_path_state.revision = 1;
    wrong_path_state.legacy_stored = Some(seeded_legacy_api_token_store(LEGACY_BEARER));
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        wrong_path_state.request(&mut pipeline, "console", verify("/v1/apps/42/extra"), 100),
        error("unauthorized")
    );
    assert_eq!(wrong_path_state.writes, 0);
    assert_eq!(
        wrong_path_state.legacy_stored.as_ref().unwrap()["version"],
        1
    );

    let mut policy_state = StateHarness::new();
    policy_state.revision = 1;
    policy_state.legacy_stored = Some(seeded_legacy_api_token_store(LEGACY_BEARER));
    let mut policy_without_route_scope = settings();
    policy_without_route_scope
        .allowed_scopes
        .remove("catalog.read");
    let mut pipeline = Pipeline::new(policy_without_route_scope);
    assert_eq!(
        policy_state.request(&mut pipeline, "console", verify("/v1/apps/42"), 100),
        error("unauthorized")
    );
    assert_eq!(policy_state.writes, 0);
    assert_eq!(policy_state.legacy_stored.as_ref().unwrap()["version"], 1);

    let mut accepted_state = StateHarness::new();
    accepted_state.revision = 1;
    accepted_state.legacy_stored = Some(seeded_legacy_api_token_store(LEGACY_BEARER));
    let mut pipeline = Pipeline::new(settings());
    let accepted = accepted_state.request(&mut pipeline, "console", verify("/v1/apps/42"), 100);
    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["kind"], "api_token");
    assert_eq!(accepted["scopes"], json!(["catalog.read"]));
    assert!(accepted["expires_at"].is_null());
    assert_eq!(accepted_state.writes, 1);
    assert_eq!(accepted_state.legacy_stored.as_ref().unwrap()["version"], 2);
    assert_eq!(
        accepted_state.legacy_stored.as_ref().unwrap()["tokens"][0]["record_version"],
        2
    );

    let listed = accepted_state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_tokens"}),
        101,
    );
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["legacy_tokens"][0]["expires_at"], Value::Null);
    assert!(listed["legacy_tokens"][0].get("scopes").is_none());
    assert!(listed["legacy_tokens"][0].get("path").is_none());
}

#[test]
fn legacy_api_token_without_route_context_cannot_authorize() {
    const LEGACY_BEARER: &str = "0123456789abcdefghijklmnopqrstuv";
    let mut state = StateHarness::new();
    state.revision = 1;
    state.legacy_stored = Some(seeded_legacy_api_token_store(LEGACY_BEARER));
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action":"verify",
                "bearer":LEGACY_BEARER,
                "required_scopes":["catalog.read"]
            }),
            100
        ),
        error("unauthorized")
    );
    assert_eq!(state.writes, 0);
    assert_eq!(state.legacy_stored.as_ref().unwrap()["version"], 1);
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
        "unauthorized",
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
        "unauthorized",
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
    let rotated_one_second_before_expiry = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":rotated_again["refresh_token"]}),
        499,
    );
    assert_eq!(rotated_one_second_before_expiry["expires_at"], 500);
    assert_eq!(rotated_one_second_before_expiry["refresh_expires_at"], 500);
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":rotated_one_second_before_expiry["bearer"],"required_scopes":[]}),
            499,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":rotated_one_second_before_expiry["bearer"],"required_scopes":[]}),
            500,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"refresh","refresh_token":rotated_one_second_before_expiry["refresh_token"]}),
            500,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"list_sessions"}),
            500,
        )["sessions"],
        json!([])
    );
}

#[test]
fn session_expiry_boundaries_are_strict_across_restart() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings_with_ttls(10, 20));
    state.setup(&mut pipeline);

    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        1_000,
    );
    assert_eq!(login["expires_at"], 1_010);
    assert_eq!(login["refresh_expires_at"], 1_020);

    let mut restarted = Pipeline::new(settings_with_ttls(10, 20));
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            1_009,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            1_010,
        )["code"],
        "unauthorized"
    );

    // The access bearer has expired, but the original refresh deadline is
    // still one second away, so rotation remains available.
    let refreshed = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        1_019,
    );
    assert_eq!(refreshed["expires_at"], 1_020);
    assert_eq!(refreshed["refresh_expires_at"], 1_020);

    let mut at_refresh_deadline = Pipeline::new(settings_with_ttls(10, 20));
    assert_eq!(
        state.request(
            &mut at_refresh_deadline,
            "frontend",
            json!({"action":"refresh","refresh_token":refreshed["refresh_token"]}),
            1_020,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut at_refresh_deadline,
            "frontend",
            json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":[]}),
            1_020,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut at_refresh_deadline,
            "console",
            json!({"action":"list_sessions"}),
            1_020,
        )["sessions"],
        json!([])
    );
}

#[test]
fn administrator_session_and_refresh_rotation_survive_restart() {
    let mut state = StateHarness::new();
    let lutris_settings = Settings {
        allowed_scopes: BTreeSet::from(["lutris.read".into()]),
        ..settings_with_ttls(120, 400)
    };
    let mut pipeline = Pipeline::new(lutris_settings.clone());
    state.setup(&mut pipeline);
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    assert_eq!(login["ok"], true);
    assert_eq!(login["scopes"], json!(["lutris.read"]));
    assert_eq!(state.revision, 3);
    assert!(state.stored.as_ref().unwrap()["credential"].is_null());
    let credential_document = state.stored.clone();
    assert_eq!(state.session_revision, 1);
    let stored = state.session_stored.as_ref().unwrap();
    assert_eq!(stored["sessions"].as_array().unwrap().len(), 1);
    let ownership_context_id = stored["sessions"][0]["ownership_context_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        ownership_context_id,
        stored["sessions"][0]["session_id"].as_str().unwrap()
    );
    assert!(state.stored.as_ref().unwrap().get("sessions").is_none());
    assert_eq!(stored["version"], SESSION_DOCUMENT_VERSION);
    assert!(
        !stored
            .to_string()
            .contains(login["bearer"].as_str().unwrap())
    );
    assert!(
        !stored
            .to_string()
            .contains(login["refresh_token"].as_str().unwrap())
    );

    let mut restarted = Pipeline::new(lutris_settings.clone());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":["lutris.read"]}),
            101,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":["lutris.write"]}),
            101,
        )["code"],
        "unauthorized"
    );

    let rotated = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        220,
    );
    assert_eq!(rotated["ok"], true);
    assert_eq!(rotated["scopes"], json!(["lutris.read"]));
    assert_eq!(rotated["refresh_expires_at"], 500);
    assert_eq!(state.revision, 4);
    assert_eq!(state.stored, credential_document);
    assert_eq!(state.session_revision, 2);
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["session_id"],
        crate::crypto::token_digest(rotated["bearer"].as_str().unwrap())
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["refresh_token_digest"],
        crate::crypto::token_digest(rotated["refresh_token"].as_str().unwrap())
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["ownership_context_id"],
        ownership_context_id
    );

    let mut restarted_again = Pipeline::new(lutris_settings.clone());
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":["lutris.read"]}),
            221,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":["lutris.write"]}),
            221,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            221,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"refresh","refresh_token":login["refresh_token"]}),
            221,
        )["code"],
        "unauthorized"
    );

    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"logout","refresh_token":rotated["refresh_token"]}),
            222,
        )["ok"],
        true
    );
    assert!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .is_none_or(|sessions| sessions.is_empty())
    );
    let mut after_logout = Pipeline::new(lutris_settings);
    assert_eq!(
        state.request(
            &mut after_logout,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":[]}),
            223,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn imported_primary_login_survives_reload_without_accepting_split_or_auxiliary_sources() {
    let source = std::env::temp_dir().join(format!(
        "amaterasu-auth-primary-auxiliary-source-{}",
        std::process::id()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "amaterasu-auth-primary-auxiliary-state-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&source);
    let _ = std::fs::remove_dir_all(&state_root);
    std::fs::create_dir_all(&source).unwrap();
    let salt = "AbCdEfGhIjKlMnOp";
    let password = "primary administrator password";
    let primary = json!({
        "username":"PrimaryAdmin",
        "password":crate::crypto::token_digest(&format!("{password}{salt}")),
        "salt":salt
    });
    let auxiliary = json!({
        "username":"AuxiliaryAdmin",
        "password":crate::crypto::token_digest(&format!("auxiliary password{salt}")),
        "salt":salt
    });
    std::fs::write(source.join("vibeshine_state.json"), auxiliary.to_string()).unwrap();
    assert!(matches!(
        crate::migration::import_legacy_authentication(&source),
        Err("legacy_authentication_credential_ambiguous")
    ));
    std::fs::write(source.join("apollo_state.json"), primary.to_string()).unwrap();
    std::fs::write(source.join("sunshine_state.json"), primary.to_string()).unwrap();
    assert!(matches!(
        crate::migration::import_legacy_authentication(&source),
        Err("legacy_authentication_credential_ambiguous")
    ));
    std::fs::remove_file(source.join("apollo_state.json")).unwrap();
    let import = crate::migration::import_legacy_authentication(&source).unwrap();
    assert_eq!(
        crate::migration::seed_authentication_state(&state_root, &import).unwrap(),
        crate::migration::AuthenticationSeedOutcome::Seeded
    );

    let saved: Value = serde_json::from_slice(
        &std::fs::read(crate::migration::namespace_path(&state_root)).unwrap(),
    )
    .unwrap();
    let mut state = StateHarness::new();
    state.revision = saved["revision"].as_u64().unwrap();
    state.stored = Some(saved["values"][STATE_KEY].clone());
    let mut loaded_authentication = Pipeline::new(settings());
    let login = state.request(
        &mut loaded_authentication,
        "frontend",
        json!({"action":"login","username":"primaryadmin","password":password}),
        100,
    );
    assert_eq!(login["ok"], true);
    assert_eq!(login["subject"], "PrimaryAdmin");
    assert_eq!(
        state.request(
            &mut loaded_authentication,
            "frontend",
            json!({"action":"login","username":"AuxiliaryAdmin","password":"auxiliary password"}),
            101,
        )["code"],
        "unauthorized"
    );

    std::fs::write(
        source.join("vibeshine_state.json"),
        r#"{"root":{"api_tokens":[{"token":"legacy-token"}]}}"#,
    )
    .unwrap();
    assert!(matches!(
        crate::migration::import_legacy_authentication(&source),
        Err("legacy_authentication_tokens_unsupported")
    ));
    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn correct_login_reclaims_one_full_remembered_session_durably() {
    let mut state = StateHarness::new();
    let configured_settings = settings_with_ttls(5_000, 5_000);
    let mut pipeline = Pipeline::new(configured_settings.clone());
    state.setup(&mut pipeline);
    let login = json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true});
    let mut original_sessions = Vec::new();
    for index in 0..16 {
        let issued = state.request(&mut pipeline, "frontend", login.clone(), 100 + index * 61);
        assert_eq!(issued["ok"], true);
        original_sessions.push(issued);
    }
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        16
    );

    let mut restarted = Pipeline::new(configured_settings.clone());
    let stored_before_failure = state.session_stored.clone();
    let revision_before_failure = state.revision;
    let session_revision_before_failure = state.session_revision;
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"login","username":"admin","password":"wrong","remember_me":true}),
            1_076
        )["code"],
        "unauthorized"
    );
    assert_eq!(state.session_stored, stored_before_failure);
    assert_eq!(state.revision, revision_before_failure);
    assert_eq!(state.session_revision, session_revision_before_failure);
    state.fail_session_write = true;
    assert_eq!(
        state.request(&mut restarted, "frontend", login.clone(), 1_077),
        error("state_unavailable")
    );
    assert_eq!(state.session_stored, stored_before_failure);
    assert_eq!(state.revision, revision_before_failure);
    assert_eq!(state.session_revision, session_revision_before_failure);
    state.fail_session_write = false;
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":original_sessions[0]["bearer"],"required_scopes":[]}),
            1_078
        )["ok"],
        true
    );

    let mut after_failed_write = Pipeline::new(configured_settings.clone());
    assert_eq!(
        state.request(
            &mut after_failed_write,
            "frontend",
            json!({"action":"verify","bearer":original_sessions[0]["bearer"],"required_scopes":[]}),
            1_078
        )["ok"],
        true
    );
    let replacement = state.request(&mut after_failed_write, "frontend", login, 1_079);
    assert_eq!(replacement["ok"], true);
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
    let serialized_sidecar = state.session_stored.as_ref().unwrap().to_string();
    for token in original_sessions
        .iter()
        .chain(std::iter::once(&replacement))
    {
        assert!(!serialized_sidecar.contains(token["bearer"].as_str().unwrap()));
        assert!(!serialized_sidecar.contains(token["refresh_token"].as_str().unwrap()));
    }

    let mut after_replacement = Pipeline::new(configured_settings.clone());
    assert_eq!(
        state.request(
            &mut after_replacement,
            "frontend",
            json!({"action":"verify","bearer":original_sessions[0]["bearer"],"required_scopes":[]}),
            1_080
        )["code"],
        "unauthorized"
    );
    for token in original_sessions
        .iter()
        .skip(1)
        .chain(std::iter::once(&replacement))
    {
        assert_eq!(
            state.request(
                &mut after_replacement,
                "frontend",
                json!({"action":"verify","bearer":token["bearer"],"required_scopes":[]}),
                1_080
            )["ok"],
            true
        );
    }
    assert_eq!(
        state.request(
            &mut after_replacement,
            "frontend",
            json!({"action":"refresh","refresh_token":original_sessions[0]["refresh_token"]}),
            1_080
        )["code"],
        "unauthorized"
    );
    let rotated = state.request(
        &mut after_replacement,
        "frontend",
        json!({"action":"refresh","refresh_token":replacement["refresh_token"]}),
        1_080,
    );
    assert_eq!(rotated["ok"], true);

    let mut after_rotation = Pipeline::new(configured_settings.clone());
    assert_eq!(
        state.request(
            &mut after_rotation,
            "frontend",
            json!({"action":"verify","bearer":replacement["bearer"],"required_scopes":[]}),
            1_081
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut after_rotation,
            "frontend",
            json!({"action":"refresh","refresh_token":replacement["refresh_token"]}),
            1_081
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut after_rotation,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":[]}),
            1_081
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut after_rotation,
            "frontend",
            json!({"action":"logout","refresh_token":rotated["refresh_token"]}),
            1_082
        )["ok"],
        true
    );
    let mut after_logout = Pipeline::new(configured_settings);
    assert_eq!(
        state.request(
            &mut after_logout,
            "frontend",
            json!({"action":"verify","bearer":rotated["bearer"],"required_scopes":[]}),
            1_083
        )["code"],
        "unauthorized"
    );
}

#[test]
fn imported_credential_replaces_stale_session_only_namespace_and_authenticates_after_restart() {
    let source = std::env::temp_dir().join(format!(
        "amaterasu-auth-import-consumer-source-{}",
        std::process::id()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "amaterasu-auth-import-consumer-state-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&source);
    let _ = std::fs::remove_dir_all(&state_root);
    std::fs::create_dir_all(source.join("credentials")).unwrap();

    let mut previous_state = StateHarness::new();
    let mut previous_pipeline = Pipeline::new(settings());
    previous_state.setup(&mut previous_pipeline);
    let previous_login = previous_state.request(
        &mut previous_pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    let namespace_path = crate::migration::namespace_path(&state_root);
    std::fs::create_dir_all(namespace_path.parent().unwrap()).unwrap();
    std::fs::write(
        &namespace_path,
        json!({"format_version":1,"revision":1,"values":{
            (SESSION_STATE_KEY): previous_state.session_stored
        }})
        .to_string(),
    )
    .unwrap();

    let password = "imported administrator password";
    let salt = "AbCdEfGhIjKlMnOp";
    std::fs::write(
        source.join("credentials/login.json"),
        json!({"username":"ImportedAdmin","password":crate::crypto::token_digest(&format!("{password}{salt}")),"salt":salt}).to_string(),
    )
    .unwrap();
    std::fs::write(
        source.join("sunshine.conf"),
        "credentials_file = credentials/login.json\n",
    )
    .unwrap();
    let import = crate::migration::import_legacy_authentication(&source).unwrap();
    assert_eq!(
        crate::migration::seed_authentication_state(&state_root, &import).unwrap(),
        crate::migration::AuthenticationSeedOutcome::Seeded
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(&namespace_path).unwrap()).unwrap();
    assert_eq!(saved["revision"], 2);
    assert_eq!(
        saved["values"][SESSION_STATE_KEY],
        previous_state.session_stored.as_ref().unwrap().clone()
    );

    let mut state = StateHarness::new();
    state.revision = saved["revision"].as_u64().unwrap();
    state.stored = Some(saved["values"][STATE_KEY].clone());
    state.session_stored = Some(saved["values"][SESSION_STATE_KEY].clone());
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"verify","bearer":previous_login["bearer"],"required_scopes":[]}),
            101,
        )["code"],
        "unauthorized"
    );
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"importedadmin","password":password,"remember_me":true}),
        102,
    );
    assert_eq!(login["ok"], true);
    assert_eq!(login["subject"], "ImportedAdmin");
    assert!(
        state.stored.as_ref().unwrap()["credential"]
            .get("legacy_salt")
            .is_none()
    );

    let mut restarted = Pipeline::new(settings());
    let verified = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"verify","bearer":login["bearer"],"required_scopes":["catalog.read"]}),
        103,
    );
    assert_eq!(verified["ok"], true);
    assert_eq!(verified["subject"], "ImportedAdmin");
    let refreshed = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        104,
    );
    assert_eq!(refreshed["ok"], true);
    assert_eq!(refreshed["subject"], "ImportedAdmin");
    let mut after_refresh = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut after_refresh,
            "frontend",
            json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":["catalog.read"]}),
            105,
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut after_refresh,
            "frontend",
            json!({"action":"logout","refresh_token":refreshed["refresh_token"]}),
            106,
        )["ok"],
        true
    );
    let mut after_logout = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut after_logout,
            "frontend",
            json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":[]}),
            107,
        )["code"],
        "unauthorized"
    );

    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(state_root).unwrap();
}

#[test]
fn scope_digest_mismatch_invalidates_hydrated_sessions() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    assert_eq!(session["ok"], true);

    let changed_settings = Settings {
        allowed_scopes: BTreeSet::from(["catalog.read".into()]),
        ..settings()
    };
    let mut restarted = Pipeline::new(changed_settings);
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":["catalog.read"]}),
            101,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "console",
            json!({"action":"list_sessions"}),
            101,
        )["sessions"],
        json!([])
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "observed policy mismatch must be durably pruned"
    );
    let mut restored_policy = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restored_policy,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":["catalog.read"]}),
            102,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn policy_generation_prunes_sessions_after_unobserved_revert_and_restart() {
    let mut state = StateHarness::new();
    let original_settings = Settings {
        authorization_policy_generation: 41,
        ..settings()
    };
    let mut original_policy = Pipeline::new(original_settings.clone());
    state.setup(&mut original_policy);
    let original_session = state.request(
        &mut original_policy,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    assert_eq!(original_session["ok"], true);
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["policy_generation"],
        41
    );

    // The changed policy is installed at generation 42, then restored at 43
    // before authentication handles another request. The session document
    // still contains generation 41, so restart cannot revive it by digest ABA.
    let _intermediate_policy = Settings {
        allowed_scopes: BTreeSet::from(["catalog.read".into()]),
        authorization_policy_generation: 42,
        ..original_settings.clone()
    };
    let restored_settings = Settings {
        authorization_policy_generation: 43,
        ..original_settings
    };
    let mut restarted = Pipeline::new(restored_settings.clone());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"refresh","refresh_token":original_session["refresh_token"]}),
            101,
        )["code"],
        "unauthorized"
    );
    let pruned = state.session_stored.as_ref().unwrap();
    assert_eq!(pruned["version"], SESSION_DOCUMENT_VERSION);
    assert_eq!(pruned["sessions"], json!([]));

    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":original_session["bearer"],"required_scopes":["catalog.read"]}),
            102,
        )["code"],
        "unauthorized"
    );
    let restored_login = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        103,
    );
    assert_eq!(restored_login["ok"], true);
    assert_eq!(
        restored_login["scopes"],
        json!(["catalog.read", "catalog.write"])
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["policy_generation"],
        43
    );
    let mut after_login_restart = Pipeline::new(restored_settings);
    assert_eq!(
        state.request(
            &mut after_login_restart,
            "frontend",
            json!({"action":"verify","bearer":restored_login["bearer"],"required_scopes":["catalog.write"]}),
            104,
        )["ok"],
        true
    );
}

#[test]
fn legacy_session_records_without_a_policy_generation_are_durably_pruned() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    let legacy_document = state.session_stored.as_mut().unwrap();
    legacy_document["version"] = json!(1);
    for record in legacy_document["sessions"].as_array_mut().unwrap() {
        record.as_object_mut().unwrap().remove("policy_generation");
        record
            .as_object_mut()
            .unwrap()
            .remove("ownership_context_id");
    }

    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap(),
        &json!({"version":SESSION_DOCUMENT_VERSION,"sessions":[]})
    );
}

#[test]
fn failed_prune_write_blocks_request_until_policy_mismatch_is_persisted() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    let changed_settings = Settings {
        allowed_scopes: BTreeSet::from(["catalog.read".into()]),
        ..settings()
    };
    let mut changed_policy = Pipeline::new(changed_settings);
    state.fail_session_write = true;
    assert_eq!(
        state.request(
            &mut changed_policy,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101,
        ),
        error("state_unavailable")
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    state.fail_session_write = false;
    assert_eq!(
        state.request(
            &mut changed_policy,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            102,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn observed_refresh_expiry_cannot_revive_after_restart_with_earlier_clock() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings_with_ttls(10, 20));
    state.setup(&mut pipeline);
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    let mut expired = Pipeline::new(settings_with_ttls(10, 20));
    assert_eq!(
        state.request(
            &mut expired,
            "console",
            json!({"action":"list_sessions"}),
            120
        )["sessions"],
        json!([])
    );
    let mut earlier_clock = Pipeline::new(settings_with_ttls(10, 20));
    assert_eq!(
        state.request(
            &mut earlier_clock,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn failed_login_and_refresh_commits_never_return_uncommitted_token_pairs() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);

    state.fail_write = true;
    let failed_login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        100,
    );
    assert_eq!(failed_login, error("state_unavailable"));
    assert!(state.session_stored.is_none());

    state.fail_write = false;
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple","remember_me":true}),
        101,
    );
    assert_eq!(login["ok"], true);

    state.fail_write = true;
    let failed_refresh = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        102,
    );
    assert_eq!(failed_refresh, error("state_unavailable"));

    state.fail_write = false;
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            103,
        )["ok"],
        true
    );
    let retried_refresh = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        103,
    );
    assert_eq!(retried_refresh["ok"], true);
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"][0]["session_id"],
        crate::crypto::token_digest(retried_refresh["bearer"].as_str().unwrap())
    );
}

#[test]
fn legacy_login_waits_for_session_sidecar_commit_after_credential_upgrade() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let salt = "AbCdEfGhIjKlMnOp";
    let password = "correct horse battery staple";
    state.stored = Some(json!({
        "credential": {
            "username": "Admin",
            "password_hash": crate::crypto::token_digest(&format!("{password}{salt}")),
            "legacy_salt": salt,
            "case_insensitive_username": true
        },
        "tokens": []
    }));
    state.revision = 1;
    state.fail_session_write = true;

    let failed_login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password}),
        100,
    );
    assert_eq!(failed_login, error("state_unavailable"));
    assert!(failed_login.get("bearer").is_none());
    assert!(
        state.stored.as_ref().unwrap()["credential"]
            .get("legacy_salt")
            .is_none()
    );
    assert!(state.session_stored.is_none());

    assert_eq!(state.revision, 2, "the credential write committed");

    state.fail_session_write = false;
    let committed_login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password}),
        101,
    );
    assert_eq!(committed_login["ok"], true);
    assert!(state.session_stored.is_some());
    assert_eq!(state.revision, 3);
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":committed_login["bearer"],"required_scopes":[]}),
            102,
        )["ok"],
        true
    );
}

#[test]
fn legacy_credential_upgrade_persists_session_at_shared_namespace_revision() {
    let mut state = StateHarness::new();
    let salt = "AbCdEfGhIjKlMnOp";
    let password = "correct horse battery staple";
    state.stored = Some(json!({
        "credential": {
            "username": "Admin",
            "password_hash": crate::crypto::token_digest(&format!("{password}{salt}")),
            "legacy_salt": salt,
            "case_insensitive_username": true
        },
        "tokens": []
    }));
    state.revision = 1;

    let mut pipeline = Pipeline::new(settings());
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password,"remember_me":true}),
        100,
    );
    assert_eq!(login["ok"], true);
    assert_eq!(state.revision, 3, "both keys share one revision");
    assert_eq!(state.session_revision, 1);
    assert!(
        state.stored.as_ref().unwrap()["credential"]
            .get("legacy_salt")
            .is_none()
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":["catalog.read"]}),
            101,
        )["ok"],
        true
    );
    let refreshed = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"refresh","refresh_token":login["refresh_token"]}),
        102,
    );
    assert_eq!(refreshed["ok"], true);
    assert_eq!(state.revision, 4);

    let mut restarted_again = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"verify","bearer":refreshed["bearer"],"required_scopes":["catalog.read"]}),
            103,
        )["ok"],
        true
    );
}

#[test]
fn restart_prunes_sessions_with_a_subject_different_from_the_imported_administrator() {
    let mut state = StateHarness::new();
    let salt = "AbCdEfGhIjKlMnOp";
    let password = "correct horse battery staple";
    state.stored = Some(json!({
        "credential": {
            "username": "Admin",
            "password_hash": crate::crypto::token_digest(&format!("{password}{salt}")),
            "legacy_salt": salt,
            "case_insensitive_username": true
        },
        "tokens": []
    }));
    state.revision = 1;

    let mut pipeline = Pipeline::new(settings());
    let login = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password}),
        100,
    );
    assert_eq!(login["subject"], "Admin");

    // An older module version recorded the login spelling as the session
    // subject. A restart must not restore that stale identity claim.
    state.session_stored.as_mut().unwrap()["sessions"][0]["subject"] = json!("ADMIN");
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":login["bearer"],"required_scopes":[]}),
            101,
        ),
        error("unauthorized")
    );
    assert_eq!(
        state.session_stored.as_ref().unwrap()["sessions"],
        json!([])
    );

    let fresh = state.request(
        &mut restarted,
        "frontend",
        json!({"action":"login","username":"ADMIN","password":password}),
        102,
    );
    assert_eq!(fresh["subject"], "Admin");
    let mut restarted_again = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted_again,
            "frontend",
            json!({"action":"verify","bearer":fresh["bearer"],"required_scopes":[]}),
            103,
        )["subject"],
        "Admin"
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
    state.revision = 1;

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
    let credential = &state.stored.as_ref().unwrap()["accounts"][0];
    assert!(state.stored.as_ref().unwrap()["credential"].is_null());
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
        child_request:50,
        stage: Stage::Loading(serde_json::from_value(json!({"action":"setup","username":"admin","password":"correct horse battery staple"})).unwrap()),
        phase: PendingPhase::Waiting,
    });
    let mut completed = false;
    pipeline
        .tick_at(100, |command| {
            Ok(match command {
                Command::TakeResult { request: 50 } => Response::error("cancelled", "parent ended"),
                Command::ReleaseResult { request: 50 } => Response::Ok,
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
fn logout_revokes_persistently_and_active_sessions_survive_restart() {
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
        )["ok"],
        true
    );
    assert_eq!(state.writes, 5);
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
    assert!(
        state.session_stored.as_ref().unwrap()["sessions"]
            .as_array()
            .is_none_or(|sessions| sessions.is_empty())
    );
    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":session["bearer"],"required_scopes":[]}),
            101,
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"refresh","refresh_token":session["refresh_token"]}),
            101,
        )["code"],
        "unauthorized"
    );
}

#[test]
fn enrollment_requires_local_issuance_and_consumes_code_with_credential() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let generate = json!({"action":"generate_enrollment_code"});
    assert_eq!(
        state.request(&mut pipeline, "amaterasu.management", generate.clone(), 100)["code"],
        "forbidden"
    );
    let issued = state.request(&mut pipeline, "amaterasu.console", generate, 100);
    let code = issued["code"].as_str().unwrap().to_owned();
    assert_eq!(issued["expires_at"], 700);
    assert!(!state.stored.as_ref().unwrap().to_string().contains(&code));
    let enroll = json!({"action":"enroll_first_admin","code":code,"username":"admin","password":"correct horse battery staple"});
    assert_eq!(
        state.request(&mut pipeline, "frontend", enroll.clone(), 101)["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(&mut pipeline, "amaterasu.management", enroll.clone(), 101)["ok"],
        true
    );
    assert!(state.stored.as_ref().unwrap().get("enrollment").is_none());
    assert!(state.stored.as_ref().unwrap()["credential"].is_null());
    assert_eq!(
        state.stored.as_ref().unwrap()["accounts"][0]["role"],
        "administrator"
    );
    assert_eq!(
        state.request(&mut pipeline, "amaterasu.management", enroll, 102)["code"],
        "already_initialized"
    );
}

#[test]
fn enrollment_code_expires_and_reissuance_invalidates_previous_code() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    let first = state.request(
        &mut pipeline,
        "amaterasu.console",
        json!({"action":"generate_enrollment_code"}),
        100,
    );
    let second = state.request(
        &mut pipeline,
        "amaterasu.console",
        json!({"action":"generate_enrollment_code"}),
        101,
    );
    let request = |code: &Value| json!({"action":"enroll_first_admin","code":code,"username":"admin","password":"correct horse battery staple"});
    assert_eq!(
        state.request(
            &mut pipeline,
            "amaterasu.management",
            request(&first["code"]),
            102
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "amaterasu.management",
            request(&second["code"]),
            701
        )["code"],
        "unauthorized"
    );
}

#[test]
fn account_lifecycle_enforces_roles_and_per_user_revocation_across_restart() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let login = |name: &str| json!({"action":"login","username":name,"password":"correct horse battery staple"});
    let admin = state.request(&mut pipeline, "frontend", login("admin"), 100);
    let admin_bearer = admin["bearer"].clone();
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"create_account","bearer":admin_bearer,"username":"operator","password":"correct horse battery staple","role":"operator"}),101)["ok"], true);
    let operator = state.request(&mut pipeline, "frontend", login("operator"), 101);
    assert_eq!(operator["role"], "operator");
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"verify","bearer":operator["bearer"],"required_scopes":["catalog.read"]}),101)["ok"], true);
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"verify","bearer":operator["bearer"],"required_scopes":["catalog.write"]}),101)["code"], "unauthorized");
    assert_eq!(
        state.request(
            &mut pipeline,
            "frontend",
            json!({"action":"list_accounts","bearer":operator["bearer"]}),
            101
        )["code"],
        "forbidden"
    );
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"update_account","bearer":admin["bearer"],"username":"operator","role":"viewer","enabled":true}),102)["ok"], true);
    let mut restarted = Pipeline::new(settings());
    state.fail_session_write = true;
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":operator["bearer"],"required_scopes":[]}),
            103
        )["code"],
        "state_unavailable"
    );
    state.fail_session_write = false;
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"refresh","refresh_token":operator["refresh_token"]}),
            104
        )["code"],
        "unauthorized"
    );
    // Updating another user must preserve the administrator session.
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"verify","bearer":admin["bearer"],"required_scopes":[]}),
            104
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut restarted,
            "frontend",
            json!({"action":"delete_account","bearer":admin["bearer"],"username":"admin"}),
            104
        )["code"],
        "last_administrator"
    );
}

#[test]
fn trusted_scope_policy_grants_only_configured_roles_and_invalidates_sessions() {
    let mut configured = settings();
    configured.allowed_scopes.insert("extension.control".into());
    configured
        .scope_minimum_roles
        .insert("extension.control".into(), crate::accounts::Role::Operator);
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(configured.clone());
    state.setup(&mut pipeline);
    let login = |username: &str| json!({"action":"login","username":username,"password":"correct horse battery staple"});
    let admin = state.request(&mut pipeline, "frontend", login("admin"), 100);
    for role in ["operator", "viewer"] {
        assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"create_account","bearer":admin["bearer"],"username":role,"password":"correct horse battery staple","role":role}),100)["ok"],true);
    }
    let operator = state.request(&mut pipeline, "frontend", login("operator"), 101);
    let viewer = state.request(&mut pipeline, "frontend", login("viewer"), 101);
    let verify = |token: &Value| json!({"action":"verify","bearer":token,"required_scopes":["extension.control"]});
    assert_eq!(
        state.request(&mut pipeline, "frontend", verify(&operator["bearer"]), 101)["ok"],
        true
    );
    assert_eq!(
        state.request(&mut pipeline, "frontend", verify(&viewer["bearer"]), 101)["code"],
        "unauthorized"
    );
    configured.scope_minimum_roles.clear();
    configured.authorization_policy_generation += 1;
    let mut restarted = Pipeline::new(configured);
    assert_eq!(
        state.request(&mut restarted, "frontend", verify(&operator["bearer"]), 102)["code"],
        "unauthorized"
    );
}

#[test]
fn account_settings_require_current_administrator_session() {
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(settings());
    state.setup(&mut pipeline);
    let login = |username: &str| json!({"action":"login","username":username,"password":"correct horse battery staple"});
    let admin = state.request(&mut pipeline, "frontend", login("admin"), 100);
    assert_eq!(state.request(&mut pipeline,"frontend",json!({"action":"create_account","bearer":admin["bearer"],"username":"viewer","password":"correct horse battery staple","role":"viewer"}),100)["ok"],true);
    let viewer = state.request(&mut pipeline, "frontend", login("viewer"), 101);
    let get = |token: &Value| json!({"action":"account_settings_get","bearer":token});
    assert_eq!(
        state.request(&mut pipeline, "frontend", get(&viewer["bearer"]), 101)["code"],
        "forbidden"
    );
    let settings_response = state.request(&mut pipeline, "frontend", get(&admin["bearer"]), 101);
    assert_eq!(settings_response["ok"], true);
    let update = |token: &Value| json!({"action":"account_settings_update","bearer":token,"session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":604800,"expected_revision":0});
    assert_eq!(
        state.request(&mut pipeline, "frontend", update(&viewer["bearer"]), 101)["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(&mut pipeline, "frontend", update(&admin["bearer"]), 101)["ok"],
        true
    );
}

#[test]
fn legacy_verifier_principal_has_no_administrator_operations() {
    let mut policy = settings();
    policy
        .legacy_verifier_principals
        .insert("amaterasu.management".into());
    let mut state = StateHarness::new();
    let mut pipeline = Pipeline::new(policy);
    state.setup(&mut pipeline);
    assert_eq!(
        state.request(
            &mut pipeline,
            "amaterasu.management",
            json!({"action":"settings_get"}),
            100
        )["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "amaterasu.management",
            json!({"action":"change_password","password":"correct horse battery staple"}),
            100
        )["code"],
        "forbidden"
    );
    let session = state.request(
        &mut pipeline,
        "frontend",
        json!({"action":"login","username":"admin","password":"correct horse battery staple"}),
        100,
    );
    assert_eq!(state.request(&mut pipeline,"amaterasu.management",json!({"action":"verify","bearer":session["bearer"],"required_scopes":["catalog.read"],"legacy_request":{"path":"/api/catalog","method":"GET"}}),100)["ok"],true);
}

#[test]
fn management_login_backstop_recovers_rapidly_and_preserves_other_caller_limits() {
    use crate::engine::Engine;
    use crate::protocol::Operation;
    let operation = Operation::Login {
        username: "user".into(),
        password: "wrong".into(),
        remember_me: false,
    };
    let mut engine = Engine::new(settings());
    // Management applies its peer/account limits before forwarding these calls;
    // authentication sees only the trusted module principal, never peer input.
    for _ in 0..20 {
        assert_eq!(
            engine.admit("amaterasu.management", &operation, 100),
            Ok(())
        );
    }
    assert_eq!(
        engine.admit("amaterasu.management", &operation, 100),
        Err("rate_limited")
    );
    // Even if two peers consumed the burst, a later client is not locked out
    // for the rest of the minute. Refill remains bounded at two per second.
    assert_eq!(
        engine.admit("amaterasu.management", &operation, 101),
        Ok(())
    );
    assert_eq!(
        engine.admit("amaterasu.management", &operation, 101),
        Ok(())
    );
    assert_eq!(
        engine.admit("amaterasu.management", &operation, 101),
        Err("rate_limited")
    );
    for now in 102..112 {
        for _ in 0..2 {
            assert_eq!(
                engine.admit("amaterasu.management", &operation, now),
                Ok(())
            );
        }
    }
    // Independent callers retain both the five-per-principal and shared 20
    // limits, and cannot use the management budget to bypass them.
    for caller in ["one", "two", "three", "four"] {
        for _ in 0..5 {
            assert_eq!(engine.admit(caller, &operation, 112), Ok(()));
        }
        assert_eq!(engine.admit(caller, &operation, 112), Err("rate_limited"));
    }
    assert_eq!(engine.admit("five", &operation, 112), Err("rate_limited"));
    assert_eq!(engine.admit("five", &operation, 172), Ok(()));
    // Restart clears in-memory ledgers; downstream per-peer limits remain
    // independently enforced by management for its lifetime.
    let mut restarted = Engine::new(settings());
    for _ in 0..20 {
        assert_eq!(
            restarted.admit("amaterasu.management", &operation, 172),
            Ok(())
        );
    }
    assert_eq!(
        restarted.admit("amaterasu.management", &operation, 172),
        Err("rate_limited")
    );
}
