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
