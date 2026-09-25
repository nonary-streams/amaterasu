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
fn now() -> u64 {
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
    }
}

struct Harness {
    document: Option<Value>,
    revision: u64,
    incoming: Option<Response>,
    state_reply: Option<Response>,
    output: Option<Value>,
    writes: usize,
    fail_write: bool,
    conflict_next_write: bool,
    cancelled: bool,
    check_cancelled: bool,
}
impl Harness {
    fn new() -> Self {
        Self {
            document: None,
            revision: 0,
            incoming: None,
            state_reply: None,
            output: None,
            writes: 0,
            fail_write: false,
            conflict_next_write: false,
            cancelled: false,
            check_cancelled: false,
        }
    }
    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
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
                assert_eq!(service, STATE_SERVICE);
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
            Command::TakeResult { request: 50 } => {
                self.state_reply.take().unwrap_or(Response::Pending)
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
        for _ in 0..5 {
            pipeline
                .tick_at(time, |command| self.send(command))
                .unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
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
    let client = exchange(&mut state, &mut pipeline, &id, "1234", now());
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
                "stream_audio":false,
                "native_pen_touch":false,
                "audio_sink_capture_only":false,
                "back_button_timeout":-1
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
        certificate_readback["config_overrides"]["stream_audio"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["native_pen_touch"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        certificate_readback["config_overrides"]["back_button_timeout"],
        -1
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
        persisted["clients"][0]["config_overrides"]["stream_audio"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["native_pen_touch"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(
        persisted["clients"][0]["config_overrides"]["back_button_timeout"],
        -1
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
    assert_eq!(unchanged["config_overrides"]["stream_audio"], false);
    assert_eq!(unchanged["config_overrides"]["native_pen_touch"], false);
    assert_eq!(
        unchanged["config_overrides"]["audio_sink_capture_only"],
        false
    );
    assert_eq!(unchanged["config_overrides"]["back_button_timeout"], -1);
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

fn crypto_certificate_der(pem: &str) -> Vec<u8> {
    crate::crypto::parse_certificate(pem, None)
        .unwrap()
        .certificate
        .to_der()
        .unwrap()
}

struct HostConfigurationHarness {
    actions: Vec<String>,
    missing_provider: bool,
    ready: bool,
    effective: Value,
    incoming: Option<Response>,
    output: Option<Value>,
    state_document: Option<Value>,
    state_revision: u64,
    state_requests: usize,
    state_writes: usize,
    polls: usize,
}

impl HostConfigurationHarness {
    fn new(effective: Value) -> Self {
        Self {
            actions: Vec::new(),
            missing_provider: false,
            ready: true,
            effective,
            incoming: None,
            output: None,
            state_document: None,
            state_revision: 0,
            state_requests: 0,
            state_writes: 0,
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
        for _ in 0..6 {
            pipeline
                .tick_at(time, |command| self.send(command))
                .unwrap();
            if let Some(output) = self.output.take() {
                return output;
            }
        }
        panic!("configured pairing request stalled")
    }

    fn send(&mut self, command: Command) -> Transport {
        Ok(match command {
            Command::Request {
                service, payload, ..
            } if service == "amaterasu.host-configuration/1" => {
                let action = payload["action"].as_str().unwrap();
                self.actions.push(action.into());
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
                    "ok":true,"ready":true,"revision":4,"effective":self.effective
                }),
            },
            Command::Request {
                service, payload, ..
            } if service == STATE_SERVICE => {
                assert_eq!(payload["key"], STATE_KEY);
                self.state_requests += 1;
                match payload["action"].as_str() {
                    Some("get") => Response::Accepted { id: 72 },
                    Some("set") => {
                        assert_eq!(payload["expected_revision"], self.state_revision);
                        self.state_writes += 1;
                        self.state_document = Some(payload["value"].clone());
                        self.state_revision += 1;
                        Response::Accepted { id: 73 }
                    }
                    _ => panic!("unexpected pairing state operation"),
                }
            }
            Command::TakeResult { request: 72 } => Response::Result {
                payload: json!({
                    "ok":true,
                    "revision":self.state_revision,
                    "found":self.state_document.is_some(),
                    "value":self.state_document.clone()
                }),
            },
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
