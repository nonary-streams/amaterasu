use super::*;
use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1v15::{Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
    signature::{RandomizedSigner, SignatureEncoding, Verifier},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use x509_cert::{
    Certificate,
    der::{DecodePem, Encode},
};

const CLIENT_CERT: &str = include_str!("../../tests/fixtures/client-cert.pem");
const CLIENT_KEY: &str = include_str!("../../tests/fixtures/client-key.pem");
const HOST_CERT: &str = include_str!("../../tests/fixtures/host-cert.pem");
const HOST_KEY: &str = include_str!("../../tests/fixtures/host-key.pem");
pub(super) fn now() -> u64 {
    Certificate::from_pem(HOST_CERT)
        .unwrap()
        .tbs_certificate
        .validity
        .not_before
        .to_unix_duration()
        .as_secs()
        .max(
            Certificate::from_pem(CLIENT_CERT)
                .unwrap()
                .tbs_certificate
                .validity
                .not_before
                .to_unix_duration()
                .as_secs(),
        )
        + 1
}
fn settings() -> Settings {
    Settings {
        administrator_principals: BTreeSet::from(["console".into()]),
        transport_principals: BTreeSet::from(["nvhttp".into()]),
        effect_authority: None,
    }
}

pub(super) struct Harness {
    document: Option<Value>,
    revision: u64,
    incoming: Option<Response>,
    state_reply: Option<Response>,
    recovery_reply: Option<Response>,
    output: Option<Value>,
    writes: usize,
    fail_write: bool,
    conflict_next_write: bool,
    cancelled: bool,
    check_cancelled: bool,
    tray_notifications: usize,
}
impl Harness {
    fn new() -> Self {
        Self {
            document: None,
            revision: 0,
            incoming: None,
            state_reply: None,
            recovery_reply: None,
            output: None,
            writes: 0,
            fail_write: false,
            conflict_next_write: false,
            cancelled: false,
            check_cancelled: false,
            tray_notifications: 0,
        }
    }
    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
            Command::CheckRequest { request: 8 } => Response::Ok,
            Command::CheckRequest { request: 7 } => {
                if self.check_cancelled {
                    Response::error("request_ended", "cancelled")
                } else {
                    Response::Ok
                }
            }
            Command::Poll {} => self.incoming.take().unwrap_or(Response::Pending),
            Command::Request {
                service,
                parent,
                payload,
                ..
            } => {
                if service == "amaterasu.system-tray/1" {
                    assert_eq!(parent, None);
                    assert_eq!(payload, json!({"action":"notify_pairing"}));
                    self.tray_notifications += 1;
                    return Ok(Response::Accepted { id: 70 });
                }
                assert_eq!(service, STATE_SERVICE);
                if parent.is_none() {
                    assert_eq!(payload["action"], "get");
                    self.recovery_reply = Some(Response::Result {
                        payload: json!({"ok":true,"revision":self.revision,"found":self.document.is_some(),"value":self.document}),
                    });
                    return Ok(Response::Accepted { id: 51 });
                }
                assert_eq!(parent, Some(7));
                assert_eq!(payload["key"], STATE_KEY);
                let response = if payload["action"] == "get" {
                    json!({"ok":true,"revision":self.revision,"found":self.document.is_some(),"value":self.document})
                } else {
                    self.writes += 1;
                    if self.conflict_next_write {
                        self.conflict_next_write = false;
                        self.revision += 1;
                        let clients = self.document.as_mut().unwrap()["clients"]
                            .as_array_mut()
                            .unwrap();
                        let client = clients.first_mut().unwrap();
                        if !client["config_overrides"].is_object() {
                            client["config_overrides"] = json!({});
                        }
                        client["config_overrides"]["fec_percentage"] = json!(73);
                        json!({
                            "ok": false,
                            "error": {
                                "code": "revision_conflict",
                                "message": "another administrator committed first",
                                "current_revision": self.revision
                            }
                        })
                    } else if payload["expected_revision"] != self.revision {
                        json!({
                            "ok": false,
                            "error": {
                                "code": "revision_conflict",
                                "message": "stale revision",
                                "current_revision": self.revision
                            }
                        })
                    } else if self.fail_write {
                        json!({"ok":false,"error":{"message":"sensitive-state-detail"}})
                    } else {
                        self.document = Some(payload["value"].clone());
                        self.revision += 1;
                        json!({"ok":true,"revision":self.revision})
                    }
                };
                self.state_reply = Some(Response::Result { payload: response });
                Response::Accepted { id: 50 }
            }
            Command::TakeResult { request: 51 } => {
                self.recovery_reply.take().unwrap_or(Response::Pending)
            }
            Command::TakeResult { request: 50 } => {
                self.state_reply.take().unwrap_or(Response::Pending)
            }
            Command::TakeResult { request: 70 } => Response::Result {
                payload: json!({"ok":true}),
            },
            Command::ReleaseResult { request: 70 } => Response::Ok,
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
            _ => panic!("unexpected pairing command"),
        })
    }
    fn enqueue(&mut self, caller: &str, payload: Value) {
        self.output = None;
        self.incoming = Some(Response::Request {
            id: 7,
            caller: caller.into(),
            initiator: "caller-supplied-authority-is-not-used".into(),
            service: SERVICE.into(),
            remaining_ms: 30_000,
            payload,
        });
    }
    fn request(
        &mut self,
        pipeline: &mut Pipeline,
        caller: &str,
        payload: Value,
        time: u64,
    ) -> Value {
        self.enqueue(caller, payload);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            pipeline
                .tick_at(time, |command| self.send(command))
                .unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("pairing request stalled")
    }
    fn initialize(&mut self, pipeline: &mut Pipeline) {
        let result=self.request(pipeline,"console",json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":HOST_KEY}),now());
        assert_eq!(result["ok"], true, "{result}");
        assert!(result.get("private_key_pem").is_none());
    }
}

// Independent client-side transcript, following Moonlight Qt's documented wire
// operations. It does not call the host crypto/transcript helpers. Certificates
// and private keys were generated independently by the OpenSSL CLI.
struct ClientTranscript {
    key: [u8; 16],
    challenge: [u8; 16],
    secret: [u8; 16],
    server_hash: Vec<u8>,
    host_certificate: Certificate,
}
fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect()
}
fn aes_blocks(key: &[u8; 16], bytes: &[u8], encrypt: bool) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut result = bytes.to_vec();
    for block in result.as_chunks_mut::<16>().0 {
        let block = GenericArray::from_mut_slice(block);
        if encrypt {
            cipher.encrypt_block(block)
        } else {
            cipher.decrypt_block(block)
        }
    }
    result
}
impl ClientTranscript {
    fn new(pin: &str, host_pem: &str) -> Self {
        let mut material: Vec<u8> = (0..16).collect();
        material.extend_from_slice(pin.as_bytes());
        let hash = Sha256::digest(material);
        Self {
            key: hash[..16].try_into().unwrap(),
            challenge: [0x11; 16],
            secret: [0x22; 16],
            server_hash: Vec::new(),
            host_certificate: Certificate::from_pem(host_pem).unwrap(),
        }
    }
    fn challenge(&self) -> String {
        encode(&aes_blocks(&self.key, &self.challenge, true))
    }
    fn respond(&mut self, encrypted: &str) -> String {
        let plaintext = aes_blocks(&self.key, &decode(encrypted), false);
        assert_eq!(plaintext.len(), 48);
        self.server_hash = plaintext[..32].to_vec();
        let client = Certificate::from_pem(CLIENT_CERT).unwrap();
        let mut hash = Sha256::new();
        hash.update(&plaintext[32..]);
        hash.update(client.signature.as_bytes().unwrap());
        hash.update(self.secret);
        encode(&aes_blocks(&self.key, &hash.finalize(), true))
    }
    fn validate_server(&self, proof: &str) -> bool {
        let proof = decode(proof);
        let public = RsaPublicKey::from_public_key_der(
            &self
                .host_certificate
                .tbs_certificate
                .subject_public_key_info
                .to_der()
                .unwrap(),
        )
        .unwrap();
        VerifyingKey::<Sha256>::new(public)
            .verify(&proof[..16], &Signature::try_from(&proof[16..]).unwrap())
            .unwrap();
        let mut hash = Sha256::new();
        hash.update(self.challenge);
        hash.update(self.host_certificate.signature.as_bytes().unwrap());
        hash.update(&proof[..16]);
        hash.finalize().as_slice() == self.server_hash
    }
    fn finish(&self) -> String {
        let private = RsaPrivateKey::from_pkcs8_pem(CLIENT_KEY).unwrap();
        let signature =
            SigningKey::<Sha256>::new(private).sign_with_rng(&mut rand_core::OsRng, &self.secret);
        let mut proof = self.secret.to_vec();
        proof.extend_from_slice(&signature.to_vec());
        encode(&proof)
    }
}
fn begin(state: &mut Harness, pipeline: &mut Pipeline, client_id: &str, time: u64) -> String {
    let response=state.request(pipeline,"nvhttp",json!({"action":"begin","client_id":client_id,"name":"Moonlight test","client_certificate_pem":CLIENT_CERT,"salt":encode(&(0..16).collect::<Vec<_>>())}),time);
    assert_eq!(response["ok"], true, "{response}");
    response["pairing_id"].as_str().unwrap().into()
}
fn exchange(
    state: &mut Harness,
    pipeline: &mut Pipeline,
    id: &str,
    client_pin: &str,
    time: u64,
) -> ClientTranscript {
    assert_eq!(
        state.request(
            pipeline,
            "console",
            json!({"action":"approve_pin","pairing_id":id,"pin":"1234"}),
            time
        )["ok"],
        true
    );
    let cert = state.request(
        pipeline,
        "nvhttp",
        json!({"action":"get_server_cert","pairing_id":id}),
        time,
    );
    let host_pem = String::from_utf8(decode(cert["plaincert"].as_str().unwrap())).unwrap();
    let mut client = ClientTranscript::new(client_pin, &host_pem);
    let challenge = state.request(
        pipeline,
        "nvhttp",
        json!({"action":"client_challenge","pairing_id":id,"clientchallenge":client.challenge()}),
        time,
    );
    let response = client.respond(challenge["challengeresponse"].as_str().unwrap());
    let proof=state.request(pipeline,"nvhttp",json!({"action":"server_challenge_response","pairing_id":id,"serverchallengeresp":response}),time);
    assert_eq!(
        client.validate_server(proof["pairingsecret"].as_str().unwrap()),
        client_pin == "1234"
    );
    client
}

#[test]
fn full_moonlight_gen7_transcript_persists_certificate_before_success() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    let id = begin(&mut state, &mut pipeline, "moonlight", now());
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"get_server_cert","pairing_id":id}),
            now()
        )["pending"],
        true
    );
    assert_eq!(state.tray_notifications, 1);
    let client = exchange(&mut state, &mut pipeline, &id, "1234", now());
    assert_eq!(state.tray_notifications, 1);
    state.enqueue("nvhttp",json!({"action":"client_pairing_secret","pairing_id":id,"clientpairingsecret":client.finish()}));
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    assert!(state.output.is_none());
    assert_eq!(state.writes, 2);
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    assert_eq!(state.output.as_ref().unwrap()["paired"], 1);
    assert_eq!(state.tray_notifications, 1);
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    assert_eq!(state.tray_notifications, 1);
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["paired"],
        1
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "untrusted",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["code"],
        "forbidden"
    );
    let listed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    let certificate_id = listed["clients"][0]["certificate_id"].clone();
    assert!(!listed.to_string().contains("PRIVATE KEY"));
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"unpair","certificate_id":certificate_id}),
            now()
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["code"],
        "not_paired"
    );
}

#[test]
fn bad_pin_signature_tampering_and_replay_never_pair() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    for (index, pin) in ["0000", "1234"].iter().enumerate() {
        let id = begin(&mut state, &mut pipeline, &format!("client-{index}"), now());
        let client = exchange(&mut state, &mut pipeline, &id, pin, now());
        let mut proof = decode(&client.finish());
        if *pin == "1234" {
            proof[20] ^= 1;
        }
        let request = json!({"action":"client_pairing_secret","pairing_id":id,"clientpairingsecret":encode(&proof)});
        assert_eq!(
            state.request(&mut pipeline, "nvhttp", request.clone(), now())["code"],
            "pairing_failed"
        );
        assert_eq!(
            state.request(&mut pipeline, "nvhttp", request, now())["code"],
            "pairing_not_found"
        );
    }
    assert!(
        state.document.as_ref().unwrap()["clients"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pin_approval_owner_isolation_phase_order_expiry_and_bounds() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    let id = begin(&mut state, &mut pipeline, "client", now());
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"approve_pin","pairing_id":id,"pin":"1234"}),
            now()
        )["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "foreign",
            json!({"action":"get_server_cert","pairing_id":id}),
            now()
        )["code"],
        "pairing_not_found"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"client_challenge","pairing_id":id,"clientchallenge":"00".repeat(16)}),
            now()
        )["code"],
        "invalid_phase"
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"approve_pin","pairing_id":id,"pin":"1234"}),
            now()
        )["code"],
        "pairing_not_found"
    );
    let id = begin(&mut state, &mut pipeline, "expiry", now());
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"approve_pin","pairing_id":id,"pin":"1234"}),
            now() + 120
        )["code"],
        "pairing_not_found"
    );
    for index in 0..8 {
        begin(
            &mut state,
            &mut pipeline,
            &format!("bounded-{index}"),
            now() + 120,
        );
    }
    assert_eq!(state.request(&mut pipeline,"nvhttp",json!({"action":"begin","client_id":"overflow","name":"test","client_certificate_pem":CLIENT_CERT,"salt":"00".repeat(16)}),now()+120)["code"],"rate_limited");
}

#[test]
fn identity_import_rejects_wrong_key_and_failed_commit_has_no_success() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    let bad=state.request(&mut pipeline,"console",json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":CLIENT_KEY}),now());
    assert_eq!(bad["code"], "invalid_identity");
    assert!(state.document.is_none());
    state.fail_write = true;
    let failed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":HOST_KEY}),
        now(),
    );
    assert_eq!(failed, error("state_unavailable"));
    assert!(state.document.is_none());
    state.fail_write = false;
    state.cancelled = true;
    state.initialize(&mut pipeline);
    state.cancelled = false;
    let mut pipeline = Pipeline::new(settings());
    let identity = state.request(&mut pipeline, "nvhttp", json!({"action":"identity"}), now());
    assert_eq!(identity["certificate_pem"], HOST_CERT);
    assert!(!identity.to_string().contains("PRIVATE KEY"));
}

#[test]
fn client_challenge_matches_independent_openssl_vector() {
    let client = ClientTranscript::new("1234", HOST_CERT);
    assert_eq!(encode(&client.key), "bad0b4f7cae08eb7c1b5acc763a8ed25");
    assert_eq!(client.challenge(), "aad45a6506af0973b77bcc7262fe36d5");
}

#[test]
fn generated_host_identity_is_self_signed_and_matches_its_private_key() {
    let identity = crate::crypto::generate_identity().unwrap();
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    crate::crypto::validate_identity(&identity, time).unwrap();
    assert!(crate::protocol::valid_host_id(&identity.host_id));
    let certificate = Certificate::from_pem(&identity.certificate_pem).unwrap();
    let public = RsaPublicKey::from_public_key_der(
        &certificate
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .unwrap(),
    )
    .unwrap();
    VerifyingKey::<Sha256>::new(public)
        .verify(
            &certificate.tbs_certificate.to_der().unwrap(),
            &Signature::try_from(certificate.signature.as_bytes().unwrap()).unwrap(),
        )
        .unwrap();
}

#[test]
fn initialize_validates_generated_identity_after_key_generation() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    let request_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(60);

    let response = state.request(
        &mut pipeline,
        "console",
        json!({"action":"initialize"}),
        request_time,
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(state.writes, 1);
    let persisted: Document = serde_json::from_value(state.document.unwrap()).unwrap();
    let current = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(persisted.validate(current).is_ok());
}

#[test]
fn malformed_certificate_and_expired_certificate_fail_closed() {
    let mut certificate = Certificate::from_pem(CLIENT_CERT).unwrap();
    let mut signature = certificate.signature.as_bytes().unwrap().to_vec();
    signature[0] ^= 1;
    certificate.signature = x509_cert::der::asn1::BitString::from_bytes(&signature).unwrap();
    use x509_cert::der::EncodePem;
    let changed = certificate.to_pem(rsa::pkcs8::LineEnding::LF).unwrap();
    assert!(crate::crypto::parse_certificate(&changed, Some(now())).is_err());
    let expired = certificate
        .tbs_certificate
        .validity
        .not_after
        .to_unix_duration()
        .as_secs();
    assert!(crate::crypto::parse_certificate(CLIENT_CERT, Some(expired)).is_err());
    assert!(crate::crypto::parse_certificate("not a certificate", Some(now())).is_err());
}

#[test]
fn authorized_import_preserves_host_identifier_and_client_certificate() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    let host_id = "12345678-1234-4234-8234-123456789abc";
    let imported=state.request(&mut pipeline,"console",json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":HOST_KEY,"host_id":host_id}),now());
    assert_eq!(imported["host_id"], host_id);
    let import_client = json!({"action":"import_client","client_id":"existing-moonlight","name":"Migrated client","certificate_pem":CLIENT_CERT});
    assert_eq!(
        state.request(&mut pipeline, "nvhttp", import_client.clone(), now())["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(&mut pipeline, "console", import_client, now())["ok"],
        true
    );
    let mut pipeline = Pipeline::new(settings());
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["paired"],
        1
    );
}

#[test]
fn failed_pairing_commit_does_not_authorize_and_completed_phase_is_consumed() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    let id = begin(&mut state, &mut pipeline, "fail-commit", now());
    let client = exchange(&mut state, &mut pipeline, &id, "1234", now());
    let finish = json!({"action":"client_pairing_secret","pairing_id":id,"clientpairingsecret":client.finish()});
    state.fail_write = true;
    assert_eq!(
        state.request(&mut pipeline, "nvhttp", finish.clone(), now())["code"],
        "state_unavailable"
    );
    state.fail_write = false;
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["code"],
        "not_paired"
    );
    assert_eq!(
        state.request(&mut pipeline, "nvhttp", finish, now())["code"],
        "pairing_not_found"
    );
}

#[test]
fn cancelled_pin_approval_is_checked_before_mutating_the_transcript() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    let id = begin(&mut state, &mut pipeline, "cancelled-pin", now());
    state.enqueue(
        "console",
        json!({"action":"approve_pin","pairing_id":id,"pin":"1234"}),
    );
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    state.check_cancelled = true;
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    assert_eq!(state.output.as_ref().unwrap()["code"], "request_ended");
    state.check_cancelled = false;
    assert_eq!(
        state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"get_server_cert","pairing_id":id}),
            now()
        )["pending"],
        true
    );
}

#[test]
fn client_config_update_is_durable_and_stale_admin_revision_cannot_overwrite_it() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action": "import_client",
                "client_id": "living-room",
                "name": "Living room",
                "certificate_pem": CLIENT_CERT
            }),
            now()
        )["ok"],
        true
    );
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action": "import_client",
                "client_id": "living-room",
                "name": "Second certificate with duplicate client id",
                "certificate_pem": HOST_CERT
            }),
            now()
        )["ok"],
        true
    );

    let initial = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    let certificate_id = initial["clients"][0]["certificate_id"].as_str().unwrap();
    assert_eq!(initial["clients"][1]["client_id"], "living-room");
    let initial_revision = initial["revision"].as_u64().unwrap();
    assert!(initial_revision > 0);
    let writes_before_transport_attempt = state.writes;
    let transport_write = state.request(
        &mut pipeline,
        "nvhttp",
        json!({
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": initial_revision,
            "config_overrides": {"fec_percentage": 61}
        }),
        now(),
    );
    assert_eq!(transport_write["code"], "forbidden");
    assert_eq!(state.writes, writes_before_transport_attempt);

    let invalid_stream_override = state.request(
        &mut pipeline,
        "console",
        json!({
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": initial_revision,
            "config_overrides": {"minimum_fps_target":-1.0}
        }),
        now(),
    );
    assert_eq!(invalid_stream_override["code"], "invalid_request");
    assert_eq!(state.writes, writes_before_transport_attempt);
    let invalid_display_override = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":certificate_id,
            "expected_revision":initial_revision,
            "config_overrides":{"dd_configuration_option":"unknown"}
        }),
        now(),
    );
    assert_eq!(invalid_display_override["code"], "invalid_request");
    assert_eq!(state.writes, writes_before_transport_attempt);
    let unchanged_revision = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(unchanged_revision["revision"], initial_revision);
    assert_eq!(state.writes, writes_before_transport_attempt);

    let write = state.request(
        &mut pipeline,
        "console",
        json!({
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": initial_revision,
            "config_overrides": {
                "fec_percentage":47,
                "max_bitrate":-15000,
                "minimum_fps_target":0.0,
                "video_max_batch_size_kb":0,
                "dd_mode_remapping":{"mixed":[]},
                "dd_resolution_option":"manual",
                "dd_manual_resolution":"1920x1080",
                "dd_refresh_rate_option":"prefer_highest",
                "dd_hdr_option":"disabled",
                "dd_configuration_option":"verify_only",
                "stream_audio":false,
                "native_pen_touch":false,
                "nvenc_spatial_aq":false,
                "audio_sink_capture_only":false,
                "back_button_timeout":-1,
                "ds4_back_as_touchpad_click":false
            }
        }),
        now(),
    );
    assert_eq!(write["ok"], true);

    let certificate_readback = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(certificate_readback["paired"], 1);
    assert_eq!(
        certificate_readback["config_overrides"]["fec_percentage"],
        47
    );
    assert_eq!(
        certificate_readback["config_overrides"]["max_bitrate"],
        -15000
    );
    assert_eq!(
        certificate_readback["config_overrides"]["minimum_fps_target"],
        0.0
    );
    assert_eq!(
        certificate_readback["config_overrides"]["video_max_batch_size_kb"],
        0
    );
    assert_eq!(
        certificate_readback["config_overrides"]["dd_mode_remapping"],
        json!({"mixed":[]})
    );
    assert_eq!(
        certificate_readback["config_overrides"]["dd_configuration_option"],
        "verify_only"
    );
    assert_eq!(
        certificate_readback["config_overrides"]["stream_audio"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["native_pen_touch"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["nvenc_spatial_aq"],
        json!(false)
    );
    assert_eq!(
        certificate_readback["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(
        certificate_readback["config_overrides"]["ds4_back_as_touchpad_click"],
        false
    );
    let duplicate_client_id_readback = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":HOST_CERT}),
        now(),
    );
    assert_eq!(duplicate_client_id_readback["client_id"], "living-room");
    assert_eq!(duplicate_client_id_readback["config_overrides"], json!({}));
    let persisted = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert!(persisted["revision"].as_u64().unwrap() > initial_revision);
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["fec_percentage"],
        47
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["max_bitrate"],
        -15000
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["minimum_fps_target"],
        0.0
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["video_max_batch_size_kb"],
        0
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["dd_manual_resolution"],
        "1920x1080"
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["dd_refresh_rate_option"],
        "prefer_highest"
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["stream_audio"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["native_pen_touch"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["nvenc_spatial_aq"],
        json!(false)
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["back_button_timeout"],
        -1
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["ds4_back_as_touchpad_click"],
        false
    );

    // A second administrator holding the same pre-write revision must reload
    // and receive a conflict rather than silently replacing the committed value.
    let writes_after_commit = state.writes;
    let stale = state.request(
        &mut pipeline,
        "console",
        json!({
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": initial_revision,
            "config_overrides": {"fec_percentage": 61}
        }),
        now(),
    );
    assert_eq!(stale["code"], "revision_conflict");
    assert_eq!(stale["current_revision"], persisted["revision"]);
    assert_eq!(state.writes, writes_after_commit);
    let unchanged = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(unchanged["config_overrides"]["fec_percentage"], 47);
    assert_eq!(unchanged["config_overrides"]["max_bitrate"], -15000);
    assert_eq!(unchanged["config_overrides"]["minimum_fps_target"], 0.0);
    assert_eq!(unchanged["config_overrides"]["video_max_batch_size_kb"], 0);
    assert_eq!(
        unchanged["config_overrides"]["dd_configuration_option"],
        "verify_only"
    );
    assert_eq!(unchanged["config_overrides"]["stream_audio"], false);
    assert_eq!(unchanged["config_overrides"]["native_pen_touch"], false);
    assert_eq!(
        unchanged["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(unchanged["config_overrides"]["back_button_timeout"], -1);
    assert_eq!(
        unchanged["config_overrides"]["ds4_back_as_touchpad_click"],
        false
    );
}

#[test]
fn old_paired_fec_override_document_loads_without_audio_migration() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action":"import_client",
                "client_id":"legacy-client",
                "name":"Legacy client",
                "certificate_pem":CLIENT_CERT
            }),
            now()
        )["ok"],
        true
    );
    state.document.as_mut().unwrap()["clients"][0]["config_overrides"] =
        json!({"fec_percentage":73});
    let writes_before_read = state.writes;

    let result = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(result["paired"], 1);
    assert_eq!(result["config_overrides"]["fec_percentage"], 73);
    assert!(result["config_overrides"].get("max_bitrate").is_none());
    assert!(
        result["config_overrides"]
            .get("minimum_fps_target")
            .is_none()
    );
    assert!(
        result["config_overrides"]
            .get("video_max_batch_size_kb")
            .is_none()
    );
    assert!(result["config_overrides"].get("stream_audio").is_none());
    assert!(
        result["config_overrides"]
            .get("audio_sink_capture_only")
            .is_none()
    );
    assert_eq!(state.writes, writes_before_read);
}

#[test]
fn revision_checked_unpair_preserves_trust_on_stale_or_racing_writes() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action":"import_client", "client_id":"living-room", "name":"Living room",
                "certificate_pem":CLIENT_CERT
            }),
            now()
        )["ok"],
        true
    );
    let listed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    let certificate_id = listed["clients"][0]["certificate_id"].clone();
    let revision = listed["revision"].as_u64().unwrap();
    let writes = state.writes;
    let stale = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"unpair", "certificate_id":certificate_id, "expected_revision":revision - 1
        }),
        now(),
    );
    assert_eq!(stale["code"], "revision_conflict");
    assert_eq!(state.writes, writes);
    state.conflict_next_write = true;
    let raced = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"unpair", "certificate_id":certificate_id, "expected_revision":revision
        }),
        now(),
    );
    assert_eq!(raced["code"], "revision_conflict");
    let still_paired = state.request(
        &mut pipeline,
        "nvhttp",
        json!({
            "action":"check_client_certificate", "certificate_pem":CLIENT_CERT
        }),
        now(),
    );
    assert_eq!(still_paired["paired"], 1);
    assert_eq!(still_paired["config_overrides"]["fec_percentage"], 73);
    let removed = state.request(
        &mut pipeline,
        "console",
        json!({
            "action":"unpair", "certificate_id":certificate_id, "expected_revision":state.revision
        }),
        now(),
    );
    assert_eq!(removed["ok"], true);
    let after = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(after["clients"].as_array().unwrap().len(), 0);
}

#[test]
fn external_state_cas_conflict_does_not_overwrite_another_admins_committed_value() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({
                "action": "import_client",
                "client_id": "living-room",
                "name": "Living room",
                "certificate_pem": CLIENT_CERT
            }),
            now()
        )["ok"],
        true
    );
    let listed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    let certificate_id = listed["clients"][0]["certificate_id"].as_str().unwrap();

    // The state owner commits after our get but before our CAS set.
    state.conflict_next_write = true;
    let conflicted = state.request(
        &mut pipeline,
        "console",
        json!({
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": listed["revision"],
            "config_overrides": {"fec_percentage": 61}
        }),
        now(),
    );
    assert_eq!(conflicted["code"], "revision_conflict");
    assert_eq!(conflicted["current_revision"], state.revision);

    let readback = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(readback["config_overrides"]["fec_percentage"], 73);
}

#[test]
fn native_snapshot_publishes_only_after_durable_write_ack_and_observes_revocation() {
    let mut pipeline = Pipeline::new(settings());
    let mut harness = Harness::new();
    harness.initialize(&mut pipeline);
    let client = crypto_certificate_der(CLIENT_CERT);
    assert!(
        !pipeline
            .identity
            .get(now())
            .unwrap()
            .verify_client(&client, now())
    );
    harness.enqueue("console",json!({"action":"import_client","client_id":"native-client","name":"Native client","certificate_pem":CLIENT_CERT}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    // Harness has committed the write, but the provider has not consumed its ack.
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    let trusted = pipeline.identity.get(now()).unwrap();
    assert!(trusted.verify_client(&client, now()));
    let fingerprint = crate::crypto::parse_certificate(CLIENT_CERT, None)
        .unwrap()
        .fingerprint;
    harness.enqueue(
        "console",
        json!({"action":"unpair","certificate_id":fingerprint}),
    );
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(!pipeline.identity.current(&trusted, now()));
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(
        !pipeline
            .identity
            .get(now())
            .unwrap()
            .verify_client(&client, now())
    );
}

#[test]
fn read_refresh_is_atomic_and_failed_load_or_save_suspends_native_identity() {
    let mut pipeline = Pipeline::new(settings());
    let mut harness = Harness::new();
    harness.initialize(&mut pipeline);
    let committed = pipeline.identity.get(now()).unwrap();
    harness.enqueue("nvhttp", json!({"action":"identity"}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.current(&committed, now()));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.current(&committed, now()));

    harness.fail_write = true;
    let result=harness.request(&mut pipeline,"console",json!({"action":"import_client","client_id":"uncommitted","name":"Uncommitted","certificate_pem":CLIENT_CERT}),now());
    assert_eq!(result["ok"], false);
    assert!(pipeline.identity.get(now()).is_err());
    harness.request(&mut pipeline, "nvhttp", json!({"action":"identity"}), now());
    assert!(
        !pipeline
            .identity
            .get(now())
            .unwrap()
            .verify_client(&crypto_certificate_der(CLIENT_CERT), now())
    );

    harness.enqueue("nvhttp", json!({"action":"identity"}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    harness.state_reply = Some(Response::Result {
        payload: json!({"ok":true,"revision":harness.revision,"found":true,"value":{"identity":"corrupt","clients":[]}}),
    });
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.get(now()).is_err());
}

fn paired_native_client() -> (Pipeline, Harness, String) {
    let mut pipeline = Pipeline::new(settings());
    let mut harness = Harness::new();
    harness.initialize(&mut pipeline);
    let result = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"import_client",
            "client_id":"native-client",
            "name":"Native client",
            "certificate_pem":CLIENT_CERT
        }),
        now(),
    );
    assert_eq!(result["ok"], true);
    let certificate_id = crate::crypto::parse_certificate(CLIENT_CERT, None)
        .unwrap()
        .fingerprint;
    (pipeline, harness, certificate_id)
}

#[test]
fn client_configuration_cas_preserves_confirmed_trust_until_atomic_acknowledgement() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let client = crypto_certificate_der(CLIENT_CERT);
    let confirmed = pipeline.identity.get(now()).unwrap();
    let base_revision = harness.revision;
    harness.enqueue(
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":certificate_id,
            "expected_revision":base_revision,
            "config_overrides":{"sw_preset":"","sw_tune":"  film,zerolatency  "}
        }),
    );
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.current(&confirmed, now()));
    assert!(confirmed.verify_client(&client, now()));

    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.revision, base_revision + 1);
    assert!(pipeline.identity.current(&confirmed, now()));
    let acknowledgement = harness.state_reply.take();
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.current(&confirmed, now()));
    harness.state_reply = acknowledgement;
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();

    assert_eq!(harness.output.as_ref().unwrap()["ok"], true);
    let committed = pipeline.identity.get(now()).unwrap();
    assert!(committed.verify_client(&client, now()));
    assert!(!pipeline.identity.current(&confirmed, now()));
    let readback = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(readback["config_overrides"]["sw_preset"], "");
    assert_eq!(
        readback["config_overrides"]["sw_tune"],
        "  film,zerolatency  "
    );
    assert!(pipeline.identity.current(&committed, now()));
}

#[test]
fn client_configuration_uncertain_load_or_commit_invalidates_native_authority() {
    for failure in ["load", "save", "conflict", "ack_revision"] {
        let (mut pipeline, mut harness, certificate_id) = paired_native_client();
        let confirmed = pipeline.identity.get(now()).unwrap();
        let base_revision = harness.revision;
        harness.fail_write = failure == "save";
        harness.conflict_next_write = failure == "conflict";
        harness.enqueue(
            "console",
            json!({
                "action":"update_client_config_overrides",
                "certificate_id":certificate_id,
                "expected_revision":base_revision,
                "config_overrides":{"sw_preset":"fast"}
            }),
        );
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        assert!(pipeline.identity.current(&confirmed, now()), "{failure}");
        if failure == "load" {
            harness.state_reply = Some(Response::Result {
                payload: json!({"ok":true,"revision":base_revision,"found":true,"value":null}),
            });
        }
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        if failure != "load" {
            assert!(pipeline.identity.current(&confirmed, now()), "{failure}");
            if failure == "ack_revision" {
                harness.state_reply = Some(Response::Result {
                    payload: json!({"ok":true,"revision":base_revision + 2}),
                });
            }
            pipeline
                .tick_at(now(), |command| harness.send(command))
                .unwrap();
        }
        assert!(pipeline.identity.get(now()).is_err(), "{failure}");
        assert!(!pipeline.identity.current(&confirmed, now()), "{failure}");
        let output = harness.output.as_ref().unwrap();
        assert_eq!(output["ok"], false, "{failure}");
        if failure == "ack_revision" {
            assert_eq!(output["code"], "state_unavailable");
        }
    }
}

#[test]
fn stale_client_configuration_revision_preserves_trust_without_writing() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let confirmed = pipeline.identity.get(now()).unwrap();
    let writes = harness.writes;
    let result = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":certificate_id,
            "expected_revision":harness.revision - 1,
            "config_overrides":{"sw_preset":"fast"}
        }),
        now(),
    );
    assert_eq!(result["code"], "revision_conflict");
    assert_eq!(harness.writes, writes);
    assert!(pipeline.identity.current(&confirmed, now()));
}

#[test]
fn abandoned_client_configuration_load_cannot_retain_unvalidated_authority() {
    for expired in [false, true] {
        let (mut pipeline, mut harness, certificate_id) = paired_native_client();
        let confirmed = pipeline.identity.get(now()).unwrap();
        let writes = harness.writes;
        harness.enqueue(
            "console",
            json!({
                "action":"update_client_config_overrides",
                "certificate_id":certificate_id,
                "expected_revision":harness.revision,
                "config_overrides":{"sw_preset":"fast"}
            }),
        );
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        assert!(pipeline.identity.current(&confirmed, now()));
        // The returned durable state has not been validated when the parent
        // deadline/cancellation guard stops this request.
        harness.state_reply = Some(Response::Result {
            payload: json!({"ok":true,"revision":harness.revision + 1,"found":true,"value":null}),
        });
        if expired {
            pipeline.pending.as_mut().unwrap().request.deadline = Instant::now();
        } else {
            harness.check_cancelled = true;
        }
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        assert!(pipeline.identity.get(now()).is_err());
        assert!(!pipeline.identity.current(&confirmed, now()));
        assert_eq!(harness.writes, writes);
        assert_eq!(
            harness.output.as_ref().unwrap()["code"],
            if expired {
                "deadline_exceeded"
            } else {
                "request_ended"
            }
        );
    }
}

#[test]
fn revision_exhaustion_cannot_submit_or_publish_client_configuration() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    harness.revision = u64::MAX;
    let writes = harness.writes;
    let result = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":certificate_id,
            "expected_revision":u64::MAX,
            "config_overrides":{"sw_preset":"fast"}
        }),
        now(),
    );
    assert_eq!(result["code"], "state_unavailable");
    assert_eq!(harness.writes, writes);
    assert!(pipeline.identity.get(now()).is_err());
}

fn crypto_certificate_der(pem: &str) -> Vec<u8> {
    crate::crypto::parse_certificate(pem, None)
        .unwrap()
        .certificate
        .to_der()
        .unwrap()
}

struct HostConfigurationHarness {
    actions: Vec<String>,
    operations: Vec<String>,
    missing_provider: bool,
    ready: bool,
    revision: u64,
    effective: Value,
    incoming: Option<Response>,
    output: Option<Value>,
    state_document: Option<Value>,
    migration_marker: Option<Value>,
    state_revision: u64,
    state_requests: usize,
    state_writes: usize,
    migration_state_requests: usize,
    migration_state_writes: usize,
    pending_migration_delete_revision: Option<u64>,
    fail_marker_delete_once: bool,
    snapshot_prior_page: bool,
    snapshot_page_count: usize,
    snapshot_revision_conflict: bool,
    state_save_pending: bool,
    polls: usize,
}

impl HostConfigurationHarness {
    fn new(effective: Value) -> Self {
        Self {
            actions: Vec::new(),
            operations: Vec::new(),
            missing_provider: false,
            ready: true,
            revision: 4,
            effective,
            incoming: None,
            output: None,
            state_document: None,
            migration_marker: None,
            state_revision: 0,
            state_requests: 0,
            state_writes: 0,
            migration_state_requests: 0,
            migration_state_writes: 0,
            pending_migration_delete_revision: None,
            fail_marker_delete_once: false,
            snapshot_prior_page: false,
            snapshot_page_count: 0,
            snapshot_revision_conflict: false,
            state_save_pending: false,
            polls: 0,
        }
    }

    fn enqueue_identity(&mut self) {
        self.enqueue("console", json!({"action":"identity"}));
    }

    fn enqueue(&mut self, caller: &str, payload: Value) {
        self.output = None;
        self.incoming = Some(Response::Request {
            id: 8,
            caller: caller.into(),
            initiator: caller.into(),
            service: SERVICE.into(),
            remaining_ms: 30_000,
            payload,
        });
    }

    fn request(
        &mut self,
        pipeline: &mut Pipeline,
        caller: &str,
        payload: Value,
        time: u64,
    ) -> Value {
        self.enqueue(caller, payload);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            pipeline
                .tick_at(time, |command| self.send(command))
                .unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("configured pairing request stalled")
    }

    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
            Command::Request {
                service,
                payload,
                operation_id,
                ..
            } if service == "amaterasu.host-configuration/1" => {
                let action = payload["action"].as_str().unwrap();
                self.actions.push(action.into());
                self.operations.push(operation_id);
                if action == "readiness" && self.missing_provider {
                    Response::error("service_unavailable", "host configuration is missing")
                } else if action == "readiness" {
                    Response::Accepted { id: 70 }
                } else {
                    assert_eq!(
                        action, "get",
                        "pairing must not request host-config updates"
                    );
                    Response::Accepted { id: 71 }
                }
            }
            Command::TakeResult { request: 70 } => Response::Result {
                payload: json!({"ok":self.ready,"ready":self.ready}),
            },
            Command::TakeResult { request: 71 } => Response::Result {
                payload: json!({
                    "ok":true,"ready":true,"revision":self.revision,"effective":self.effective
                }),
            },
            Command::Request {
                service, payload, ..
            } if service == STATE_SERVICE => match payload["action"].as_str() {
                Some("snapshot") => {
                    self.migration_state_requests += 1;
                    Response::Accepted { id: 74 }
                }
                Some("delete") => {
                    assert_eq!(payload["key"], IMPORTED_IDENTITY_MIGRATION_KEY);
                    self.migration_state_writes += 1;
                    self.pending_migration_delete_revision = payload["expected_revision"].as_u64();
                    Response::Accepted { id: 75 }
                }
                Some("get") => {
                    assert_eq!(payload["key"], STATE_KEY);
                    self.state_requests += 1;
                    Response::Accepted { id: 72 }
                }
                Some("set") => {
                    assert_eq!(payload["key"], STATE_KEY);
                    self.state_requests += 1;
                    assert_eq!(payload["expected_revision"], self.state_revision);
                    self.state_writes += 1;
                    self.state_document = Some(payload["value"].clone());
                    self.state_revision += 1;
                    Response::Accepted { id: 73 }
                }
                _ => panic!("unexpected pairing state operation"),
            },
            Command::TakeResult { request: 72 } => Response::Result {
                payload: json!({
                    "ok":true,
                    "revision":self.state_revision,
                    "found":self.state_document.is_some(),
                    "value":self.state_document.clone()
                }),
            },
            Command::TakeResult { request: 74 } => {
                if self.snapshot_prior_page && self.snapshot_page_count == 0 {
                    self.snapshot_page_count += 1;
                    return Ok(Response::Result {
                        payload: json!({
                            "ok":true,
                            "revision":self.state_revision,
                            "values":{"before-pairing":1},
                            "next_cursor":{"revision":self.state_revision,"after":"before-pairing"}
                        }),
                    });
                }
                if self.snapshot_revision_conflict && self.snapshot_page_count == 1 {
                    self.snapshot_page_count += 1;
                    self.state_revision += 1;
                    return Ok(Response::Result {
                        payload: json!({
                            "ok":false,
                            "error":{"code":"revision_conflict"},
                            "revision":self.state_revision
                        }),
                    });
                }
                self.snapshot_page_count += 1;
                let mut values = Map::new();
                if let Some(document) = &self.state_document {
                    values.insert(STATE_KEY.into(), document.clone());
                }
                if let Some(marker) = &self.migration_marker {
                    values.insert(IMPORTED_IDENTITY_MIGRATION_KEY.into(), marker.clone());
                }
                Response::Result {
                    payload: json!({
                        "ok":true,
                        "revision":self.state_revision,
                        "values":values,
                        "next_cursor":null
                    }),
                }
            }
            Command::TakeResult { request: 75 } => {
                let expected_revision = self.pending_migration_delete_revision.take().unwrap();
                if self.fail_marker_delete_once {
                    self.fail_marker_delete_once = false;
                    self.state_revision += 1;
                    Response::Result {
                        payload: json!({
                            "ok":false,
                            "error":{"code":"revision_conflict"},
                            "revision":self.state_revision
                        }),
                    }
                } else if expected_revision != self.state_revision {
                    Response::Result {
                        payload: json!({
                            "ok":false,
                            "error":{"code":"revision_conflict"},
                            "revision":self.state_revision
                        }),
                    }
                } else {
                    let deleted = self.migration_marker.take().is_some();
                    self.state_revision += 1;
                    Response::Result {
                        payload: json!({
                            "ok":true,
                            "deleted":deleted,
                            "revision":self.state_revision
                        }),
                    }
                }
            }
            Command::TakeResult { request: 73 } if self.state_save_pending => Response::Pending,
            Command::TakeResult { request: 73 } => Response::Result {
                payload: json!({"ok":true,"revision":self.state_revision}),
            },
            Command::CheckRequest { request: 8 } => Response::Ok,
            Command::Poll {} => {
                self.polls += 1;
                self.incoming.take().unwrap_or(Response::Pending)
            }
            Command::Complete {
                request: 8,
                payload,
            } => {
                self.output = Some(payload);
                Response::Ok
            }
            command => panic!("unexpected host-configuration command: {command:?}"),
        })
    }
}

fn configured_identity_files(
    certificate_pem: &str,
    private_key_pem: &str,
) -> (std::path::PathBuf, Value) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("amaterasu-pairing-config-{nonce}"));
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let certificate = directory.join("host-cert.pem");
    let key = directory.join("host-key.pem");
    std::fs::write(&certificate, certificate_pem).unwrap();
    std::fs::write(&key, private_key_pem).unwrap();
    (
        directory,
        json!({
            "cert":certificate.to_string_lossy(),
            "pkey":key.to_string_lossy()
        }),
    )
}

fn configured_fixture_files() -> (std::path::PathBuf, Value) {
    configured_identity_files(HOST_CERT, HOST_KEY)
}

fn generated_configured_files() -> (std::path::PathBuf, Value, crate::protocol::Identity) {
    let identity = crate::crypto::generate_identity().unwrap();
    let (directory, effective) =
        configured_identity_files(&identity.certificate_pem, &identity.private_key_pem);
    (directory, effective, identity)
}

fn paired_fixture_client() -> Value {
    paired_fixture_client_for(CLIENT_CERT)
}

fn paired_fixture_client_for(certificate_pem: &str) -> Value {
    let certificate_id = crate::crypto::parse_certificate(certificate_pem, None)
        .unwrap()
        .fingerprint;
    json!([{
        "certificate_id":certificate_id,
        "client_id":"existing-client",
        "name":"Existing client",
        "certificate_pem":certificate_pem,
        "paired_at":1
    }])
}

fn certificate_expiration(pem: &str) -> u64 {
    Certificate::from_pem(pem)
        .unwrap()
        .tbs_certificate
        .validity
        .not_after
        .to_unix_duration()
        .as_secs()
}

fn stored_document_with_fixture_identity(clients: Value) -> Value {
    json!({
        "identity": {
            "host_id":"00000000-0000-4000-8000-000000000001",
            "certificate_pem":HOST_CERT,
            "private_key_pem":HOST_KEY
        },
        "clients":clients
    })
}

fn imported_identity_marker(document: &Value) -> Value {
    let document = serde_json::from_value(document.clone()).unwrap();
    crate::identity::imported_identity_migration_marker(&document).unwrap()
}

#[test]
fn configured_pipeline_calls_readiness_then_get_once_before_pairing_state() {
    let (directory, effective) = configured_fixture_files();
    let mut harness = HostConfigurationHarness::new(effective);
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

    let output = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(harness.actions, ["readiness", "get"]);
    assert_eq!(harness.polls, 1);
    assert_eq!(harness.state_requests, 1);
    assert_eq!(output["code"], "not_initialized");
    assert_eq!(
        pipeline.identity.get(now()).unwrap().certificate(),
        crypto_certificate_der(HOST_CERT)
    );
    let list_clients = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(list_clients["code"], "not_initialized");
    assert_eq!(harness.actions, ["readiness", "get"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn imported_pairing_identity_materializes_before_generation_and_survives_restart() {
    let document = stored_document_with_fixture_identity(paired_fixture_client());
    let marker = imported_identity_marker(&document);
    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap().to_owned();
    let private_key_path = effective["pkey"].as_str().unwrap().to_owned();
    std::fs::remove_file(&certificate_path).unwrap();
    std::fs::remove_file(&private_key_path).unwrap();
    let mut harness = HostConfigurationHarness::new(effective.clone());
    harness.state_document = Some(document.clone());
    harness.migration_marker = Some(marker);
    harness.state_revision = 6;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = now();

    let response = harness.request(&mut pipeline, "console", json!({"action":"identity"}), time);
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["certificate_pem"], HOST_CERT);
    assert_eq!(response["host_id"], document["identity"]["host_id"]);
    assert_eq!(
        std::fs::read(certificate_path).unwrap(),
        HOST_CERT.as_bytes()
    );
    assert_eq!(
        std::fs::read(private_key_path).unwrap(),
        HOST_KEY.as_bytes()
    );
    assert!(harness.migration_marker.is_none());
    assert_eq!(harness.migration_state_writes, 1);
    let first = pipeline.identity.get(time).unwrap();
    let fingerprint = crate::crypto::parse_certificate(HOST_CERT, Some(time))
        .unwrap()
        .fingerprint;
    assert_eq!(first.certificate(), crypto_certificate_der(HOST_CERT));
    assert!(first.verify_client(&crypto_certificate_der(CLIENT_CERT), time));

    // Restart sees the durable files and client list after the one-shot marker
    // was consumed; it keeps the same TLS fingerprint and paired-client trust.
    let mut restarted_harness = HostConfigurationHarness::new(effective);
    restarted_harness.state_document = Some(document);
    restarted_harness.state_revision = harness.state_revision;
    let mut restarted =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let restarted_response = restarted_harness.request(
        &mut restarted,
        "console",
        json!({"action":"identity"}),
        time,
    );
    assert_eq!(restarted_response["certificate_pem"], HOST_CERT);
    assert_eq!(
        crate::crypto::parse_certificate(
            restarted_response["certificate_pem"].as_str().unwrap(),
            Some(time)
        )
        .unwrap()
        .fingerprint,
        fingerprint
    );
    let restarted_snapshot = restarted.identity.get(time).unwrap();
    assert!(restarted_snapshot.verify_client(&crypto_certificate_der(CLIENT_CERT), time));
    assert_eq!(restarted_harness.migration_state_writes, 0);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn imported_identity_materialization_retries_after_marker_delete_conflict() {
    let document = stored_document_with_fixture_identity(paired_fixture_client());
    let marker = imported_identity_marker(&document);
    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap();
    let private_key_path = effective["pkey"].as_str().unwrap();
    std::fs::remove_file(certificate_path).unwrap();
    std::fs::remove_file(private_key_path).unwrap();
    let mut first_harness = HostConfigurationHarness::new(effective.clone());
    first_harness.state_document = Some(document.clone());
    first_harness.migration_marker = Some(marker.clone());
    first_harness.state_revision = 6;
    first_harness.fail_marker_delete_once = true;
    let mut first_pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let failure = first_harness.request(
        &mut first_pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(failure["code"], "host_configuration_unavailable");
    assert_eq!(first_harness.migration_marker, Some(marker.clone()));
    assert_eq!(
        std::fs::read(certificate_path).unwrap(),
        HOST_CERT.as_bytes()
    );
    assert_eq!(
        std::fs::read(private_key_path).unwrap(),
        HOST_KEY.as_bytes()
    );

    // The exact pair already exists after the interrupted attempt. A new
    // process validates/protects it, retries the marker CAS, and becomes ready.
    let mut retry_harness = HostConfigurationHarness::new(effective);
    retry_harness.state_document = Some(document);
    retry_harness.migration_marker = Some(marker);
    retry_harness.state_revision = first_harness.state_revision;
    let mut retry_pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let retried = retry_harness.request(
        &mut retry_pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(retried["certificate_pem"], HOST_CERT);
    assert!(retry_harness.migration_marker.is_none());
    assert!(
        retry_pipeline
            .identity
            .get(now())
            .unwrap()
            .verify_client(&crypto_certificate_der(CLIENT_CERT), now())
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn imported_identity_state_snapshot_follows_pages_at_one_revision() {
    let document = stored_document_with_fixture_identity(paired_fixture_client());
    let marker = imported_identity_marker(&document);
    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap();
    let private_key_path = effective["pkey"].as_str().unwrap();
    std::fs::remove_file(certificate_path).unwrap();
    std::fs::remove_file(private_key_path).unwrap();
    let mut harness = HostConfigurationHarness::new(effective.clone());
    harness.state_document = Some(document.clone());
    harness.migration_marker = Some(marker.clone());
    harness.state_revision = 6;
    harness.snapshot_prior_page = true;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(response["certificate_pem"], HOST_CERT);
    assert_eq!(harness.snapshot_page_count, 2);
    assert!(harness.migration_marker.is_none());
    std::fs::remove_dir_all(directory).unwrap();

    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap().to_owned();
    let private_key_path = effective["pkey"].as_str().unwrap().to_owned();
    std::fs::remove_file(&certificate_path).unwrap();
    std::fs::remove_file(&private_key_path).unwrap();
    let mut conflicted = HostConfigurationHarness::new(effective);
    conflicted.state_document = Some(document);
    conflicted.migration_marker = Some(marker);
    conflicted.state_revision = 6;
    conflicted.snapshot_prior_page = true;
    conflicted.snapshot_revision_conflict = true;
    let mut conflicted_pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let failure = conflicted.request(
        &mut conflicted_pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(failure["code"], "host_configuration_unavailable");
    assert!(conflicted.migration_marker.is_some());
    assert!(!std::path::Path::new(&certificate_path).exists());
    assert!(!std::path::Path::new(&private_key_path).exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn exact_existing_imported_pair_is_tightened_before_marker_consumption() {
    use std::os::unix::fs::PermissionsExt;

    let document = stored_document_with_fixture_identity(paired_fixture_client());
    let marker = imported_identity_marker(&document);
    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap().to_owned();
    let private_key_path = effective["pkey"].as_str().unwrap().to_owned();
    std::fs::set_permissions(&certificate_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&private_key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(document);
    harness.migration_marker = Some(marker);
    harness.state_revision = 2;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(response["certificate_pem"], HOST_CERT);
    assert_eq!(
        std::fs::metadata(&certificate_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&private_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(harness.migration_marker.is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn destination_tls_files_remain_authoritative_when_imported_pairing_value_loses() {
    let imported_document = stored_document_with_fixture_identity(json!([]));
    let marker = imported_identity_marker(&imported_document);
    let destination_identity = crate::crypto::generate_identity().unwrap();
    let destination_document = json!({
        "identity": destination_identity,
        "clients": []
    });
    let (directory, effective) = configured_identity_files(CLIENT_CERT, CLIENT_KEY);
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(destination_document.clone());
    harness.migration_marker = Some(marker);
    harness.state_revision = 3;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["certificate_pem"], CLIENT_CERT);
    assert_eq!(
        response["host_id"],
        destination_document["identity"]["host_id"]
    );
    assert!(harness.migration_marker.is_none());
    assert_eq!(
        std::fs::read_to_string(harness.effective["cert"].as_str().unwrap()).unwrap(),
        CLIENT_CERT
    );
    assert_eq!(
        std::fs::read_to_string(harness.effective["pkey"].as_str().unwrap()).unwrap(),
        CLIENT_KEY
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn exact_import_with_partial_or_conflicting_files_fails_without_repair() {
    let document = stored_document_with_fixture_identity(json!([]));
    let marker = imported_identity_marker(&document);
    let (directory, effective) = configured_identity_files(HOST_CERT, HOST_KEY);
    let certificate_path = effective["cert"].as_str().unwrap().to_owned();
    let private_key_path = effective["pkey"].as_str().unwrap().to_owned();
    std::fs::write(&certificate_path, b"destination certificate").unwrap();
    std::fs::remove_file(&private_key_path).unwrap();
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(document);
    harness.migration_marker = Some(marker);
    harness.state_revision = 3;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        now(),
    );
    assert_eq!(response["code"], "host_configuration_unavailable");
    assert!(harness.migration_marker.is_some());
    assert_eq!(
        std::fs::read(&certificate_path).unwrap(),
        b"destination certificate"
    );
    assert!(!std::path::Path::new(&private_key_path).exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn provider_missing_or_unready_fails_closed_without_reading_pairing_state() {
    for (missing_provider, ready) in [(true, false), (false, false)] {
        let mut harness = HostConfigurationHarness::new(json!({}));
        harness.missing_provider = missing_provider;
        harness.ready = ready;
        harness.enqueue_identity();
        let mut pipeline =
            Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

        for _ in 0..2 {
            if harness.output.is_some() {
                break;
            }
            pipeline
                .tick_at(now(), |command| harness.send(command))
                .unwrap();
        }
        let output = harness.output.unwrap();
        assert_eq!(output["code"], "host_configuration_unavailable");
        assert_eq!(harness.state_requests, 0);
        assert_eq!(harness.actions, ["readiness"]);
    }
}

#[test]
fn configured_provider_missing_effective_key_or_certificate_fails_closed() {
    for effective in [
        json!({"cert":"/unused/certificate.pem"}),
        json!({"pkey":"/unused/key.pem"}),
    ] {
        let mut harness = HostConfigurationHarness::new(effective);
        harness.enqueue_identity();
        let mut pipeline =
            Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);

        for _ in 0..3 {
            pipeline
                .tick_at(now(), |command| harness.send(command))
                .unwrap();
        }
        assert_eq!(
            harness.output.unwrap()["code"],
            "host_configuration_unavailable"
        );
        assert_eq!(harness.actions, ["readiness", "get"]);
        assert_eq!(harness.state_requests, 0);
    }
}

#[test]
fn missing_first_boot_files_are_provisioned_before_pairing_reads_state() {
    let (directory, effective, _) = generated_configured_files();
    std::fs::remove_file(effective["cert"].as_str().unwrap()).unwrap();
    std::fs::remove_file(effective["pkey"].as_str().unwrap()).unwrap();
    let mut harness = HostConfigurationHarness::new(effective.clone());
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let response = harness.request(&mut pipeline, "console", json!({"action":"identity"}), time);
    assert_eq!(response["code"], "not_initialized");
    assert_eq!(harness.state_requests, 1);
    assert_eq!(harness.state_writes, 0);
    assert_eq!(harness.actions, ["readiness", "get"]);

    let certificate_path = effective["cert"].as_str().unwrap();
    let key_path = effective["pkey"].as_str().unwrap();
    let provisioned_certificate = std::fs::read_to_string(certificate_path).unwrap();
    let provisioned_key = std::fs::read_to_string(key_path).unwrap();
    let configured = crate::identity::ConfiguredIdentity::load(
        certificate_path,
        key_path,
        harness.revision,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    assert_eq!(configured.certificate_pem, provisioned_certificate);
    assert_eq!(configured.private_key_pem, provisioned_key);
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(
        pipeline.identity.get(current_time).unwrap().certificate(),
        crypto_certificate_der(&provisioned_certificate)
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unavailable_configuration_does_not_defer_unauthorized_identity_import() {
    let mut harness = HostConfigurationHarness::new(json!({}));
    harness.missing_provider = true;
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let response = harness.request(
        &mut pipeline,
        "untrusted",
        json!({
            "action":"import_identity",
            "certificate_pem":HOST_CERT,
            "private_key_pem":HOST_KEY
        }),
        now(),
    );
    assert_eq!(response["code"], "host_configuration_unavailable");
    assert!(pipeline.deferred_import.is_none());
}

#[test]
fn refreshed_configuration_revokes_old_tls_identity_when_new_files_are_invalid() {
    let (directory, effective, configured) = generated_configured_files();
    let mut harness = HostConfigurationHarness::new(effective);
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let imported = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"import_identity",
            "certificate_pem":configured.certificate_pem,
            "private_key_pem":configured.private_key_pem
        }),
        time,
    );
    assert_eq!(imported["ok"], true, "{imported}");
    assert!(pipeline.identity.get(time).is_ok());

    harness.revision = 5;
    harness.effective["cert"] = "/missing/host-cert.pem".into();
    pipeline
        .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
            harness.send(command)
        })
        .unwrap();
    assert!(
        pipeline
            .identity
            .get(time + HOST_CONFIGURATION_REFRESH_SECONDS)
            .is_ok()
    );
    pipeline
        .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
            harness.send(command)
        })
        .unwrap();
    pipeline
        .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
            harness.send(command)
        })
        .unwrap();
    assert!(matches!(
        pipeline.host_configuration,
        HostConfigurationState::Failed { .. }
    ));
    assert!(
        pipeline
            .identity
            .get(time + HOST_CONFIGURATION_REFRESH_SECONDS)
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unchanged_configuration_refresh_keeps_the_published_tls_snapshot() {
    let (directory, effective) = configured_fixture_files();
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(stored_document_with_fixture_identity(json!([])));
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = now();
    let response = harness.request(&mut pipeline, "console", json!({"action":"identity"}), time);
    assert_eq!(response["ok"], true, "{response}");
    let original = pipeline.identity.get(time).unwrap();

    for _ in 0..3 {
        pipeline
            .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
                harness.send(command)
            })
            .unwrap();
        let current = pipeline
            .identity
            .get(time + HOST_CONFIGURATION_REFRESH_SECONDS)
            .unwrap();
        assert!(Arc::ptr_eq(&original, &current));
    }
    assert_eq!(harness.actions, ["readiness", "get", "readiness", "get"]);
    assert!(matches!(
        pipeline.host_configuration,
        HostConfigurationState::Ready { .. }
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pending_state_save_finishes_under_its_original_configuration_revision() {
    let (directory, effective) = configured_fixture_files();
    let durable = stored_document_with_fixture_identity(paired_fixture_client());
    let certificate_id = durable["clients"][0]["certificate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(durable);
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = now();
    let response = harness.request(&mut pipeline, "console", json!({"action":"identity"}), time);
    assert_eq!(response["ok"], true, "{response}");

    harness.enqueue(
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":certificate_id,
            "config_overrides":{"fec_percentage":73},
            "expected_revision":0
        }),
    );
    pipeline
        .tick_at(time, |command| harness.send(command))
        .unwrap();
    pipeline
        .tick_at(time, |command| harness.send(command))
        .unwrap();
    assert!(matches!(
        pipeline.pending.as_ref().map(|pending| &pending.stage),
        Some(Stage::Saving { .. })
    ));
    harness.state_save_pending = true;
    harness.revision = 5;
    harness.effective["cert"] = "/missing/host-cert.pem".into();

    pipeline
        .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
            harness.send(command)
        })
        .unwrap();
    assert_eq!(harness.actions, ["readiness", "get"]);
    assert!(harness.output.is_none());
    harness.state_save_pending = false;
    pipeline
        .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
            harness.send(command)
        })
        .unwrap();
    assert_eq!(harness.output.as_ref().unwrap()["ok"], true);
    assert_eq!(pipeline.configured_identity().unwrap().provider_revision, 4);
    assert_eq!(harness.actions, ["readiness", "get"]);

    for _ in 0..3 {
        pipeline
            .tick_at(time + HOST_CONFIGURATION_REFRESH_SECONDS, |command| {
                harness.send(command)
            })
            .unwrap();
    }
    assert_eq!(harness.actions, ["readiness", "get", "readiness", "get"]);
    assert!(
        pipeline
            .identity
            .get(time + HOST_CONFIGURATION_REFRESH_SECONDS)
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_identity_and_pairing_transcript_use_the_effective_pair() {
    let (directory, effective, configured) = generated_configured_files();
    let old_host_id = "00000000-0000-4000-8000-000000000001";
    let durable = stored_document_with_fixture_identity(paired_fixture_client_for(HOST_CERT));
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(durable.clone());
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let identity = harness.request(&mut pipeline, "console", json!({"action":"identity"}), time);
    assert_eq!(identity["host_id"], old_host_id);
    assert_eq!(identity["certificate_pem"], configured.certificate_pem);
    assert!(identity.get("private_key_pem").is_none());

    let begin = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({
            "action":"begin",
            "client_id":"new-client",
            "name":"New client",
            "client_certificate_pem":CLIENT_CERT,
            "salt":encode(&(0..16).collect::<Vec<_>>())
        }),
        time,
    );
    assert_eq!(begin["ok"], true, "{begin}");
    let pairing_id = begin["pairing_id"].as_str().unwrap();
    assert_eq!(
        harness.request(
            &mut pipeline,
            "console",
            json!({"action":"approve_pin","pairing_id":pairing_id,"pin":"1234"}),
            time,
        )["ok"],
        true
    );
    let server_certificate = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"get_server_cert","pairing_id":pairing_id}),
        time,
    );
    let server_pem =
        String::from_utf8(decode(server_certificate["plaincert"].as_str().unwrap())).unwrap();
    assert_eq!(server_pem, configured.certificate_pem);
    let mut transcript = ClientTranscript::new("1234", &server_pem);
    let challenge = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({
            "action":"client_challenge",
            "pairing_id":pairing_id,
            "clientchallenge":transcript.challenge()
        }),
        time,
    );
    let response = transcript.respond(challenge["challengeresponse"].as_str().unwrap());
    let proof = harness.request(
        &mut pipeline,
        "nvhttp",
        json!({
            "action":"server_challenge_response",
            "pairing_id":pairing_id,
            "serverchallengeresp":response
        }),
        time,
    );
    assert!(transcript.validate_server(proof["pairingsecret"].as_str().unwrap()));
    assert_eq!(harness.state_document.as_ref().unwrap(), &durable);
    assert_eq!(harness.state_writes, 0);
    assert_eq!(harness.actions, ["readiness", "get"]);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_initialize_persists_only_generated_identity_and_returns_effective_certificate() {
    let (directory, effective, configured) = generated_configured_files();
    let mut harness = HostConfigurationHarness::new(effective);
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"initialize"}),
        time,
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["certificate_pem"], configured.certificate_pem);
    assert!(response.get("private_key_pem").is_none());
    let persisted = harness.state_document.as_ref().unwrap();
    assert_eq!(persisted["identity"]["host_id"], response["host_id"]);
    assert_ne!(
        persisted["identity"]["certificate_pem"],
        configured.certificate_pem
    );
    assert_ne!(
        persisted["identity"]["private_key_pem"],
        configured.private_key_pem
    );
    assert_eq!(harness.state_writes, 1);
    let identity_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(
        pipeline.identity.get(identity_time).unwrap().certificate(),
        crypto_certificate_der(&configured.certificate_pem)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_write_restores_persisted_identity_and_preserves_paired_clients() {
    let (directory, effective, configured) = generated_configured_files();
    let durable = stored_document_with_fixture_identity(paired_fixture_client());
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(durable.clone());
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let client_id = durable["clients"][0]["certificate_id"].as_str().unwrap();

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"update_client_config_overrides",
            "certificate_id":client_id,
            "config_overrides":{"fec_percentage":73},
            "expected_revision":0
        }),
        time,
    );
    assert_eq!(response["ok"], true, "{response}");
    let persisted = harness.state_document.as_ref().unwrap();
    assert_eq!(persisted["identity"], durable["identity"]);
    assert_eq!(persisted["clients"][0]["certificate_id"], client_id);
    assert_eq!(persisted["clients"][0]["client_id"], "existing-client");
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["fec_percentage"],
        73
    );
    let serialized_state = serde_json::to_string(persisted).unwrap();
    assert!(!serialized_state.contains(&configured.private_key_pem));
    assert_eq!(harness.state_writes, 1);
    let snapshot = pipeline.identity.get(time).unwrap();
    assert_eq!(
        snapshot.certificate(),
        crypto_certificate_der(&configured.certificate_pem)
    );
    assert!(snapshot.verify_client(&crypto_certificate_der(CLIENT_CERT), time));

    let restart_document = persisted.clone();
    let mut restarted_harness = HostConfigurationHarness::new(harness.effective.clone());
    restarted_harness.state_document = Some(restart_document.clone());
    let mut restarted_pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let restarted_identity = restarted_harness.request(
        &mut restarted_pipeline,
        "console",
        json!({"action":"identity"}),
        time,
    );
    assert_eq!(
        restarted_identity["certificate_pem"],
        configured.certificate_pem
    );
    assert_eq!(
        restarted_identity["host_id"],
        restart_document["identity"]["host_id"]
    );
    assert_eq!(
        restarted_harness.state_document.as_ref().unwrap(),
        &restart_document
    );
    assert_eq!(restarted_harness.state_writes, 0);
    assert_eq!(
        restarted_pipeline.identity.get(time).unwrap().certificate(),
        crypto_certificate_der(&configured.certificate_pem)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_pair_overlays_an_expired_stored_certificate_without_rewriting_it() {
    let (directory, effective, configured) = generated_configured_files();
    let durable = stored_document_with_fixture_identity(json!([]));
    let mut harness = HostConfigurationHarness::new(effective);
    harness.state_document = Some(durable.clone());
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let after_expiry = certificate_expiration(HOST_CERT) + 1;

    let response = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"identity"}),
        after_expiry,
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["certificate_pem"], configured.certificate_pem);
    assert_eq!(response["host_id"], durable["identity"]["host_id"]);
    assert_eq!(harness.state_document.as_ref().unwrap(), &durable);
    assert_eq!(harness.state_writes, 0);
    assert!(
        pipeline
            .identity
            .get(after_expiry)
            .is_ok_and(|snapshot| snapshot.certificate()
                == crypto_certificate_der(&configured.certificate_pem))
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_provider_rejects_import_identity_without_state_access() {
    let (directory, effective, _) = generated_configured_files();
    let mut harness = HostConfigurationHarness::new(effective);
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let response = harness.request(
        &mut pipeline,
        "console",
        json!({
            "action":"import_identity",
            "certificate_pem":HOST_CERT,
            "private_key_pem":HOST_KEY
        }),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    );
    assert_eq!(response["code"], "identity_managed_by_host_configuration");
    assert_eq!(harness.state_requests, 0);
    assert_eq!(harness.state_writes, 0);
    assert_eq!(harness.actions, ["readiness", "get"]);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn legacy_pipeline_skips_host_configuration_and_keeps_persisted_identity_flow() {
    let mut pipeline = Pipeline::new(settings());
    let mut commands = Vec::new();
    pipeline
        .tick_at(now(), |command| {
            commands.push(command);
            Ok(Response::Pending)
        })
        .unwrap();
    assert!(matches!(commands.as_slice(), [Command::Poll {}]));
}

fn import_client(
    state: &mut Harness,
    pipeline: &mut Pipeline,
    client_id: &str,
    pem: &str,
) -> Value {
    let reply = state.request(
        pipeline,
        "console",
        json!({"action":"import_client","client_id":client_id,"name":"Moonlight test","certificate_pem":pem}),
        now(),
    );
    assert_eq!(reply["ok"], true, "{reply}");
    reply
}

fn check_client(state: &mut Harness, pipeline: &mut Pipeline, pem: &str) -> Value {
    state.request(
        pipeline,
        "nvhttp",
        json!({"action":"check_client_certificate","certificate_pem":pem}),
        now(),
    )
}

#[test]
fn presented_client_unpair_is_transport_only_and_rejects_admin_and_foreign_callers() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    import_client(&mut state, &mut pipeline, "moonlight", CLIENT_CERT);
    let writes = state.writes;
    let request = json!({"action":"unpair_presented_client","certificate_pem":CLIENT_CERT});
    for caller in ["console", "untrusted"] {
        assert_eq!(
            state.request(&mut pipeline, caller, request.clone(), now())["code"],
            "forbidden",
            "{caller}"
        );
    }
    assert_eq!(state.writes, writes);
    assert_eq!(
        check_client(&mut state, &mut pipeline, CLIENT_CERT)["paired"],
        1
    );
}

#[test]
fn presented_client_unpair_removes_exactly_that_certificate_and_is_idempotent() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    // The host certificate is a second valid self-signed certificate; it stands
    // in for another paired client that must stay trusted.
    let other = import_client(&mut state, &mut pipeline, "other", HOST_CERT);
    let first = import_client(&mut state, &mut pipeline, "moonlight", CLIENT_CERT);
    let request = json!({"action":"unpair_presented_client","certificate_pem":CLIENT_CERT});

    let removed = state.request(&mut pipeline, "nvhttp", request.clone(), now());
    assert_eq!(removed["ok"], true, "{removed}");
    assert_eq!(removed["unpaired"], 1);
    assert_eq!(removed["certificate_id"], first["certificate_id"]);
    assert_eq!(
        check_client(&mut state, &mut pipeline, CLIENT_CERT)["code"],
        "not_paired"
    );
    let kept = check_client(&mut state, &mut pipeline, HOST_CERT);
    assert_eq!(kept["certificate_id"], other["certificate_id"]);

    let writes = state.writes;
    let repeated = state.request(&mut pipeline, "nvhttp", request, now());
    assert_eq!(repeated["ok"], true);
    assert_eq!(repeated["unpaired"], 0);
    assert_eq!(
        state.writes, writes,
        "an unknown certificate must not write"
    );
}

#[test]
fn presented_client_unpair_persists_across_restart_and_allows_pairing_again() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    import_client(&mut state, &mut pipeline, "moonlight", CLIENT_CERT);
    let removed = state.request(
        &mut pipeline,
        "nvhttp",
        json!({"action":"unpair_presented_client","certificate_pem":CLIENT_CERT}),
        now(),
    );
    assert_eq!(removed["unpaired"], 1);

    let mut restarted = Pipeline::new(settings());
    assert_eq!(
        check_client(&mut state, &mut restarted, CLIENT_CERT)["code"],
        "not_paired"
    );
    let id = begin(&mut state, &mut restarted, "moonlight", now());
    let client = exchange(&mut state, &mut restarted, &id, "1234", now());
    let paired = state.request(
        &mut restarted,
        "nvhttp",
        json!({"action":"client_pairing_secret","pairing_id":id,"clientpairingsecret":client.finish()}),
        now(),
    );
    assert_eq!(paired["paired"], 1, "{paired}");
    assert_eq!(
        check_client(&mut state, &mut restarted, CLIENT_CERT)["paired"],
        1
    );
}

#[test]
fn presented_client_unpair_rejects_malformed_and_oversized_certificates() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    import_client(&mut state, &mut pipeline, "moonlight", CLIENT_CERT);
    let writes = state.writes;
    for pem in ["", "not a certificate", &"A".repeat(4097)] {
        let reply = state.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"unpair_presented_client","certificate_pem":pem}),
            now(),
        );
        assert_eq!(reply["ok"], false, "{pem:.20}");
    }
    assert_eq!(state.writes, writes);
    assert_eq!(
        check_client(&mut state, &mut pipeline, CLIENT_CERT)["paired"],
        1
    );
}

/// Releases a deliberately blocked worker even if an assertion fails, so a
/// teardown regression reports a failure instead of leaving a native thread.
struct DelayedIdentityWork {
    release: Option<std::sync::mpsc::Sender<()>>,
    started: std::sync::mpsc::Receiver<()>,
}

impl DelayedIdentityWork {
    fn generator<T: Send + 'static>(
        result: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
    ) -> (Self, Generator<T>) {
        let (release, wait) = std::sync::mpsc::channel();
        let (started, started_wait) = std::sync::mpsc::channel();
        let generator = Box::new(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            result()
        });
        (
            Self {
                release: Some(release),
                started: started_wait,
            },
            generator,
        )
    }

    fn wait_started(&self) {
        self.started.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    fn finish(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
}

impl Drop for DelayedIdentityWork {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

fn generated_fixture() -> Result<Identity, &'static str> {
    Ok(Identity {
        host_id: "00000000-0000-4000-8000-000000000002".into(),
        certificate_pem: HOST_CERT.into(),
        private_key_pem: HOST_KEY.into(),
    })
}

fn start_delayed_initialization(
    pipeline: &mut Pipeline,
    state: &mut Harness,
) -> DelayedIdentityWork {
    let (delay, generator) = DelayedIdentityWork::generator(generated_fixture);
    pipeline.identity_generator = Some(generator);
    state.enqueue("console", json!({"action":"initialize"}));
    for _ in 0..2 {
        pipeline
            .tick_at(now(), |command| state.send(command))
            .unwrap();
    }
    delay.wait_started();
    assert!(pipeline.identity_generation.is_some());
    delay
}

fn drain_identity_work(pipeline: &mut Pipeline, state: &mut Harness) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while pipeline.identity_generation.is_some() && Instant::now() < deadline {
        pipeline
            .tick_at(now(), |command| state.send(command))
            .unwrap();
        std::thread::yield_now();
    }
    assert!(pipeline.identity_generation.is_none());
}

#[test]
fn pending_identity_generation_leaves_host_ticks_and_configuration_work_responsive() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    let mut delay = start_delayed_initialization(&mut pipeline, &mut state);
    let started = Instant::now();
    let mut configuration_requests_completed = 0;
    for _ in 0..20 {
        pipeline
            .tick_at(now(), |command| state.send(command))
            .unwrap();
        // The host executes other modules on this same scheduler thread. Work
        // after pairing's tick must progress before the generator is released.
        configuration_requests_completed += 1;
    }
    assert_eq!(configuration_requests_completed, 20);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(state.output.is_none());
    assert_eq!(state.writes, 0);
    assert!(pipeline.identity.get(now()).is_err());
    delay.finish();
    drain_identity_work(&mut pipeline, &mut state);
    assert_eq!(state.writes, 1);
    assert!(
        pipeline.identity.get(now()).is_err(),
        "save has not acknowledged yet"
    );
    pipeline
        .tick_at(now(), |command| state.send(command))
        .unwrap();
    assert_eq!(state.output.as_ref().unwrap()["ok"], true);
    assert!(pipeline.identity.get(now()).is_ok());
}

#[test]
fn cancellation_and_deadline_discard_generated_identity_and_keep_one_owned_worker() {
    for deadline_ended in [false, true] {
        let mut state = Harness::new();
        let mut pipeline = Pipeline::new(settings());
        let mut delay = start_delayed_initialization(&mut pipeline, &mut state);
        if deadline_ended {
            pipeline
                .identity_generation
                .as_mut()
                .unwrap()
                .initialization
                .as_mut()
                .unwrap()
                .request
                .deadline = Instant::now();
        } else {
            state.check_cancelled = true;
        }
        pipeline
            .tick_at(now(), |command| state.send(command))
            .unwrap();
        assert_eq!(
            state.output.as_ref().unwrap()["code"],
            if deadline_ended {
                "deadline_exceeded"
            } else {
                "request_ended"
            }
        );
        assert!(
            pipeline
                .identity_generation
                .as_ref()
                .unwrap()
                .initialization
                .is_none()
        );
        state.enqueue("console", json!({"action":"initialize"}));
        pipeline
            .tick_at(now(), |command| state.send(command))
            .unwrap();
        assert_eq!(
            state.output.as_ref().unwrap()["code"],
            "identity_generation_busy"
        );
        delay.finish();
        drain_identity_work(&mut pipeline, &mut state);
        assert_eq!(state.writes, 0);
        assert!(state.document.is_none());
        assert!(pipeline.identity.get(now()).is_err());
    }
}

#[test]
fn pipeline_teardown_joins_the_owned_identity_worker() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    let mut delay = start_delayed_initialization(&mut pipeline, &mut state);
    let (dropping, dropping_wait) = std::sync::mpsc::channel();
    let (dropped, dropped_wait) = std::sync::mpsc::channel();
    let teardown = std::thread::spawn(move || {
        dropping.send(()).unwrap();
        drop(pipeline);
        dropped.send(()).unwrap();
    });
    dropping_wait.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        dropped_wait
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );
    delay.finish();
    dropped_wait.recv_timeout(Duration::from_secs(2)).unwrap();
    teardown.join().unwrap();
    assert_eq!(state.writes, 0);
}

#[test]
fn missing_configured_identity_provisioning_yields_ticks_and_invalidates_stale_tls() {
    let (directory, effective) = configured_fixture_files();
    let mut harness = HostConfigurationHarness::new(effective.clone());
    harness.state_document = Some(stored_document_with_fixture_identity(json!([])));
    let mut pipeline =
        Pipeline::with_host_configuration(settings(), Arc::new(Shared::default()), true);
    let time = now();
    assert_eq!(
        harness.request(&mut pipeline, "console", json!({"action":"identity"}), time)["ok"],
        true
    );
    assert!(pipeline.identity.get(time).is_ok());
    fs::remove_file(effective["cert"].as_str().unwrap()).unwrap();
    fs::remove_file(effective["pkey"].as_str().unwrap()).unwrap();
    let (mut delay, loader) = DelayedIdentityWork::generator(move || {
        ConfiguredIdentity::load(
            effective["cert"].as_str().unwrap(),
            effective["pkey"].as_str().unwrap(),
            4,
            time,
        )
    });
    pipeline.configured_identity_loader = Some(loader);
    let refresh_time = time + HOST_CONFIGURATION_REFRESH_SECONDS;
    for _ in 0..3 {
        pipeline
            .tick_at(refresh_time, |command| harness.send(command))
            .unwrap();
    }
    delay.wait_started();
    assert!(matches!(
        pipeline.host_configuration,
        HostConfigurationState::LoadingIdentity(_)
    ));
    assert!(pipeline.identity.get(refresh_time).is_err());
    let started = Instant::now();
    for _ in 0..20 {
        pipeline
            .tick_at(refresh_time, |command| harness.send(command))
            .unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        pipeline.host_configuration,
        HostConfigurationState::LoadingIdentity(_)
    ));
    delay.finish();
    let deadline = Instant::now() + Duration::from_secs(30);
    while matches!(
        pipeline.host_configuration,
        HostConfigurationState::LoadingIdentity(_)
    ) && Instant::now() < deadline
    {
        pipeline
            .tick_at(refresh_time, |command| harness.send(command))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        pipeline.host_configuration,
        HostConfigurationState::Ready { .. }
    ));
    assert_eq!(harness.state_writes, 0);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn certificate_permissions_are_administered_with_revision_and_survive_restart() {
    let mut state = Harness::new();
    let mut pipeline = Pipeline::new(settings());
    state.initialize(&mut pipeline);
    import_client(&mut state, &mut pipeline, "moonlight", CLIENT_CERT);
    let listed = state.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(listed["permission_mode_enabled"], false);
    let certificate_id = listed["clients"][0]["certificate_id"].clone();
    let revision = listed["revision"].as_u64().unwrap();
    let permissions =
        json!({"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false});
    let update = json!({"action":"update_client_permissions","certificate_id":certificate_id,"permissions":permissions,"expected_revision":revision});
    assert_eq!(
        state.request(&mut pipeline, "nvhttp", update.clone(), now())["code"],
        "forbidden"
    );
    assert_eq!(
        state.request(&mut pipeline, "console", update.clone(), now())["ok"],
        true
    );
    assert_eq!(
        state.request(&mut pipeline, "console", update, now())["code"],
        "revision_conflict"
    );
    let mode =
        json!({"action":"set_permission_mode","enabled":true,"expected_revision":state.revision});
    assert_eq!(
        state.request(&mut pipeline, "console", mode, now())["ok"],
        true
    );
    let mut restarted = Pipeline::new(settings());
    let checked = check_client(&mut state, &mut restarted, CLIENT_CERT);
    assert_eq!(checked["permissions"], permissions);
    assert_eq!(checked["permission_mode_enabled"], true);
    assert_eq!(checked["certificate_id"], certificate_id);
}

#[test]
fn old_pairing_documents_default_to_full_client_permissions_and_disabled_mode() {
    let document = stored_document_with_fixture_identity(paired_fixture_client());
    let parsed: crate::protocol::Document = serde_json::from_value(document).unwrap();
    assert!(!parsed.permission_mode_enabled);
    assert_eq!(
        parsed.clients[0].permissions,
        crate::protocol::ClientPermissions::default()
    );
}

#[test]
fn permission_write_suspends_authority_until_acknowledged_or_refreshed() {
    use amaterasu_identity::{IDENTITY_FORBIDDEN, PERMISSION_LAUNCH};
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let client = crypto_certificate_der(CLIENT_CERT);
    let mode =
        json!({"action":"set_permission_mode","enabled":true,"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", mode, now())["ok"],
        true
    );
    let old = pipeline.identity.get(now()).unwrap();
    assert_eq!(
        old.check_permission(&client, now(), PERMISSION_LAUNCH),
        Ok(())
    );
    harness.enqueue("console", json!({
        "action":"update_client_permissions", "certificate_id":certificate_id,
        "permissions":{"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false},
        "expected_revision":harness.revision
    }));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(!pipeline.identity.current(&old, now()));
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    let committed = pipeline.identity.get(now()).unwrap();
    assert_eq!(
        committed.check_permission(&client, now(), PERMISSION_LAUNCH),
        Err(IDENTITY_FORBIDDEN)
    );

    harness.fail_write = true;
    let mode = json!({"action":"set_permission_mode","enabled":false,"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", mode, now())["ok"],
        false
    );
    assert!(pipeline.identity.get(now()).is_err());
    assert_eq!(
        harness.request(
            &mut pipeline,
            "nvhttp",
            json!({"action":"check_client_certificate","certificate_pem":CLIENT_CERT}),
            now()
        )["ok"],
        true
    );
    assert_eq!(
        pipeline
            .identity
            .get(now())
            .unwrap()
            .check_permission(&client, now(), PERMISSION_LAUNCH),
        Err(IDENTITY_FORBIDDEN)
    );
}

#[test]
fn uncertain_write_recovers_committed_permissions_without_foreground_requests() {
    use amaterasu_identity::{IDENTITY_FORBIDDEN, PERMISSION_LAUNCH};
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let client = crypto_certificate_der(CLIENT_CERT);
    // Simulate a different committed policy followed by an uncertain write result.
    let stored = harness.document.as_mut().unwrap();
    stored["permission_mode_enabled"] = json!(true);
    stored["clients"][0]["permissions"] =
        json!({"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false});
    harness.revision += 1;
    harness.fail_write = true;
    let request = json!({"action":"update_client_permissions","certificate_id":certificate_id,"permissions":{"list_apps":true,"view_streams":true,"launch_apps":true,"terminate_streams":true},"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", request, now())["ok"],
        false
    );
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(
        pipeline
            .identity
            .get(now())
            .unwrap()
            .check_permission(&client, now(), PERMISSION_LAUNCH),
        Err(IDENTITY_FORBIDDEN)
    );
}

#[test]
fn recovery_read_overlapped_by_permission_write_cannot_restore_old_policy() {
    use amaterasu_identity::{IDENTITY_FORBIDDEN, PERMISSION_LAUNCH};
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let client = crypto_certificate_der(CLIENT_CERT);
    harness.fail_write = true;
    let mode =
        json!({"action":"set_permission_mode","enabled":true,"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", mode.clone(), now())["ok"],
        false
    );
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(harness.recovery_reply.is_some());
    harness.fail_write = false;
    assert_eq!(
        harness.request(&mut pipeline, "console", mode, now())["ok"],
        true
    );
    let update = json!({"action":"update_client_permissions","certificate_id":certificate_id,"permissions":{"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false},"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", update, now())["ok"],
        true
    );
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(harness.recovery_reply.is_none());
    assert_eq!(
        pipeline
            .identity
            .get(now())
            .unwrap()
            .check_permission(&client, now(), PERMISSION_LAUNCH),
        Err(IDENTITY_FORBIDDEN)
    );
}

#[test]
fn uncertain_write_recovery_observes_committed_unpair() {
    let (mut pipeline, mut harness, _) = paired_native_client();
    let client = crypto_certificate_der(CLIENT_CERT);
    harness.document.as_mut().unwrap()["clients"] = json!([]);
    harness.revision += 1;
    harness.fail_write = true;
    let mode =
        json!({"action":"set_permission_mode","enabled":true,"expected_revision":harness.revision});
    assert_eq!(
        harness.request(&mut pipeline, "console", mode, now())["ok"],
        false
    );
    assert!(pipeline.identity.get(now()).is_err());
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert!(
        !pipeline
            .identity
            .get(now())
            .unwrap()
            .verify_client(&client, now())
    );
}

#[test]
fn pairing_mutation_fences_and_waits_for_effect_owner_completion() {
    let (mut pipeline, mut harness, certificate_id, epoch, source, scope) =
        authorized_feedback_for_test();
    let admission = pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"send-1","sequence":1}),now()).unwrap();
    let ticket = admission["ticket"].as_str().unwrap().to_owned();
    let writes = harness.writes;
    harness.enqueue("console",json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.writes, writes);
    assert!(pipeline.deferred_effect_mutation.is_some());
    assert_eq!(pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"send-2","sequence":2}),now()),Err("source_denied"));
    let lookup = pipeline.apply_effect_request("input","input",json!({"action":"lookup_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"send-1"}),now()).unwrap();
    assert_eq!(lookup["ticket"], ticket);
    assert_eq!(lookup["fenced"], true);
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"});
    harness.incoming = Some(Response::Request {
        id: 8,
        caller: "input".into(),
        initiator: "input".into(),
        service: EFFECT_SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"send-1","ticket":ticket,"retirement":retirement}),
    });
    let mut receipt = None;
    pipeline
        .tick_at(now(), |command| match command {
            Command::Complete {
                request: 8,
                payload,
            } => {
                receipt = Some(payload);
                Ok(Response::Ok)
            }
            command => harness.send(command),
        })
        .unwrap();
    assert_eq!(receipt.unwrap()["ok"], true);
    assert_eq!(harness.writes, writes);
    let generation = pipeline.effect_gate.as_ref().unwrap().fence_generation();
    pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    for _ in 0..5 {
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        if harness.output.is_some() {
            break;
        }
    }
    assert_eq!(harness.output.unwrap()["ok"], true);
    assert_eq!(harness.writes, writes + 1);
}
#[test]
fn host_configuration_refresh_prevents_reopening_effect_admission() {
    let mut configured = settings();
    configured.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let mut pipeline =
        Pipeline::with_host_configuration(configured, Arc::new(Shared::default()), true);
    pipeline.host_configuration = HostConfigurationState::WaitingGet(42);
    let epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let mut incoming = Some(Response::Request {
        id: 8,
        caller: "input".into(),
        initiator: "input".into(),
        service: EFFECT_SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"recover","epoch":epoch,"owner_epoch":"retired-owner"}),
    });
    let mut result = None;
    pipeline
        .tick_at(now(), |command| {
            Ok(match command {
                Command::TakeResult { request: 42 } => Response::Pending,
                Command::Poll {} => incoming.take().unwrap_or(Response::Pending),
                Command::CheckRequest { request: 8 } => Response::Ok,
                Command::Complete {
                    request: 8,
                    payload,
                } => {
                    result = Some(payload);
                    Response::Ok
                }
                other => panic!("unexpected command: {other:?}"),
            })
        })
        .unwrap();
    assert_eq!(result.unwrap()["code"], "host_configuration_unavailable");
    let gate = pipeline.effect_gate.as_ref().unwrap();
    assert_eq!(gate.epoch(), epoch);
    assert!(gate.startup_recovery_required());
    assert!(!gate.drained());
}

#[test]
fn pairing_effect_service_checks_broker_identity_and_current_certificate() {
    let (mut pipeline, _harness, _) = paired_native_client();
    pipeline.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let startup_epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let recover = json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner"});
    assert_eq!(
        pipeline.apply_effect_request("input", "foreign", recover.clone(), now()),
        Err("forbidden")
    );
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", recover.clone(), now()),
        Err("forbidden")
    );
    let response = pipeline
        .apply_effect_request("input", "input", recover, now())
        .unwrap();
    let epoch = response["epoch"].as_str().unwrap().to_owned();
    let source = json!({"version":1,"id":"ab".repeat(16),"generation":1,
        "host_epoch":"host","session_id":"session","stream_id":"stream",
        "stream_generation":1,"device":"device","device_generation":1,
        "effect_owner_epoch":"owner"});
    assert_eq!(
        pipeline.apply_effect_request(
            "nvhttp",
            "nvhttp",
            json!({"action":"bind","epoch":epoch,"certificate_pem":HOST_CERT,"source":source}),
            now()
        ),
        Err("source_denied")
    );
    pipeline
        .apply_effect_request(
            "nvhttp",
            "nvhttp",
            json!({"action":"bind","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source}),
            now(),
        )
        .unwrap();
    pipeline.identity.invalidate();
    assert_eq!(
        pipeline.apply_effect_request(
            "input",
            "input",
            json!({"action":"admit","epoch":epoch,"source":source,"operation_id":"op","sequence":1}),
            now()
        ),
        Err("scope_required")
    );
}

#[test]
fn canceled_pairing_mutation_does_not_release_inflight_effects() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.recover("owner").unwrap();
    let source = crate::effect_gate::Source {
        version: 1,
        id: "ab".repeat(16),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    gate.bind(source.clone()).unwrap();
    let ticket = gate.admit(&source, "send", 1).unwrap().to_owned();
    let epoch = gate.epoch().to_owned();
    let writes = harness.writes;
    harness.enqueue("console", json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    harness.check_cancelled = true;
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.output.unwrap()["code"], "request_ended");
    assert_eq!(harness.writes, writes);
    let gate = pipeline.effect_gate.as_mut().unwrap();
    assert_eq!(gate.admit(&source, "new", 2), Err("admission_denied"));
    assert_eq!(
        gate.acknowledge_drain(&epoch, gate.fence_generation()),
        Err("effects_in_flight")
    );
    gate.complete(&epoch, "send", &ticket).unwrap();
    assert!(!gate.drained());
    gate.acknowledge_drain(&epoch, gate.fence_generation())
        .unwrap();
    assert!(gate.drained());
}

#[test]
fn identity_invalidation_fences_effects_and_blocks_republication_until_drain() {
    let (mut pipeline, harness, _) = paired_native_client();
    let snapshot = pipeline.identity.get(now()).unwrap();
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.recover("owner").unwrap();
    let source = crate::effect_gate::Source {
        version: 1,
        id: "ab".repeat(16),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    gate.bind(source.clone()).unwrap();
    let ticket = gate.admit(&source, "send", 1).unwrap().to_owned();
    let epoch = gate.epoch().to_owned();
    pipeline.invalidate_identity();
    assert!(!pipeline.identity.current(&snapshot, now()));
    assert_eq!(
        pipeline.publish_identity(harness.revision, Some(snapshot.clone())),
        Err("effect_retirement_pending")
    );
    let gate = pipeline.effect_gate.as_mut().unwrap();
    assert_eq!(gate.admit(&source, "other", 2), Err("admission_denied"));
    gate.complete(&epoch, "send", &ticket).unwrap();
    assert_eq!(
        pipeline.publish_identity(harness.revision, Some(snapshot.clone())),
        Err("effect_retirement_pending")
    );
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.acknowledge_drain(&epoch, gate.fence_generation())
        .unwrap();
    pipeline
        .publish_identity(harness.revision, Some(snapshot))
        .unwrap();
}

#[test]
fn configured_authority_restart_requires_owner_retirement_before_unpair_commit() {
    let (_old, mut harness, certificate_id) = paired_native_client();
    let mut configured = settings();
    configured.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let mut restarted = Pipeline::new(configured);
    let writes = harness.writes;
    let epoch = restarted.effect_gate.as_ref().unwrap().epoch().to_owned();
    assert!(!restarted.effect_gate.as_ref().unwrap().drained());
    harness.enqueue("console", json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    restarted
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.writes, writes);
    assert!(restarted.deferred_effect_mutation.is_some());
    harness.incoming = Some(Response::Request {
        id: 8,
        caller: "input".into(),
        initiator: "input".into(),
        service: EFFECT_SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"recover","epoch":epoch,"owner_epoch":"retired-old-owner"}),
    });
    let mut result = None;
    restarted
        .tick_at(now(), |command| match command {
            Command::Complete {
                request: 8,
                payload,
            } => {
                result = Some(payload);
                Ok(Response::Ok)
            }
            command => harness.send(command),
        })
        .unwrap();
    assert_eq!(result.unwrap()["ok"], true);
    assert_eq!(harness.writes, writes);
    for _ in 0..5 {
        restarted
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        if harness.output.is_some() {
            break;
        }
    }
    assert_eq!(harness.output.unwrap()["ok"], true);
    assert_eq!(harness.writes, writes + 1);
}

#[test]
fn unavailable_configured_authority_cannot_bypass_unpair_fence() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    pipeline.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    pipeline.effect_gate = None;
    assert!(!pipeline.authority_available());
    let writes = harness.writes;
    harness.enqueue("console", json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.output.unwrap()["code"], "authority_unavailable");
    assert_eq!(harness.writes, writes);
}

#[test]
fn request_canceled_after_delivery_cannot_fence_a_trust_mutation() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    pipeline.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.recover("owner").unwrap();
    let source = crate::effect_gate::Source {
        version: 1,
        id: "ab".repeat(16),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    gate.bind(source.clone()).unwrap();
    let writes = harness.writes;
    harness.enqueue("console", json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    harness.check_cancelled = true;
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.output.unwrap()["code"], "request_ended");
    assert_eq!(harness.writes, writes);
    assert!(
        pipeline
            .effect_gate
            .as_mut()
            .unwrap()
            .admit(&source, "after-cancel", 1)
            .is_ok()
    );
}

#[test]
fn canceled_effect_request_cannot_admit_or_acknowledge_drain() {
    let (mut pipeline, mut harness, _, epoch, source, scope) = authorized_feedback_for_test();
    let mut request = Some(Response::Request {
        id: 8,
        caller: "input".into(),
        initiator: "input".into(),
        service: EFFECT_SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled","sequence":1}),
    });
    let mut output = None;
    pipeline
        .tick_at(now(), |command| match command {
            Command::Poll {} => Ok(request.take().unwrap_or(Response::Pending)),
            Command::CheckRequest { request: 8 } => {
                Ok(Response::error("request_ended", "cancelled"))
            }
            Command::Complete {
                request: 8,
                payload,
            } => {
                output = Some(payload);
                Ok(Response::Ok)
            }
            command => harness.send(command),
        })
        .unwrap();
    assert_eq!(output.unwrap()["code"], "request_ended");
    assert_eq!(pipeline.effect_gate.as_ref().unwrap().in_flight(), 0);
    let allowed=pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"valid","sequence":1}),now()).unwrap();
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"});
    pipeline.apply_effect_request("input","input",json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"valid","ticket":allowed["ticket"],"retirement":retirement}),now()).unwrap();
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.fence();
    let generation = gate.fence_generation();
    let mut request = Some(Response::Request {
        id: 8,
        caller: "input".into(),
        initiator: "input".into(),
        service: EFFECT_SERVICE.into(),
        remaining_ms: 30_000,
        payload: json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
    });
    pipeline
        .tick_at(now(), |command| match command {
            Command::Poll {} => Ok(request.take().unwrap_or(Response::Pending)),
            Command::CheckRequest { request: 8 } => {
                Ok(Response::error("request_ended", "cancelled"))
            }
            Command::Complete { request: 8, .. } => Ok(Response::Ok),
            command => harness.send(command),
        })
        .unwrap();
    assert!(pipeline.effect_gate.as_ref().unwrap().drain_required());
}
#[test]
fn real_broker_startup_recovery_blocks_pairing_mutation_until_owner_retires() {
    let (_old, harness, certificate_id) = paired_native_client();
    let mut document = harness.document.unwrap();
    let mut revision = harness.revision;
    let mut configured = settings();
    configured.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let mut pipeline = Pipeline::new(configured);
    let mut broker = authority_broker();
    let Response::Accepted { id: request } = broker.dispatch("console", Command::Request {
        service: SERVICE.into(), payload: json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":revision}),
        timeout_ms: 30_000, operation_id: "unpair-real".into(), parent: None,
    }) else { panic!("pairing request not accepted") };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert!(matches!(
        broker.dispatch("console", Command::TakeResult { request }),
        Response::Pending
    ));
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    let epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let Response::Accepted { id: recovery } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: json!({"action":"recover","epoch":epoch,"owner_epoch":"retired-owner"}),
            timeout_ms: 30_000,
            operation_id: "recover-real".into(),
            parent: None,
        },
    ) else {
        panic!("recovery not accepted")
    };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert!(
        matches!(broker.dispatch("input", Command::TakeResult {request:recovery}), Response::Result { payload } if payload["ok"] == true)
    );
    let mut writes = 0;
    let mut answer = None;
    for _ in 0..16 {
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        if let Response::Request { id, payload, .. } = broker.dispatch("state", Command::Poll {}) {
            let response = if payload["action"] == "get" {
                json!({"ok":true,"revision":revision,"found":true,"value":document})
            } else {
                assert_eq!(payload["expected_revision"], revision);
                writes += 1;
                document = payload["value"].clone();
                revision += 1;
                json!({"ok":true,"revision":revision})
            };
            assert!(matches!(
                broker.dispatch(
                    "state",
                    Command::Complete {
                        request: id,
                        payload: response
                    }
                ),
                Response::Ok
            ));
        }
        if let Response::Result { payload } =
            broker.dispatch("console", Command::TakeResult { request })
        {
            answer = Some(payload);
            break;
        }
    }
    assert_eq!(answer.unwrap()["ok"], true);
    assert_eq!(writes, 1);
    assert!(document["clients"].as_array().unwrap().is_empty());
}

#[test]
fn real_broker_cancel_between_delivery_and_check_preserves_open_authority() {
    let (mut pipeline, _harness, certificate_id) = paired_native_client();
    pipeline.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.recover("owner").unwrap();
    let source = crate::effect_gate::Source {
        version: 1,
        id: "ab".repeat(16),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    gate.bind(source.clone()).unwrap();
    let mut broker = authority_broker();
    let Response::Accepted { id } = broker.dispatch(
        "console",
        Command::Request {
            service: SERVICE.into(),
            payload: json!({"action":"unpair","certificate_id":certificate_id}),
            timeout_ms: 30_000,
            operation_id: "cancel-after-delivery".into(),
            parent: None,
        },
    ) else {
        panic!("pairing request not accepted")
    };
    let mut canceled = false;
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result, Response::Request { id: delivered, .. } if *delivered == id) {
                assert!(matches!(
                    broker.dispatch("console", Command::Cancel { request: id }),
                    Response::Ok
                ));
                canceled = true;
            }
            Ok(result)
        })
        .unwrap();
    assert!(canceled);
    assert!(matches!(
        broker.dispatch("state", Command::Poll {}),
        Response::Pending
    ));
    assert!(
        pipeline
            .effect_gate
            .as_mut()
            .unwrap()
            .admit(&source, "still-authorized", 1)
            .is_ok()
    );
}

#[test]
fn real_broker_cancelled_effect_requests_cannot_admit_or_acknowledge_drain() {
    let (mut pipeline, _harness, _, epoch, source, scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let Response::Accepted{id}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),
        payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled","sequence":1}),
        timeout_ms:30_000,operation_id:"cancel-admission".into(),parent:None}) else {panic!("request rejected")};
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result,Response::Request{id:delivered,..} if *delivered==id) {
                assert!(matches!(
                    broker.dispatch("input", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    assert_eq!(pipeline.effect_gate.as_ref().unwrap().in_flight(), 0);
    let allowed=pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"valid","sequence":1}),now()).unwrap();
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"});
    pipeline.apply_effect_request("input","input",json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"valid","ticket":allowed["ticket"],"retirement":retirement}),now()).unwrap();
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.fence();
    let generation = gate.fence_generation();
    let Response::Accepted { id: drain } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
            timeout_ms: 30_000,
            operation_id: "cancel-drain".into(),
            parent: None,
        },
    ) else {
        panic!("drain rejected")
    };
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result,Response::Request{id:delivered,..} if *delivered==drain) {
                assert!(matches!(
                    broker.dispatch("input", Command::Cancel { request: drain }),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    assert!(pipeline.effect_gate.as_ref().unwrap().drain_required());
}
#[test]
fn trust_mutation_waits_for_stream_lease_retirement_and_owner_drain() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    pipeline.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let gate = pipeline.effect_gate.as_mut().unwrap();
    gate.recover("owner").unwrap();
    let source = crate::effect_gate::Source {
        version: 1,
        id: "ab".repeat(16),
        generation: 1,
        host_epoch: "host".into(),
        session_id: "session".into(),
        stream_id: "stream".into(),
        stream_generation: 1,
        device: "device".into(),
        device_generation: 1,
        effect_owner_epoch: "owner".into(),
    };
    let scope = crate::effect_gate::StreamScope {
        capabilities: [amaterasu_module_sdk::effect_authority::FeedbackCapability {
            schema: "speaker.pcm".into(),
            version: 1,
            direction: amaterasu_module_sdk::effect_authority::FeedbackDirection::ToClient,
        }]
        .into(),
        controller_slot: 3,
        transport_service: "amaterasu.control-transport/1".into(),
        transport_stream: 7,
        transport_epoch: "c".repeat(32),
    };
    gate.bind_scoped(source.clone(), Some(scope.clone()))
        .unwrap();
    let ticket = gate
        .admit_stream_lease(&source, "lease", 1, scope.clone())
        .unwrap()
        .to_owned();
    let epoch = gate.epoch().to_owned();
    let writes = harness.writes;
    harness.enqueue("console", json!({"action":"unpair","certificate_id":certificate_id,"expected_revision":harness.revision}));
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.writes, writes);
    assert!(
        pipeline
            .effect_gate
            .as_ref()
            .unwrap()
            .lookup_stream_lease(&source, "lease", &scope)
            .unwrap()
            .1
    );
    let mut retirement = crate::effect_gate::StreamRetirement {
        ok: true,
        service: scope.transport_service.clone(),
        stream: scope.transport_stream + 1,
        provider_epoch: scope.transport_epoch.clone(),
        operation_id: "retire".into(),
        scope: "whole_control_stream".into(),
        state: "retired".into(),
    };
    assert_eq!(pipeline.apply_effect_request("input", "input", json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"lease","ticket":ticket,"retirement":retirement}), now()), Err("retirement_mismatch"));
    assert_eq!(harness.writes, writes);
    retirement.stream = scope.transport_stream;
    pipeline.apply_effect_request("input", "input", json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"lease","ticket":ticket,"retirement":retirement}), now()).unwrap();
    pipeline
        .tick_at(now(), |command| harness.send(command))
        .unwrap();
    assert_eq!(harness.writes, writes);
    let generation = pipeline.effect_gate.as_ref().unwrap().fence_generation();
    pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
            now(),
        )
        .unwrap();
    for _ in 0..5 {
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        if harness.output.is_some() {
            break;
        }
    }
    assert_eq!(harness.output.unwrap()["ok"], true);
    assert_eq!(harness.writes, writes + 1);
}

#[test]
fn stream_binding_cannot_rebind_source_to_another_certificate_or_capability_scope() {
    let (mut pipeline, mut harness, _) = paired_native_client();
    assert_eq!(harness.request(&mut pipeline, "console", json!({"action":"import_client","client_id":"other-client","name":"Other", "certificate_pem":HOST_CERT}), now())["ok"], true);
    let first_id = crate::crypto::parse_certificate(CLIENT_CERT, None)
        .unwrap()
        .fingerprint;
    let other_id = crate::crypto::parse_certificate(HOST_CERT, None)
        .unwrap()
        .fingerprint;
    grant_feedback_for_test(&mut harness, &mut pipeline, &first_id, &["speaker.pcm"]);
    grant_feedback_for_test(&mut harness, &mut pipeline, &other_id, &["speaker.pcm"]);
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let startup_epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let result = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":startup_epoch,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap();
    let epoch = result["epoch"].as_str().unwrap();
    let source = json!({"version":1,"id":"ab".repeat(16),"generation":1,"host_epoch":"host",
        "session_id":"session","stream_id":"stream","stream_generation":1,"device":"device",
        "device_generation":1,"effect_owner_epoch":"owner"});
    let scope = json!({"capabilities":[{"schema":"speaker.pcm","version":1,"direction":"to_client"}],"controller_slot":3,"transport_service":"amaterasu.control-transport/1",
        "transport_stream":7,"transport_epoch":"c".repeat(32)});
    let request = json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope});
    assert_eq!(
        pipeline
            .apply_effect_request("nvhttp", "nvhttp", request.clone(), now())
            .unwrap()["ok"],
        true
    );
    let mut foreign = request.clone();
    foreign["certificate_pem"] = json!(HOST_CERT);
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", foreign, now()),
        Err("source_owner_conflict")
    );
    let mut altered = request;
    altered["scope"]["controller_slot"] = json!(4);
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", altered, now()),
        Err("source_scope_conflict")
    );
}

#[test]
fn real_broker_stream_lease_returns_exact_selected_source_and_scope() {
    let (mut pipeline, _harness, _, epoch, source, scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let Response::Accepted {id} = broker.dispatch("input", Command::Request { service:EFFECT_SERVICE.into(),
        payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"lease","sequence":1}),
        timeout_ms:30_000, operation_id:"admit-stream-real".into(), parent:None }) else { panic!("request rejected") };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: id })
    else {
        panic!("missing receipt")
    };
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["source"], source);
    assert_eq!(payload["scope"], scope);
    assert_eq!(payload["kind"], "stream_lease");
    for schema in [
        "unknown.feedback",
        "input.keyboard",
        "usb.forward",
        "custom.extension",
    ] {
        let mut expanded = scope.clone();
        expanded["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(json!({"schema":schema,"version":1,"direction":"to_client"}));
        let Response::Accepted { id: rejected } = broker.dispatch("input", Command::Request {
            service:EFFECT_SERVICE.into(), payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":expanded,"operation_id":format!("extra-{schema}"),"sequence":2}),
            timeout_ms:30_000, operation_id:format!("broker-{schema}"), parent:None,
        }) else { panic!("request rejected by transport") };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch("input", Command::TakeResult { request: rejected })
        else {
            panic!("missing denial")
        };
        assert_eq!(payload["code"], "scope_denied");
    }
    let mut input_scope = scope.clone();
    input_scope["capabilities"][0]["direction"] = json!("to_device");
    let Response::Accepted { id: rejected } = broker.dispatch("input",Command::Request {
        service:EFFECT_SERVICE.into(),payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":input_scope,"operation_id":"input","sequence":2}),
        timeout_ms:30_000,operation_id:"broker-input-direction".into(),parent:None,
    }) else { panic!("request rejected by transport") };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: rejected })
    else {
        panic!("missing denial")
    };
    assert_eq!(payload["code"], "invalid_request");
    assert_eq!(pipeline.effect_gate.as_ref().unwrap().in_flight(), 1);
}

fn feedback_capability(schema: &str) -> amaterasu_module_sdk::effect_authority::FeedbackCapability {
    amaterasu_module_sdk::effect_authority::FeedbackCapability {
        schema: schema.into(),
        version: 1,
        direction: amaterasu_module_sdk::effect_authority::FeedbackDirection::ToClient,
    }
}

fn grant_feedback_for_test(
    harness: &mut Harness,
    pipeline: &mut Pipeline,
    certificate_id: &str,
    capabilities: &[&str],
) {
    let grants: Vec<_> = capabilities
        .iter()
        .map(|schema| json!({"schema":schema,"version":1,"direction":"to_client"}))
        .collect();
    let response = harness.request(
        pipeline,
        "console",
        json!({"action":"update_client_feedback_grants",
        "certificate_id":certificate_id,"expected_revision":harness.revision,"grants":grants}),
        now(),
    );
    assert_eq!(response["ok"], true, "{response}");
}

fn policy_for_feedback(
    schemas: &[&str],
    required_permission: crate::protocol::FeedbackPermission,
) -> crate::protocol::EffectAuthoritySettings {
    crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: schemas
            .iter()
            .map(|schema| crate::protocol::FeedbackPolicy {
                capability: feedback_capability(schema),
                required_permission,
            })
            .collect(),
    }
}

fn feedback_source_for_test(owner_epoch: &str) -> Value {
    json!({"version":1,"id":"ab".repeat(16),"generation":1,"host_epoch":"host", "session_id":"session",
        "stream_id":"stream","stream_generation":1,"device":"device","device_generation":1,"effect_owner_epoch":owner_epoch})
}
fn feedback_scope_for_test(schemas: &[&str]) -> Value {
    json!({"capabilities":schemas.iter().map(|schema| json!({"schema":schema,"version":1,"direction":"to_client"})).collect::<Vec<_>>(),
        "controller_slot":3,"transport_service":"amaterasu.control-transport/1","transport_stream":7,"transport_epoch":"c".repeat(32)})
}

#[test]
fn feedback_scope_requires_explicit_selected_policy_and_client_grant() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let bind = json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope});
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", bind, now()),
        Err("scope_denied")
    );
    assert_eq!(pipeline.apply_effect_request("input", "input", json!({"action":"admit","epoch":epoch,"source":source,"operation_id":"unscoped","sequence":1}), now()), Err("scope_required"));
    pipeline.effect_authority = None;
    grant_feedback_for_test(
        &mut harness,
        &mut pipeline,
        &certificate_id,
        &["speaker.pcm"],
    );
    pipeline.effect_authority = Some(policy_for_feedback(
        &[],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner-2"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner-2");
    let bind = json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope});
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", bind, now()),
        Err("scope_denied")
    );
}

#[test]
fn feedback_preserves_view_only_and_explicit_controller_restriction() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    grant_feedback_for_test(
        &mut harness,
        &mut pipeline,
        &certificate_id,
        &["speaker.pcm"],
    );
    let permissions =
        json!({"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false});
    assert_eq!(harness.request(&mut pipeline,"console",json!({"action":"update_client_permissions","certificate_id":certificate_id,"permissions":permissions,"expected_revision":harness.revision}),now())["ok"],true);
    assert_eq!(harness.request(&mut pipeline,"console",json!({"action":"set_permission_mode","enabled":true,"expected_revision":harness.revision}),now())["ok"],true);
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    let bind = json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope});
    assert_eq!(
        pipeline.apply_effect_request("nvhttp", "nvhttp", bind.clone(), now()),
        Err("scope_denied")
    );
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::View,
    ));
    assert_eq!(
        pipeline
            .apply_effect_request("nvhttp", "nvhttp", bind, now())
            .unwrap()["ok"],
        true
    );
    // A fresh configuration with an explicitly disabled controller must stay denied even if the capability is granted.
    let (mut restricted, mut state, id) = paired_native_client();
    grant_feedback_for_test(&mut state, &mut restricted, &id, &["speaker.pcm"]);
    assert_eq!(state.request(&mut restricted,"console",json!({"action":"update_client_config_overrides","certificate_id":id,"config_overrides":{"controller":false},"expected_revision":state.revision}),now())["ok"],true);
    restricted.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::View,
    ));
    let initial = restricted.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = restricted
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(restricted.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":feedback_source_for_test("owner"),"scope":feedback_scope_for_test(&["speaker.pcm"])}),now()),Err("scope_denied"));
}

pub(super) fn authorized_feedback_for_test() -> (Pipeline, Harness, String, String, Value, Value) {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    grant_feedback_for_test(
        &mut harness,
        &mut pipeline,
        &certificate_id,
        &["speaker.pcm"],
    );
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    let bound = pipeline.apply_effect_request("nvhttp", "nvhttp", json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}), now()).unwrap();
    assert_eq!(bound["ok"], true);
    (pipeline, harness, certificate_id, epoch, source, scope)
}
fn authority_configuration() -> amaterasu_contracts::RuntimeConfig {
    use amaterasu_contracts::{Grants, RuntimeConfig};
    let mut configuration = RuntimeConfig::default();
    configuration.principals.insert(
        "pairing".into(),
        Grants {
            provide: [SERVICE.into(), EFFECT_SERVICE.into()].into(),
            call: [STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "state".into(),
        Grants {
            provide: [STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "console".into(),
        Grants {
            call: [SERVICE.into(), STATE_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "input".into(),
        Grants {
            call: [EFFECT_SERVICE.into(), "amaterasu.nvhttp/1".into()].into(),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "nvhttp".into(),
        Grants {
            provide: ["amaterasu.nvhttp/1".into()].into(),
            call: [EFFECT_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    configuration.principals.insert(
        "foreign".into(),
        Grants {
            call: [EFFECT_SERVICE.into()].into(),
            ..Default::default()
        },
    );
    configuration
}

fn broker_for_authority(
    configuration: amaterasu_contracts::RuntimeConfig,
) -> amaterasu_runtime::Broker {
    let mut broker = amaterasu_runtime::Broker::new(configuration).unwrap();
    for (principal, service) in [
        ("pairing", SERVICE),
        ("pairing", EFFECT_SERVICE),
        ("state", STATE_SERVICE),
        ("nvhttp", "amaterasu.nvhttp/1"),
    ] {
        assert!(matches!(
            broker.dispatch(
                principal,
                Command::Register {
                    service: service.into()
                }
            ),
            Response::Ok
        ));
    }
    broker
}

fn authority_broker() -> amaterasu_runtime::Broker {
    broker_for_authority(authority_configuration())
}

fn protocol_authority_broker(method_schema: Value) -> amaterasu_runtime::Broker {
    use amaterasu_contracts::{
        ApiMethod, ApiMethodApproval, Grants,
        effect_fence::{EffectFencePolicy, ProtectedMethod},
    };
    let mut config = authority_configuration();
    config
        .principals
        .get_mut("input")
        .unwrap()
        .call
        .insert("example.effects/1".into());
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: ["example.effects/1".into()].into(),
            ..Default::default()
        },
    );
    config.host_context.provider_incarnations.insert(
        "provider".into(),
        amaterasu_contracts::effect_fence::ProviderIncarnation {
            version: 1,
            id: "e".repeat(32),
        },
    );
    let method: ApiMethod = serde_json::from_value(method_schema).unwrap();
    config.api_methods.push(ApiMethodApproval {
        provider: "provider".into(),
        callers: ["input".into()].into(),
        method: method.clone(),
    });
    config.effect_fence = Some(EffectFencePolicy {
        authority: "pairing".into(),
        issuer: "input".into(),
        source_slots: false,
        methods: vec![ProtectedMethod {
            service: "example.effects/1".into(),
            method: "apply".into(),
            interface_major: 1,
        }],
        cleanup_methods: vec![],
    });
    let mut broker = broker_for_authority(config);
    assert!(matches!(
        broker.dispatch("provider", Command::RegisterApi { method }),
        Response::Ok
    ));
    broker
}

#[test]
fn real_pairing_broker_fences_one_source_until_owner_retirement() {
    let (mut pipeline, _harness, _certificate, epoch, source, scope) =
        authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut sequence = 0_u64;
    let mut call = |pipeline: &mut Pipeline, caller: &str, payload: Value| {
        sequence += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: EFFECT_SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("broker-source-retirement-{sequence}"),
                parent: None,
            },
        ) else {
            panic!("request not accepted");
        };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("no response");
        };
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    };
    let admitted = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"lease-one","sequence":1}),
    );
    let ticket = admitted["ticket"].as_str().unwrap();
    let fence = json!({"action":"fence_source","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one"});
    assert_eq!(
        call(&mut pipeline, "foreign", fence.clone())["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut pipeline, "nvhttp", fence.clone())["state"],
        "fenced"
    );
    assert_eq!(call(&mut pipeline, "nvhttp", fence)["state"], "fenced");
    let discovered = call(
        &mut pipeline,
        "nvhttp",
        json!({"action":"lookup_current_source_retirement","epoch":epoch,"source":source,"scope":scope}),
    );
    assert_eq!(discovered["operation_id"], "retire-one");
    assert_eq!(discovered["state"], "fenced");
    let late = call(
        &mut pipeline,
        "nvhttp",
        json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}),
    );
    assert_eq!(late["code"], "source_denied");
    let late_admit = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"late","sequence":2}),
    );
    assert_eq!(late_admit["code"], "source_denied");
    let finish = json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one","retirement":null});
    assert_eq!(
        call(&mut pipeline, "nvhttp", finish.clone())["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut pipeline, "input", finish.clone())["code"],
        "effects_in_flight"
    );
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
        "provider_epoch":"c".repeat(32),"operation_id":"transport-retire","scope":"whole_control_stream","state":"retired"});
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"lease-one","ticket":ticket,"retirement":retirement})
        )["ok"],
        true
    );
    assert_eq!(
        call(&mut pipeline, "input", finish)["code"],
        "lease_acknowledgment_pending"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"acknowledge","epoch":epoch,"operation_id":"lease-one","ticket":ticket})
        )["ok"],
        true
    );
    let finish = json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one","retirement":null});
    assert_eq!(
        call(&mut pipeline, "input", finish)["code"],
        "retirement_mismatch"
    );
    let finish = json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one","retirement":retirement});
    assert_eq!(
        call(&mut pipeline, "input", finish.clone())["state"],
        "retired"
    );
    assert_eq!(call(&mut pipeline, "input", finish)["state"], "retired");
    let lookup = call(
        &mut pipeline,
        "nvhttp",
        json!({"action":"lookup_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one"}),
    );
    assert_eq!(lookup["state"], "retired");
    assert!(lookup.get("certificate_pem").is_none());
    let restart = call(
        &mut pipeline,
        "nvhttp",
        json!({"action":"fence_protocol_restart","epoch":epoch,"operation_id":"restart-test"}),
    );
    assert_eq!(restart["state"], "fenced");
    let after = call(
        &mut pipeline,
        "input",
        json!({"action":"recover","epoch":epoch,"owner_epoch":"new-owner"}),
    );
    assert_eq!(after["ok"], true);
    assert_ne!(after["epoch"], epoch);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"lookup_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-one"})
        )["code"],
        "stale_epoch"
    );
    let receipt: amaterasu_module_sdk::effect_source_retirement::Receipt =
        serde_json::from_value(lookup).unwrap();
    assert!(receipt.matches(
        &epoch,
        &serde_json::from_value(source).unwrap(),
        &serde_json::from_value(scope).unwrap(),
        "retire-one"
    ));
}

#[test]
fn real_nvhttp_recovery_and_pairing_reconcile_a_cancelled_bind_through_broker() {
    use amaterasu_nvhttp::test_support::{SourceRecovery, SourceRecoveryOutcome};
    let (mut pairing, _state, _certificate, epoch, source, scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let request = amaterasu_module_sdk::nvhttp_source::BindFeedbackSource {
        authority_epoch: epoch.clone(),
        source: serde_json::from_value(source.clone()).unwrap(),
        scope: serde_json::from_value(scope.clone()).unwrap(),
        input_boot_epoch: "input-boot".into(),
    };
    let operation = request.retirement_operation_id();
    let Response::Accepted{id:lease}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),
        payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"combined-lease","sequence":1}),
        timeout_ms:30000,operation_id:"combined-lease-admission".into(),parent:None}) else {panic!("lease rejected")};
    pairing
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: lease })
    else {
        panic!("no lease response")
    };
    let ticket = payload["ticket"].as_str().unwrap().to_owned();
    let Response::Accepted { id: origin } = broker.dispatch(
        "input",
        Command::Request {
            service: "amaterasu.nvhttp/1".into(),
            payload: request.payload(),
            timeout_ms: 30000,
            operation_id: "uncertain-original".into(),
            parent: None,
        },
    ) else {
        panic!("original rejected")
    };
    assert!(matches!(
        broker.dispatch("nvhttp", Command::Poll {}),
        Response::Request { .. }
    ));
    assert!(matches!(
        broker.dispatch("input", Command::Cancel { request: origin }),
        Response::Ok
    ));
    let mut recovery = SourceRecovery::new(origin, request.clone());
    let mut fenced = false;
    for _ in 0..20 {
        if let SourceRecoveryOutcome::FenceSettled {
            fence_confirmed,
            operation_id,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("nvhttp", command)))
        {
            assert!(fence_confirmed);
            assert_eq!(operation_id, operation);
            fenced = true;
            break;
        }
        pairing
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
    }
    assert!(fenced);
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),
        "operation_id":"real-owner-fixture-retire","scope":"whole_control_stream","state":"retired"});
    let Response::Accepted{id:completed_lease}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),
        payload:json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"combined-lease","ticket":ticket,"retirement":retirement}),
        timeout_ms:30000,operation_id:"combined-lease-completion".into(),parent:None}) else {panic!("lease completion rejected")};
    pairing
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert!(matches!(
        broker.dispatch(
            "input",
            Command::TakeResult {
                request: completed_lease
            }
        ),
        Response::Result { .. }
    ));
    let Response::Accepted{id:ack}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),
        payload:json!({"action":"acknowledge","epoch":epoch,"operation_id":"combined-lease","ticket":ticket}),
        timeout_ms:30000,operation_id:"combined-lease-ack".into(),parent:None}) else {panic!("lease ack rejected")};
    pairing
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    assert!(matches!(
        broker.dispatch("input", Command::TakeResult { request: ack }),
        Response::Result { .. }
    ));
    let Response::Accepted{id:completion}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),
        payload:json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":operation,"retirement":retirement}),
        timeout_ms:30000,operation_id:"owner-retire-source".into(),parent:None}) else {panic!("completion rejected")};
    pairing
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } = broker.dispatch(
        "input",
        Command::TakeResult {
            request: completion,
        },
    ) else {
        panic!("no retirement result")
    };
    assert_eq!(payload["state"], "retired");
    let Response::Accepted { id: reconcile } = broker.dispatch(
        "input",
        Command::Request {
            service: "amaterasu.nvhttp/1".into(),
            payload: request.reconciliation_payload(),
            timeout_ms: 30000,
            operation_id: "owner-reconcile-source".into(),
            parent: None,
        },
    ) else {
        panic!("reconcile rejected")
    };
    assert!(matches!(
        broker.dispatch("nvhttp", Command::Poll {}),
        Response::Request { .. }
    ));
    recovery.begin_lookup(reconcile, &request).unwrap();
    let mut verified = false;
    for _ in 0..20 {
        if let SourceRecoveryOutcome::Reply {
            retired: true,
            payload,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("nvhttp", command)))
        {
            assert!(request.accepts_retirement_confirmation(&payload, &operation));
            verified = true;
            break;
        }
        pairing
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
    }
    assert!(verified);
    assert_eq!(pairing.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}),now()),Err("source_denied"));
}

#[test]
fn protocol_restart_requires_old_owner_drain_before_epoch_rotation() {
    let (mut pipeline, _harness, _certificate, epoch, source, scope) =
        authorized_feedback_for_test();
    let admitted=pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"restart-lease","sequence":1}),now()).unwrap();
    let ticket = admitted["ticket"].as_str().unwrap();
    let fence =
        json!({"action":"fence_protocol_restart","epoch":epoch,"operation_id":"restart-op"});
    assert_eq!(
        pipeline.apply_effect_request("input", "input", fence.clone(), now()),
        Err("forbidden")
    );
    let first = pipeline
        .apply_effect_request("nvhttp", "nvhttp", fence.clone(), now())
        .unwrap();
    assert_eq!(first["state"], "fenced");
    assert_eq!(
        pipeline
            .apply_effect_request("nvhttp", "nvhttp", fence, now())
            .unwrap(),
        first
    );
    assert_eq!(
        pipeline.apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":epoch,"owner_epoch":"new-owner"}),
            now()
        ),
        Err("recovery_required")
    );
    let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,
        "provider_epoch":"c".repeat(32),"operation_id":"restart-retire","scope":"whole_control_stream","state":"retired"});
    assert_eq!(pipeline.apply_effect_request("input","input",json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"restart-lease","ticket":ticket,"retirement":retirement}),now()).unwrap()["ok"],true);
    let generation = pipeline.effect_gate.as_ref().unwrap().fence_generation();
    assert_eq!(
        pipeline
            .apply_effect_request(
                "input",
                "input",
                json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
                now()
            )
            .unwrap()["ok"],
        true
    );
    let next = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":epoch,"owner_epoch":"new-owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(next, epoch);
    assert_eq!(
        pipeline.apply_effect_request(
            "nvhttp",
            "nvhttp",
            json!({"action":"fence_protocol_restart","epoch":epoch,"operation_id":"old"}),
            now()
        ),
        Err("stale_epoch")
    );
}

#[test]
fn delivered_then_cancelled_source_fence_and_completion_do_not_mutate_authority() {
    let (mut pipeline, _harness, _certificate, epoch, source, scope) =
        authorized_feedback_for_test();
    let mut broker = authority_broker();
    let fence = json!({"action":"fence_source","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled-fence"});
    let Response::Accepted { id } = broker.dispatch(
        "nvhttp",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: fence.clone(),
            timeout_ms: 30000,
            operation_id: "cancel-fence-delivered".into(),
            parent: None,
        },
    ) else {
        panic!("not accepted")
    };
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result,Response::Request{id:delivered,..} if *delivered==id) {
                assert!(matches!(
                    broker.dispatch("nvhttp", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    assert_eq!(pipeline.apply_effect_request("nvhttp","nvhttp",json!({"action":"lookup_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled-fence"}),now()),Err("unknown_source_retirement"));
    assert_eq!(
        pipeline
            .apply_effect_request("nvhttp", "nvhttp", fence, now())
            .unwrap()["state"],
        "fenced"
    );
    let complete = json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled-fence","retirement":null});
    let Response::Accepted { id } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: complete,
            timeout_ms: 30000,
            operation_id: "cancel-completion-delivered".into(),
            parent: None,
        },
    ) else {
        panic!("not accepted")
    };
    pipeline
        .tick_at(now(), |command| {
            let result = broker.dispatch("pairing", command);
            if matches!(&result,Response::Request{id:delivered,..} if *delivered==id) {
                assert!(matches!(
                    broker.dispatch("input", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            Ok(result)
        })
        .unwrap();
    let lookup=pipeline.apply_effect_request("input","input",json!({"action":"lookup_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":"cancelled-fence"}),now()).unwrap();
    assert_eq!(lookup["state"], "fenced");
}

#[test]
fn real_broker_rejects_recovery_while_host_configuration_is_unavailable() {
    let mut configured = settings();
    configured.effect_authority = Some(crate::protocol::EffectAuthoritySettings {
        protocol_policies: vec![],
        protocol_owner: "nvhttp".into(),
        effect_owner: "input".into(),
        feedback_policies: vec![],
    });
    let mut pipeline =
        Pipeline::with_host_configuration(configured, Arc::new(Shared::default()), true);
    pipeline.host_configuration = HostConfigurationState::Failed {
        retry_at: now() + 10,
    };
    let epoch = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let mut broker = authority_broker();
    let Response::Accepted { id } = broker.dispatch(
        "input",
        Command::Request {
            service: EFFECT_SERVICE.into(),
            payload: json!({"action":"recover","epoch":epoch,"owner_epoch":"retired-owner"}),
            timeout_ms: 30_000,
            operation_id: "recovery-during-config-failure".into(),
            parent: None,
        },
    ) else {
        panic!("broker admission failed");
    };
    pipeline
        .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
        .unwrap();
    let Response::Result { payload } =
        broker.dispatch("input", Command::TakeResult { request: id })
    else {
        panic!("missing response");
    };
    assert_eq!(payload["code"], "host_configuration_unavailable");
    let gate = pipeline.effect_gate.as_ref().unwrap();
    assert_eq!(gate.epoch(), epoch);
    assert!(gate.startup_recovery_required());
}

#[test]
fn feedback_grant_and_controller_revocation_wait_for_lease_retirement() {
    for change in ["grant", "controller"] {
        let (mut pipeline, mut harness, certificate_id, epoch, source, scope) =
            authorized_feedback_for_test();
        let admitted = pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"lease","sequence":1}),now()).unwrap();
        let write = if change == "grant" {
            json!({"action":"update_client_feedback_grants","certificate_id":certificate_id,"expected_revision":harness.revision,"grants":[]})
        } else {
            json!({"action":"update_client_config_overrides","certificate_id":certificate_id,"expected_revision":harness.revision,"config_overrides":{"controller":false}})
        };
        let writes = harness.writes;
        harness.enqueue("console", write);
        pipeline
            .tick_at(now(), |command| harness.send(command))
            .unwrap();
        assert_eq!(harness.writes, writes, "{change}");
        assert!(pipeline.deferred_effect_mutation.is_some(), "{change}");
        let retirement = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"retire","scope":"whole_control_stream","state":"retired"});
        pipeline.apply_effect_request("input","input",json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"lease","ticket":admitted["ticket"],"retirement":retirement}),now()).unwrap();
        let generation = pipeline.effect_gate.as_ref().unwrap().fence_generation();
        pipeline
            .apply_effect_request(
                "input",
                "input",
                json!({"action":"drain","epoch":epoch,"fence_generation":generation}),
                now(),
            )
            .unwrap();
        for _ in 0..5 {
            pipeline
                .tick_at(now(), |command| harness.send(command))
                .unwrap();
            if harness.output.is_some() {
                break;
            }
        }
        assert_eq!(harness.output.take().unwrap()["ok"], true, "{change}");
        assert_eq!(harness.writes, writes + 1, "{change}");
        let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
        let next = pipeline
            .apply_effect_request(
                "input",
                "input",
                json!({"action":"recover","epoch":initial,"owner_epoch":"next"}),
                now(),
            )
            .unwrap()["epoch"]
            .as_str()
            .unwrap()
            .to_owned();
        let denied=pipeline.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":next,"certificate_pem":CLIENT_CERT,"source":feedback_source_for_test("next"),"scope":scope}),now());
        assert_eq!(denied, Err("scope_denied"), "{change}");
    }
}

#[test]
fn granted_capability_cannot_expand_a_bound_stream_scope() {
    let (mut pipeline, mut harness, id) = paired_native_client();
    grant_feedback_for_test(
        &mut harness,
        &mut pipeline,
        &id,
        &["speaker.pcm", "other.feedback"],
    );
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm", "other.feedback"],
        crate::protocol::FeedbackPermission::Launch,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    pipeline.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}),now()).unwrap();
    let expanded = feedback_scope_for_test(&["speaker.pcm", "other.feedback"]);
    let denied=pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":expanded,"operation_id":"expanded","sequence":1}),now());
    assert_eq!(denied, Err("stream_scope_denied"));
    assert_eq!(pipeline.effect_gate.as_ref().unwrap().in_flight(), 0);
    assert!(pipeline.apply_effect_request("input","input",json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":scope,"operation_id":"exact","sequence":1}),now()).is_ok());
}

#[test]
fn feedback_grants_are_administered_with_revision_and_default_to_empty() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let listed = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert!(listed["clients"][0].get("feedback_grants").is_none());
    let update = json!({"action":"update_client_feedback_grants","certificate_id":certificate_id,
        "expected_revision":harness.revision,"grants":[{"schema":"speaker.pcm","version":1,"direction":"to_client"}]});
    assert_eq!(
        harness.request(&mut pipeline, "nvhttp", update.clone(), now())["code"],
        "forbidden"
    );
    assert_eq!(
        harness.request(&mut pipeline, "console", update.clone(), now())["ok"],
        true
    );
    assert_eq!(
        harness.request(&mut pipeline, "console", update, now())["code"],
        "revision_conflict"
    );
    let listed = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(
        listed["clients"][0]["feedback_grants"][0]["schema"],
        "speaker.pcm"
    );
    let mut restarted = Pipeline::new(settings());
    let listed = harness.request(
        &mut restarted,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(
        listed["clients"][0]["feedback_grants"][0]["schema"],
        "speaker.pcm"
    );
}

#[test]
fn real_broker_view_only_client_cannot_expand_beyond_explicit_feedback_grant() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    grant_feedback_for_test(
        &mut harness,
        &mut pipeline,
        &certificate_id,
        &["speaker.pcm"],
    );
    let permissions =
        json!({"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false});
    assert_eq!(harness.request(&mut pipeline,"console",json!({"action":"update_client_permissions","certificate_id":certificate_id,"permissions":permissions,"expected_revision":harness.revision}),now())["ok"],true);
    assert_eq!(harness.request(&mut pipeline,"console",json!({"action":"set_permission_mode","enabled":true,"expected_revision":harness.revision}),now())["ok"],true);
    pipeline.effect_authority = Some(policy_for_feedback(
        &["speaker.pcm"],
        crate::protocol::FeedbackPermission::View,
    ));
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    pipeline.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}),now()).unwrap();
    let mut broker = authority_broker();
    for (schema, expected) in [
        ("speaker.pcm", true),
        ("usb.forward", false),
        ("input.keyboard", false),
        ("custom.extension", false),
    ] {
        let requested = feedback_scope_for_test(&[schema]);
        let Response::Accepted{id}=broker.dispatch("input",Command::Request{service:EFFECT_SERVICE.into(),payload:json!({"action":"admit_stream_lease","epoch":epoch,"source":source,"scope":requested,"operation_id":format!("op-{schema}"),"sequence":1}),timeout_ms:30_000,operation_id:format!("broker-{schema}"),parent:None})else{panic!("broker admission failed")};
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch("input", Command::TakeResult { request: id })
        else {
            panic!("missing response")
        };
        assert_eq!(payload["ok"], expected, "{schema}: {payload}");
        if !expected {
            assert_eq!(payload["code"], "scope_denied");
        }
    }
}

mod stream_consumer_broker;

#[test]
fn broker_retirement_receipt_fences_every_source_on_the_transport() {
    let (mut pipeline, _state, _certificate, epoch, first, scope) = authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut sequence = 0_u64;
    let mut call = |pipeline: &mut Pipeline, caller: &str, payload: Value| {
        sequence += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: EFFECT_SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("transport-receipt-test-{sequence}"),
                parent: None,
            },
        ) else {
            panic!("request rejected")
        };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("missing result")
        };
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    };
    let mut second = first.clone();
    second["id"] = json!("b".repeat(32));
    second["device"] = json!("second");
    let mut second_scope = scope.clone();
    second_scope["controller_slot"] = json!(1);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":second,"scope":second_scope})
        )["ok"],
        true
    );
    let admitted = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":first,"scope":scope,"operation_id":"first-lease","sequence":1}),
    );
    let ticket = admitted["ticket"].as_str().unwrap();
    let receipt = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"transport-retire","scope":"whole_control_stream","state":"retired"});
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"first-lease","ticket":ticket,"retirement":receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"admit_stream_lease","epoch":epoch,"source":second,"scope":second_scope,"operation_id":"second-lease","sequence":2})
        )["code"],
        "transport_retired"
    );
    let mut third = first.clone();
    third["id"] = json!("d".repeat(32));
    third["device"] = json!("third");
    let mut third_scope = scope.clone();
    third_scope["controller_slot"] = json!(2);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":third,"scope":third_scope})
        )["code"],
        "transport_retired"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"acknowledge","epoch":epoch,"operation_id":"first-lease","ticket":ticket})
        )["ok"],
        true
    );
    third_scope["transport_stream"] = json!(8);
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":third,"scope":third_scope})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"admit_stream_lease","epoch":epoch,"source":third,"scope":third_scope,"operation_id":"different-transport","sequence":2})
        )["ok"],
        true
    );
}

#[test]
fn nvhttp_reconciles_wide_owner_operation_ids_through_real_broker() {
    use amaterasu_nvhttp::test_support::{SourceRecovery, SourceRecoveryOutcome};
    for owner_operation in ["Owner:retire".to_owned(), "X".repeat(128)] {
        let (mut pairing, _state, _certificate, epoch, source, scope) =
            authorized_feedback_for_test();
        let mut broker = authority_broker();
        let request = amaterasu_module_sdk::nvhttp_source::BindFeedbackSource {
            authority_epoch: epoch.clone(),
            source: serde_json::from_value(source.clone()).unwrap(),
            scope: serde_json::from_value(scope.clone()).unwrap(),
            input_boot_epoch: "input-boot".into(),
        };
        let Response::Accepted { id: fence } = broker.dispatch("nvhttp", Command::Request {
            service: EFFECT_SERVICE.into(), payload: json!({"action":"fence_source","epoch":epoch,"source":source,"scope":scope,"operation_id":owner_operation}),
            timeout_ms:30_000, operation_id:"owner-fence".into(), parent:None,
        }) else { panic!("fence rejected") };
        pairing
            .tick_at(now(), |cmd| Ok(broker.dispatch("pairing", cmd)))
            .unwrap();
        assert!(matches!(
            broker.dispatch("nvhttp", Command::TakeResult { request: fence }),
            Response::Result { .. }
        ));
        assert!(matches!(
            broker.dispatch("nvhttp", Command::ReleaseResult { request: fence }),
            Response::Ok
        ));
        let Response::Accepted { id: origin } = broker.dispatch(
            "input",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: request.payload(),
                timeout_ms: 30_000,
                operation_id: "owner-origin".into(),
                parent: None,
            },
        ) else {
            panic!("origin rejected")
        };
        assert!(matches!(
            broker.dispatch("nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        let mut recovery = SourceRecovery::new(origin, request.clone());
        let mut found = false;
        for _ in 0..30 {
            if let SourceRecoveryOutcome::FenceSettled {
                fence_confirmed,
                operation_id,
                ..
            } = recovery.advance(|cmd| Ok(broker.dispatch("nvhttp", cmd)))
            {
                assert!(fence_confirmed);
                assert_eq!(operation_id, owner_operation);
                found = true;
                break;
            }
            pairing
                .tick_at(now(), |cmd| Ok(broker.dispatch("pairing", cmd)))
                .unwrap();
        }
        assert!(found);
        let Response::Accepted { id: completion } = broker.dispatch("input", Command::Request {
            service:EFFECT_SERVICE.into(), payload:json!({"action":"complete_source_retirement","epoch":epoch,"source":source,"scope":scope,"operation_id":owner_operation,"retirement":null}),
            timeout_ms:30_000, operation_id:"owner-finish".into(), parent:None,
        }) else { panic!("completion rejected") };
        pairing
            .tick_at(now(), |cmd| Ok(broker.dispatch("pairing", cmd)))
            .unwrap();
        let Response::Result { payload } = broker.dispatch(
            "input",
            Command::TakeResult {
                request: completion,
            },
        ) else {
            panic!("missing completion")
        };
        assert_eq!(payload["state"], "retired");
        let Response::Accepted { id: reconcile } = broker.dispatch(
            "input",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: request.reconciliation_payload(),
                timeout_ms: 30_000,
                operation_id: "owner-reconcile".into(),
                parent: None,
            },
        ) else {
            panic!("reconcile rejected")
        };
        assert!(matches!(
            broker.dispatch("nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        recovery.begin_lookup(reconcile, &request).unwrap();
        let mut retired = false;
        for _ in 0..30 {
            if let SourceRecoveryOutcome::Reply {
                retired: is_retired,
                payload,
                ..
            } = recovery.advance(|cmd| Ok(broker.dispatch("nvhttp", cmd)))
            {
                assert!(is_retired);
                assert!(request.accepts_retirement_confirmation(&payload, &owner_operation));
                retired = true;
                break;
            }
            pairing
                .tick_at(now(), |cmd| Ok(broker.dispatch("pairing", cmd)))
                .unwrap();
        }
        assert!(retired);
    }
}

#[test]
fn broker_lost_lease_lookup_reports_shared_transport_retirement() {
    let (mut pipeline, _state, _certificate, epoch, first, first_scope) =
        authorized_feedback_for_test();
    let mut broker = authority_broker();
    let mut next = 0_u64;
    let mut call = |pipeline: &mut Pipeline, caller: &str, payload: Value| {
        next += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: EFFECT_SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("retired-transport-lookup-{next}"),
                parent: None,
            },
        ) else {
            panic!("request rejected")
        };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("missing result")
        };
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    };
    let mut second = first.clone();
    second["id"] = json!("b".repeat(32));
    second["device"] = json!("second");
    let mut second_scope = first_scope.clone();
    second_scope["controller_slot"] = json!(1);
    let mut other = first.clone();
    other["id"] = json!("d".repeat(32));
    other["device"] = json!("other");
    let mut other_scope = first_scope.clone();
    other_scope["controller_slot"] = json!(2);
    other_scope["transport_stream"] = json!(8);
    for (source, scope) in [(&second, &second_scope), (&other, &other_scope)] {
        assert_eq!(
            call(
                &mut pipeline,
                "nvhttp",
                json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope})
            )["ok"],
            true
        );
    }
    let first_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":first,"scope":first_scope,"operation_id":"first-lease","sequence":1}),
    );
    let second_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":second,"scope":second_scope,"operation_id":"second-lease","sequence":2}),
    );
    let other_lease = call(
        &mut pipeline,
        "input",
        json!({"action":"admit_stream_lease","epoch":epoch,"source":other,"scope":other_scope,"operation_id":"other-lease","sequence":3}),
    );
    let receipt = json!({"ok":true,"service":"amaterasu.control-transport/1","stream":7,"provider_epoch":"c".repeat(32),"operation_id":"transport-retire","scope":"whole_control_stream","state":"retired"});
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"first-lease","ticket":first_lease["ticket"],"retirement":receipt})
        )["ok"],
        true
    );
    let lookup = call(
        &mut pipeline,
        "input",
        json!({"action":"lookup_stream_lease","epoch":epoch,"source":second,"scope":second_scope,"operation_id":"second-lease"}),
    );
    assert_eq!(lookup["ticket"], second_lease["ticket"]);
    assert_eq!(lookup["fenced"], true);
    let lookup_other = call(
        &mut pipeline,
        "input",
        json!({"action":"lookup_stream_lease","epoch":epoch,"source":other,"scope":other_scope,"operation_id":"other-lease"}),
    );
    assert_eq!(lookup_other["ticket"], other_lease["ticket"]);
    assert_eq!(lookup_other["fenced"], false);
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"complete_stream_lease","epoch":epoch,"operation_id":"second-lease","ticket":second_lease["ticket"],"retirement":receipt})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut pipeline,
            "input",
            json!({"action":"acknowledge","epoch":epoch,"operation_id":"second-lease","ticket":second_lease["ticket"]})
        )["ok"],
        true
    );
}

#[test]
fn protocol_grants_are_admin_only_revision_checked_and_default_deny() {
    let (mut pipeline, mut harness, certificate_id) = paired_native_client();
    let listed = harness.request(
        &mut pipeline,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert!(listed["clients"][0].get("protocol_grants").is_none());
    let update = json!({"action":"update_client_protocol_grants","certificate_id":certificate_id,
        "expected_revision":harness.revision,"grants":["example.control/1"]});
    assert_eq!(
        harness.request(&mut pipeline, "nvhttp", update.clone(), now())["code"],
        "forbidden"
    );
    assert_eq!(
        harness.request(&mut pipeline, "console", update.clone(), now())["ok"],
        true
    );
    assert_eq!(
        harness.request(&mut pipeline, "console", update, now())["code"],
        "revision_conflict"
    );
    let mut restarted = Pipeline::new(settings());
    let listed = harness.request(
        &mut restarted,
        "console",
        json!({"action":"list_clients"}),
        now(),
    );
    assert_eq!(
        listed["clients"][0]["protocol_grants"],
        json!(["example.control/1"])
    );
}

#[test]
fn broker_protocol_attestation_requires_paired_source_and_exact_client_grant() {
    let (mut pipeline, mut state, certificate_id) = paired_native_client();
    grant_feedback_for_test(&mut state, &mut pipeline, &certificate_id, &["speaker.pcm"]);
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"update_client_protocol_grants","certificate_id":certificate_id,
        "expected_revision":state.revision,"grants":["example.control/1"]}),
            now()
        )["ok"],
        true
    );
    let permissions =
        json!({"list_apps":true,"view_streams":true,"launch_apps":false,"terminate_streams":false});
    assert_eq!(
        state.request(
            &mut pipeline,
            "console",
            json!({"action":"update_client_permissions","certificate_id":certificate_id,
        "permissions":permissions,"expected_revision":state.revision}),
            now()
        )["ok"],
        true
    );
    assert_eq!(state.request(&mut pipeline,"console",json!({"action":"set_permission_mode","enabled":true,"expected_revision":state.revision}),now())["ok"],true);
    let mut selected =
        policy_for_feedback(&["speaker.pcm"], crate::protocol::FeedbackPermission::View);
    let method = json!({"service":"example.effects/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
    let cleanup = json!({"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
    selected.protocol_policies = serde_json::from_value(json!([{"capability":"example.control/1","provider":"provider",
        "message_type":21818,"method":method,"cleanup":cleanup,"method_schema":protocol_test_schema(&method),
        "cleanup_schema":protocol_test_schema(&cleanup),"max_payload_bytes":64,"required_permission":"view"}])).unwrap();
    pipeline.effect_authority = Some(selected);
    let initial = pipeline.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = pipeline
        .apply_effect_request(
            "input",
            "input",
            json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
            now(),
        )
        .unwrap()["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    let scope = feedback_scope_for_test(&["speaker.pcm"]);
    assert_eq!(pipeline.apply_effect_request("nvhttp","nvhttp",json!({"action":"bind_stream","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source,"scope":scope}),now()).unwrap()["ok"],true);
    let mut broker = protocol_authority_broker(protocol_test_schema(&method));
    let mut next = 0_u64;
    let mut call = |pipeline: &mut Pipeline, caller: &str, payload: Value| {
        next += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: EFFECT_SERVICE.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("protocol-authority-test-{next}"),
                parent: None,
            },
        ) else {
            panic!("rejected")
        };
        pipeline
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        let Response::Result { payload } =
            broker.dispatch(caller, Command::TakeResult { request: id })
        else {
            panic!("missing")
        };
        assert!(matches!(
            broker.dispatch(caller, Command::ReleaseResult { request: id }),
            Response::Ok
        ));
        payload
    };
    let attest = json!({"action":"attest_protocol_capabilities","version":1,"epoch":epoch,"source":source,"capabilities":["example.control/1"]});
    assert_eq!(
        call(&mut pipeline, "input", attest.clone())["code"],
        "forbidden"
    );
    let mut unknown = attest.clone();
    unknown["capabilities"] = json!(["example.usb/1"]);
    assert_eq!(
        call(&mut pipeline, "nvhttp", unknown)["code"],
        "scope_denied"
    );
    let attested = call(&mut pipeline, "nvhttp", attest.clone());
    assert_eq!(attested["ok"], true);
    let issuer_epoch = attested["issuer_epoch"].as_str().unwrap().to_owned();
    assert_eq!(
        call(&mut pipeline, "nvhttp", attest.clone())["issuer_epoch"],
        issuer_epoch
    );
    let mut expanded = attest.clone();
    expanded["capabilities"] = json!([]);
    assert_eq!(
        call(&mut pipeline, "nvhttp", expanded)["code"],
        "protocol_attestation_conflict"
    );
    let effect = json!({"caller":"input","operation_id":"effect.1","issuer_epoch":issuer_epoch,
        "context":{"authority_epoch":epoch,"effect_owner_epoch":"owner","target":{"kind":"stream","host_epoch":source["host_epoch"],
            "session_id":source["session_id"],"stream_id":source["stream_id"],"generation":source["stream_generation"]},"peer_ref":source["id"]},
        "message_type":21818,"capability":"example.control/1","provider":"provider","method":method,"cleanup":cleanup,
        "payload_sha256":"c".repeat(64),"timeout_ms":1000,"parent":null});
    let request = json!({"action":"admit_protocol_effect","version":1,"epoch":epoch,"source":source,"effect":effect});
    let mut forged_issuer = request.clone();
    forged_issuer["effect"]["issuer_epoch"] = json!("forged");
    assert_eq!(
        call(&mut pipeline, "input", forged_issuer)["code"],
        "scope_denied"
    );
    let mut forbidden = request.clone();
    forbidden["effect"]["capability"] = json!("example.usb/1");
    assert_eq!(
        call(&mut pipeline, "input", forbidden)["code"],
        "scope_denied"
    );
    let mut forged = request.clone();
    forged["effect"]["provider"] = json!("other-provider");
    assert_eq!(call(&mut pipeline, "input", forged)["code"], "scope_denied");
    assert_eq!(
        call(&mut pipeline, "input", request.clone())["code"],
        "protocol_authority_unavailable"
    );
    {
        use sha2::Digest;
        let payload = vec![1_u8, 2, 3];
        let mut effect = request["effect"].clone();
        let source_identity: amaterasu_module_sdk::effect_authority::Source =
            serde_json::from_value(source.clone()).unwrap();
        let scope = amaterasu_module_sdk::broker_effect_fence::EffectScope {
            authority_epoch: epoch.clone(),
            issuer_epoch: issuer_epoch.clone(),
            source: amaterasu_module_sdk::broker_effect_fence::source(&source_identity).unwrap(),
        };
        effect["operation_id"] = json!(
            amaterasu_module_sdk::protocol_slot_state::operation_id("input", &scope, 1).unwrap()
        );
        effect["payload_sha256"] = json!(format!("{:x}", sha2::Sha256::digest(&payload)));
        let request_v2 = json!({"action":"admit_protocol_effect_v2","version":2,"epoch":epoch,"source":source,
            "effect":effect,"sequence":1,"payload":payload,"cleanup":{"id":"d".repeat(64)}});
        assert_eq!(
            call(&mut pipeline, "input", request_v2.clone())["code"],
            "protocol_authority_unavailable"
        );
        let mut bad_digest = request_v2.clone();
        bad_digest["payload"][0] = json!(9);
        assert_eq!(
            call(&mut pipeline, "input", bad_digest)["code"],
            "invalid_protocol_effect"
        );
        let mut bad_cleanup = request_v2.clone();
        bad_cleanup["cleanup"]["id"] = json!("forged");
        assert_eq!(
            call(&mut pipeline, "input", bad_cleanup)["code"],
            "invalid_protocol_effect"
        );
        let mut bad_sequence = request_v2.clone();
        bad_sequence["sequence"] = json!(2);
        assert_eq!(
            call(&mut pipeline, "input", bad_sequence)["code"],
            "invalid_protocol_effect"
        );
        let mut bad_issuer = request_v2;
        bad_issuer["effect"]["issuer_epoch"] = json!("forged");
        let mut forged_scope = scope;
        forged_scope.issuer_epoch = "forged".into();
        bad_issuer["effect"]["operation_id"] = json!(
            amaterasu_module_sdk::protocol_slot_state::operation_id("input", &forged_scope, 1)
                .unwrap()
        );
        assert_eq!(
            call(&mut pipeline, "input", bad_issuer)["code"],
            "scope_denied"
        );
    }
    pipeline
        .effect_authority
        .as_mut()
        .unwrap()
        .protocol_policies[0]
        .method_schema
        .request
        .allow_extra = true;
    assert_eq!(
        call(&mut pipeline, "input", request.clone())["code"],
        "scope_denied"
    );
    pipeline
        .effect_authority
        .as_mut()
        .unwrap()
        .protocol_policies[0]
        .method_schema
        .request
        .allow_extra = false;
    pipeline
        .effect_authority
        .as_mut()
        .unwrap()
        .protocol_policies[0]
        .provider = "replacement".into();
    assert_eq!(
        call(&mut pipeline, "input", request.clone())["code"],
        "scope_denied"
    );
    let mut replacement = request.clone();
    replacement["effect"]["provider"] = json!("replacement");
    assert_eq!(
        call(&mut pipeline, "input", replacement)["code"],
        "scope_denied"
    );
    pipeline
        .effect_authority
        .as_mut()
        .unwrap()
        .protocol_policies[0]
        .provider = "provider".into();
    pipeline
        .effect_authority
        .as_mut()
        .unwrap()
        .protocol_policies[0]
        .required_permission = crate::protocol::FeedbackPermission::Launch;
    assert_eq!(
        call(&mut pipeline, "input", request.clone())["code"],
        "scope_denied"
    );
    assert_eq!(
        call(
            &mut pipeline,
            "nvhttp",
            json!({"action":"fence_source","epoch":epoch,"source":source,"scope":scope,"operation_id":"retire-protocol-source"})
        )["state"],
        "fenced"
    );
    assert_eq!(
        call(&mut pipeline, "input", request.clone())["code"],
        "source_denied"
    );
    let mut restarted = Pipeline::new(settings());
    restarted.effect_authority = pipeline.effect_authority.clone();
    assert_eq!(
        call(&mut restarted, "input", request)["code"],
        "stale_epoch"
    );
}

#[test]
fn real_state_pairing_and_broker_keep_protocol_admission_closed_across_restart() {
    use amaterasu_state::test_support::TestStateStore;
    let path = std::env::temp_dir().join(format!(
        "pairing-protocol-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(path.clone()).unwrap();
    let (mut pairing, fixture, certificate_id) = paired_native_client();
    assert_eq!(store.handle("pairing",json!({"action":"set","key":"pairing/v1","value":fixture.document.unwrap(),"expected_revision":0}))["revision"],1);
    let mut broker = authority_broker();
    let mut counter = 0_u64;
    let mut call = |pairing: &mut Pipeline, caller: &str, service: &str, payload: Value| -> Value {
        counter += 1;
        let Response::Accepted { id } = broker.dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("real-state-protocol-{counter}"),
                parent: None,
            },
        ) else {
            panic!("request denied")
        };
        for _ in 0..100 {
            pairing
                .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
                .unwrap();
            loop {
                match broker.dispatch("state", Command::Poll {}) {
                    Response::Request {
                        id: state_id,
                        caller: state_caller,
                        payload,
                        ..
                    } => {
                        let response = store.handle(&state_caller, payload);
                        assert!(matches!(
                            broker.dispatch(
                                "state",
                                Command::Complete {
                                    request: state_id,
                                    payload: response
                                }
                            ),
                            Response::Ok
                        ));
                    }
                    Response::Pending => break,
                    other => panic!("unexpected State poll {other:?}"),
                }
            }
            match broker.dispatch(caller, Command::TakeResult { request: id }) {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert!(matches!(
                        broker.dispatch(caller, Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
                other => panic!("unexpected result {other:?}"),
            }
        }
        panic!("request did not complete")
    };
    let listed = call(
        &mut pairing,
        "console",
        SERVICE,
        json!({"action":"list_clients"}),
    );
    assert!(listed["clients"][0].get("protocol_grants").is_none());
    let updated = call(
        &mut pairing,
        "console",
        SERVICE,
        json!({"action":"update_client_protocol_grants","certificate_id":certificate_id,
        "expected_revision":1,"grants":["example.control/1"]}),
    );
    assert_eq!(updated["ok"], true);
    drop(pairing);
    let mut pairing = Pipeline::new(settings());
    let listed = call(
        &mut pairing,
        "console",
        SERVICE,
        json!({"action":"list_clients"}),
    );
    assert_eq!(
        listed["clients"][0]["protocol_grants"],
        json!(["example.control/1"])
    );
    let mut selected = policy_for_feedback(&[], crate::protocol::FeedbackPermission::View);
    let method = json!({"service":"example.effects/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
    let cleanup = json!({"service":"example.effects/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":1});
    selected.protocol_policies = serde_json::from_value(
        json!([{"capability":"example.control/1","provider":"provider","message_type":21818,
        "method":method,"cleanup":cleanup,"method_schema":protocol_test_schema(&method),
        "cleanup_schema":protocol_test_schema(&cleanup),"max_payload_bytes":64,"required_permission":"view"}]),
    )
    .unwrap();
    pairing.effect_authority = Some(selected);
    let initial = pairing.effect_gate.as_ref().unwrap().epoch().to_owned();
    let epoch = call(
        &mut pairing,
        "input",
        EFFECT_SERVICE,
        json!({"action":"recover","epoch":initial,"owner_epoch":"owner"}),
    )["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = feedback_source_for_test("owner");
    assert_eq!(
        call(
            &mut pairing,
            "nvhttp",
            EFFECT_SERVICE,
            json!({"action":"bind","epoch":epoch,"certificate_pem":CLIENT_CERT,"source":source})
        )["ok"],
        true
    );
    let attestation = json!({"action":"attest_protocol_capabilities","version":1,"epoch":epoch,"source":source,"capabilities":["example.control/1"]});
    assert_eq!(
        call(&mut pairing, "input", EFFECT_SERVICE, attestation.clone())["code"],
        "forbidden"
    );
    let attested = call(&mut pairing, "nvhttp", EFFECT_SERVICE, attestation);
    assert_eq!(attested["ok"], true);
    let issuer_epoch = attested["issuer_epoch"].as_str().unwrap();
    let effect = json!({"caller":"input","operation_id":"effect.1","issuer_epoch":issuer_epoch,
        "context":{"authority_epoch":epoch,"effect_owner_epoch":"owner","target":{"kind":"stream","host_epoch":source["host_epoch"],
            "session_id":source["session_id"],"stream_id":source["stream_id"],"generation":source["stream_generation"]},"peer_ref":source["id"]},
        "message_type":21818,"capability":"example.control/1","provider":"provider","method":method,"cleanup":cleanup,
        "payload_sha256":"c".repeat(64),"timeout_ms":1000,"parent":null});
    let request = json!({"action":"admit_protocol_effect","version":1,"epoch":epoch,"source":source,"effect":effect});
    assert_eq!(
        call(&mut pairing, "input", EFFECT_SERVICE, request.clone())["code"],
        "protocol_authority_unavailable"
    );
    drop(pairing);
    let mut restarted = Pipeline::new(settings());
    restarted.effect_authority = Some(policy_for_feedback(
        &[],
        crate::protocol::FeedbackPermission::View,
    ));
    assert_eq!(
        call(&mut restarted, "input", EFFECT_SERVICE, request)["code"],
        "stale_epoch"
    );
    drop(call);
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn real_state_lost_grant_reply_stays_closed_until_durable_reload() {
    use amaterasu_state::test_support::TestStateStore;
    let path = std::env::temp_dir().join(format!(
        "pairing-protocol-lost-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = TestStateStore::open(path.clone()).unwrap();
    let (mut pairing, fixture, certificate_id) = paired_native_client();
    assert_eq!(store.handle("pairing",json!({"action":"set","key":"pairing/v1","value":fixture.document.unwrap(),"expected_revision":0}))["revision"],1);
    let mut broker = authority_broker();
    let Response::Accepted{id:parent}=broker.dispatch("console",Command::Request {service:SERVICE.into(),
        payload:json!({"action":"update_client_protocol_grants","certificate_id":certificate_id,"expected_revision":1,"grants":["example.control/1"]}),
        timeout_ms:30_000,operation_id:"lost-grant-reply".into(),parent:None}) else {panic!("request rejected")};
    let mut committed = false;
    for _ in 0..100 {
        pairing
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        match broker.dispatch("state", Command::Poll {}) {
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => {
                let is_write = payload["action"] == "set";
                let result = store.handle(&caller, payload);
                if is_write {
                    assert_eq!(result["ok"], true);
                    assert!(matches!(
                        broker.dispatch("console", Command::Cancel { request: parent }),
                        Response::Ok
                    ));
                    assert!(matches!(
                        broker.dispatch(
                            "state",
                            Command::Complete {
                                request: id,
                                payload: result
                            }
                        ),
                        Response::Error { .. }
                    ));
                    committed = true;
                    break;
                }
                assert!(matches!(
                    broker.dispatch(
                        "state",
                        Command::Complete {
                            request: id,
                            payload: result
                        }
                    ),
                    Response::Ok
                ));
            }
            Response::Pending => {}
            other => panic!("unexpected State poll {other:?}"),
        }
    }
    assert!(committed);
    for _ in 0..3 {
        pairing
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
    }
    assert!(pairing.identity.get(now()).is_err());
    let durable = store.handle("pairing", json!({"action":"get","key":"pairing/v1"}));
    assert_eq!(
        durable["value"]["clients"][0]["protocol_grants"],
        json!(["example.control/1"])
    );
    drop(pairing);
    let mut restarted = Pipeline::new(settings());
    let Response::Accepted { id } = broker.dispatch(
        "console",
        Command::Request {
            service: SERVICE.into(),
            payload: json!({"action":"list_clients"}),
            timeout_ms: 30_000,
            operation_id: "reload-grants".into(),
            parent: None,
        },
    ) else {
        panic!("reload rejected")
    };
    let mut listed = None;
    for _ in 0..50 {
        restarted
            .tick_at(now(), |command| Ok(broker.dispatch("pairing", command)))
            .unwrap();
        if let Response::Request {
            id: state_id,
            caller,
            payload,
            ..
        } = broker.dispatch("state", Command::Poll {})
        {
            let result = store.handle(&caller, payload);
            assert!(matches!(
                broker.dispatch(
                    "state",
                    Command::Complete {
                        request: state_id,
                        payload: result
                    }
                ),
                Response::Ok
            ));
        }
        if let Response::Result { payload } =
            broker.dispatch("console", Command::TakeResult { request: id })
        {
            listed = Some(payload);
            break;
        }
    }
    assert_eq!(
        listed.unwrap()["clients"][0]["protocol_grants"],
        json!(["example.control/1"])
    );
    drop(store);
    std::fs::remove_dir_all(path).unwrap();
}

fn protocol_test_schema(method: &Value) -> Value {
    let mut schema = method.clone();
    schema.as_object_mut().unwrap().remove("schema_revision");
    schema["request"] = json!({"required":{},"optional":{},"allow_extra":false});
    schema["response"] = json!({"required":{},"optional":{},"allow_extra":false});
    schema["privilege"] = json!("user");
    schema
}
