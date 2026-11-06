use crate::{
    crypto,
    protocol::*,
    transcript::{Phase, Transcript},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub enum Change {
    Read(Value),
    Write { document: Document, response: Value },
}
struct BeginClient {
    client_id: String,
    name: String,
    pem: String,
    salt: String,
}
pub struct Engine {
    settings: Settings,
    pending: BTreeMap<String, Transcript>,
    window_start: u64,
    window_attempts: u8,
    latest_time: u64,
}
impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            pending: BTreeMap::new(),
            window_start: 0,
            window_attempts: 0,
            latest_time: 0,
        }
    }
    pub fn admit(&self, caller: &str, operation: &Operation) -> Result<(), &'static str> {
        operation.validate()?;
        if operation.admin_only() && !self.settings.administrator_principals.contains(caller) {
            return Err("forbidden");
        }
        if operation.transport_only() && !self.settings.transport_principals.contains(caller) {
            return Err("forbidden");
        }
        if matches!(operation, Operation::ClientAuthorization { .. })
            && !self.settings.authority_observer_principals.contains(caller)
        {
            return Err("forbidden");
        }
        // An observation grant must not become a route to the otherwise public
        // pairing handshake operations, which carry transport responsibilities.
        if self.settings.authority_observer_principals.contains(caller)
            && !self.settings.administrator_principals.contains(caller)
            && !self.settings.transport_principals.contains(caller)
            && !matches!(operation, Operation::ClientAuthorization { .. })
        {
            return Err("forbidden");
        }
        Ok(())
    }
    pub fn apply(
        &mut self,
        caller: &str,
        operation: Operation,
        document: Document,
        now: u64,
    ) -> Result<Change, &'static str> {
        self.admit(caller, &operation)?;
        let now = self.latest_time.max(now);
        document.validate(now)?;
        self.latest_time = now;
        self.pending.retain(|_, pending| pending.expires_at > now);
        match operation {
            Operation::Initialize {} => Self::initialize(document),
            Operation::ImportIdentity {
                certificate_pem,
                private_key_pem,
                host_id,
            } => Self::import(document, certificate_pem, private_key_pem, host_id, now),
            Operation::ImportClient {
                client_id,
                name,
                certificate_pem,
            } => Self::import_client(document, client_id, name, certificate_pem, now),
            Operation::Identity {} => {
                let identity = document.identity.as_ref().ok_or("not_initialized")?;
                Ok(Change::Read(identity_response(identity)))
            }
            Operation::Begin {
                client_id,
                name,
                client_certificate_pem,
                salt,
            } => {
                let client = BeginClient {
                    client_id,
                    name,
                    pem: client_certificate_pem,
                    salt,
                };
                self.begin(caller, &document, client, now)
            }
            Operation::ApprovePin { pairing_id, pin } => self.approve(pairing_id, &pin),
            Operation::GetServerCert { pairing_id } => {
                self.advance(caller, pairing_id, &document, |transcript, identity| {
                    Ok(match transcript.server_certificate(identity)? {
                        Some(cert) => json!({"ok":true,"paired":1,"plaincert":cert}),
                        None => json!({"ok":true,"pending":true,"paired":0}),
                    })
                })
            }
            Operation::ClientChallenge {
                pairing_id,
                clientchallenge,
            } => self.advance(caller, pairing_id, &document, |transcript, identity| {
                let response = transcript.client_challenge(identity, &clientchallenge)?;
                Ok(json!({"ok":true,"paired":1,"challengeresponse":response}))
            }),
            Operation::ServerChallengeResponse {
                pairing_id,
                serverchallengeresp,
            } => self.advance(caller, pairing_id, &document, |transcript, identity| {
                let secret =
                    transcript.server_challenge_response(identity, &serverchallengeresp)?;
                Ok(json!({"ok":true,"paired":1,"pairingsecret":secret}))
            }),
            Operation::ClientPairingSecret {
                pairing_id,
                clientpairingsecret,
            } => self.finish(caller, pairing_id, &clientpairingsecret, document, now),
            Operation::Cancel { pairing_id } => {
                self.take_owned(caller, &pairing_id)?;
                Ok(Change::Read(json!({"ok":true})))
            }
            Operation::Pending {} => Ok(Change::Read(self.pending_metadata())),
            Operation::ListClients {} => Ok(Change::Read(client_metadata(&document))),
            Operation::Unpair { certificate_id, .. } => self.unpair(document, &certificate_id, now),
            Operation::UpdateClientConfigOverrides {
                certificate_id,
                config_overrides,
                expected_revision: _,
            } => Self::update_client_config_overrides(
                document,
                &certificate_id,
                *config_overrides,
                now,
            ),
            Operation::UpdateClientPermissions {
                certificate_id,
                permissions,
                ..
            } => Self::update_client_permissions(document, &certificate_id, permissions, now),
            Operation::SetPermissionMode { enabled, .. } => {
                let mut document = document;
                document.permission_mode_enabled = enabled;
                write(
                    document,
                    json!({"ok":true,"permission_mode_enabled":enabled}),
                    now,
                )
            }
            Operation::CheckClientCertificate { certificate_pem } => {
                Self::check_certificate(&document, &certificate_pem, now)
            }
            Operation::ClientAuthorization { certificate_id } => Ok(Change::Read(
                client_authorization(&document, &certificate_id, now),
            )),
            Operation::UnpairPresentedClient { certificate_pem } => {
                self.unpair_presented(document, &certificate_pem, now)
            }
        }
    }

    fn pending_metadata(&self) -> Value {
        let pending: Vec<_> = self
            .pending
            .iter()
            .map(|(id, transcript)| {
                json!({
                    "pairing_id": id,
                    "client_id": transcript.client.client_id,
                    "name": transcript.client.name,
                    "certificate_id": transcript.client.certificate_id,
                    "expires_at": transcript.expires_at,
                    "awaiting_pin": matches!(transcript.phase, Phase::AwaitingPin)
                })
            })
            .collect();
        json!({"ok":true,"pending":pending})
    }

    fn initialize(document: Document) -> Result<Change, &'static str> {
        let identity = document.identity.as_ref().ok_or("not_initialized")?;
        Ok(Change::Read(identity_response(identity)))
    }
    fn import(
        mut document: Document,
        certificate_pem: String,
        private_key_pem: String,
        host_id: Option<String>,
        now: u64,
    ) -> Result<Change, &'static str> {
        if document.identity.is_some() {
            return Err("already_initialized");
        }
        document.identity = Some(crypto::import_identity(
            certificate_pem,
            private_key_pem,
            host_id,
            now,
        )?);
        let response = identity_response(document.identity.as_ref().ok_or("identity_failure")?);
        write(document, response, now)
    }
    fn import_client(
        mut document: Document,
        client_id: String,
        name: String,
        certificate_pem: String,
        now: u64,
    ) -> Result<Change, &'static str> {
        if document.identity.is_none() {
            return Err("not_initialized");
        }
        let certificate = crypto::parse_certificate(&certificate_pem, Some(now))?;
        if document.clients.len() >= MAX_CLIENTS
            || document
                .clients
                .iter()
                .any(|client| client.certificate_id == certificate.fingerprint)
        {
            return Err("client_limit");
        }
        let response = json!({"ok":true,"certificate_id":certificate.fingerprint});
        document.clients.push(Client {
            certificate_id: certificate.fingerprint,
            client_id,
            name,
            certificate_pem,
            paired_at: now,
            permissions: ClientPermissions::default(),
            config_overrides: ClientConfigOverrides::default(),
        });
        write(document, response, now)
    }
    fn begin(
        &mut self,
        caller: &str,
        document: &Document,
        client: BeginClient,
        now: u64,
    ) -> Result<Change, &'static str> {
        let BeginClient {
            client_id,
            name,
            pem,
            salt,
        } = client;
        if document.identity.is_none() {
            return Err("not_initialized");
        }
        if now.saturating_sub(self.window_start) >= 60 {
            self.window_start = now;
            self.window_attempts = 0;
        }
        if self.window_attempts >= 8 {
            return Err("rate_limited");
        }
        self.window_attempts += 1;
        if self.pending.len() >= 8
            || self
                .pending
                .values()
                .any(|pending| pending.client.client_id == client_id)
        {
            return Err("pairing_busy");
        }
        let certificate = crypto::parse_certificate(&pem, Some(now))?;
        if document
            .clients
            .iter()
            .any(|client| client.certificate_id == certificate.fingerprint)
        {
            return Err("already_paired");
        }
        let salt: [u8; 16] = crypto::unhex(&salt)?
            .try_into()
            .map_err(|_| "invalid_request")?;
        let pairing_id = crypto::hex(&crypto::random_bytes::<16>()?);
        let expires_at = now.checked_add(120).ok_or("clock_failure")?;
        self.pending.insert(
            pairing_id.clone(),
            Transcript {
                owner: caller.into(),
                client: Client {
                    certificate_id: certificate.fingerprint,
                    client_id,
                    name,
                    certificate_pem: pem,
                    paired_at: 0,
                    permissions: ClientPermissions::default(),
                    config_overrides: ClientConfigOverrides::default(),
                },
                salt,
                expires_at,
                phase: Phase::AwaitingPin,
            },
        );
        Ok(Change::Read(
            json!({"ok":true,"pending":true,"pairing_id":pairing_id,"expires_at":expires_at}),
        ))
    }
    fn take_owned(&mut self, caller: &str, id: &str) -> Result<Transcript, &'static str> {
        if self
            .pending
            .get(id)
            .is_none_or(|pending| pending.owner != caller)
        {
            return Err("pairing_not_found");
        }
        self.pending.remove(id).ok_or("pairing_not_found")
    }
    fn approve(&mut self, id: String, pin: &str) -> Result<Change, &'static str> {
        let mut pending = self.pending.remove(&id).ok_or("pairing_not_found")?;
        pending.approve(pin)?;
        self.pending.insert(id, pending);
        Ok(Change::Read(json!({"ok":true})))
    }
    fn advance(
        &mut self,
        caller: &str,
        id: String,
        document: &Document,
        step: impl FnOnce(&mut Transcript, &Identity) -> Result<Value, &'static str>,
    ) -> Result<Change, &'static str> {
        let mut pending = self.take_owned(caller, &id)?;
        // Removing before execution consumes malformed/out-of-order transcripts.
        let response = step(
            &mut pending,
            document.identity.as_ref().ok_or("not_initialized")?,
        )?;
        self.pending.insert(id, pending);
        Ok(Change::Read(response))
    }
    fn finish(
        &mut self,
        caller: &str,
        id: String,
        proof: &str,
        mut document: Document,
        now: u64,
    ) -> Result<Change, &'static str> {
        let mut client = self.take_owned(caller, &id)?.finish(proof, now)?;
        if !document.clients.is_empty() {
            client.permissions = ClientPermissions::subsequent_client();
        }
        if document.clients.len() >= MAX_CLIENTS
            || document
                .clients
                .iter()
                .any(|existing| existing.certificate_id == client.certificate_id)
        {
            return Err("client_limit");
        }
        let response = json!({"ok":true,"paired":1,"certificate_id":client.certificate_id});
        document.clients.push(client);
        write(document, response, now)
    }
    fn unpair(
        &mut self,
        mut document: Document,
        id: &str,
        now: u64,
    ) -> Result<Change, &'static str> {
        self.remove_client(&mut document, id);
        write(document, json!({"ok":true}), now)
    }
    /// Removes the one client whose certificate fingerprint matches `pem`.
    /// An unknown certificate is not an error: the result is idempotent and
    /// reports `unpaired:0` without writing.
    fn unpair_presented(
        &mut self,
        mut document: Document,
        pem: &str,
        now: u64,
    ) -> Result<Change, &'static str> {
        // Expiry is deliberately not checked: removing trust is always safe.
        let fingerprint = crypto::parse_certificate(pem, None)?.fingerprint;
        if !self.remove_client(&mut document, &fingerprint) {
            return Ok(Change::Read(
                json!({"ok":true,"unpaired":0,"certificate_id":fingerprint}),
            ));
        }
        write(
            document,
            json!({"ok":true,"unpaired":1,"certificate_id":fingerprint}),
            now,
        )
    }
    /// Drops the committed client and any pending transcript for `id`; returns
    /// whether a committed client was removed.
    fn remove_client(&mut self, document: &mut Document, id: &str) -> bool {
        let committed = document.clients.len();
        document
            .clients
            .retain(|client| client.certificate_id != id);
        self.pending
            .retain(|_, pending| pending.client.certificate_id != id);
        document.clients.len() != committed
    }
    fn update_client_config_overrides(
        mut document: Document,
        certificate_id: &str,
        config_overrides: ClientConfigOverrides,
        now: u64,
    ) -> Result<Change, &'static str> {
        config_overrides.validate()?;
        let client = document
            .clients
            .iter_mut()
            .find(|client| client.certificate_id == certificate_id)
            .ok_or("not_paired")?;
        client.config_overrides = config_overrides;
        write(
            document,
            json!({"ok":true,"certificate_id":certificate_id}),
            now,
        )
    }
    fn update_client_permissions(
        mut document: Document,
        certificate_id: &str,
        permissions: ClientPermissions,
        now: u64,
    ) -> Result<Change, &'static str> {
        let client = document
            .clients
            .iter_mut()
            .find(|client| client.certificate_id == certificate_id)
            .ok_or("not_paired")?;
        client.permissions = permissions;
        write(
            document,
            json!({"ok":true,"certificate_id":certificate_id}),
            now,
        )
    }
    fn check_certificate(document: &Document, pem: &str, now: u64) -> Result<Change, &'static str> {
        let certificate = crypto::parse_certificate(pem, Some(now))?;
        let client = document
            .clients
            .iter()
            .find(|client| client.certificate_id == certificate.fingerprint)
            .ok_or("not_paired")?;
        Ok(Change::Read(
            json!({"ok":true,"paired":1,"certificate_id":client.certificate_id,"client_id":client.client_id,"config_overrides":client.config_overrides,"permissions":client.permissions,"permission_mode_enabled":document.permission_mode_enabled}),
        ))
    }
}
fn identity_response(identity: &Identity) -> Value {
    json!({"ok":true,"host_id":identity.host_id,"certificate_pem":identity.certificate_pem,"certificate_id":crypto::parse_certificate(&identity.certificate_pem,None).map(|certificate|certificate.fingerprint).unwrap_or_default()})
}
fn write(document: Document, response: Value, now: u64) -> Result<Change, &'static str> {
    document.validate(now).map_err(|_| "resource_limit")?;
    Ok(Change::Write { document, response })
}

fn client_metadata(document: &Document) -> Value {
    let clients: Vec<_> = document
        .clients
        .iter()
        .map(|client| {
            json!({
                "certificate_id": client.certificate_id,
                "client_id": client.client_id,
                "name": client.name,
                "paired_at": client.paired_at,
                "permissions": client.permissions,
                "config_overrides": client.config_overrides
            })
        })
        .collect();
    json!({"ok":true,"clients":clients,"permission_mode_enabled":document.permission_mode_enabled})
}

fn client_authorization(document: &Document, certificate_id: &str, now: u64) -> Value {
    let client = document.clients.iter().find(|client| {
        client.certificate_id == certificate_id
            && crypto::parse_certificate(&client.certificate_pem, Some(now)).is_ok()
    });
    let permissions = client.map(|client| client.permissions);
    let unrestricted = client.is_some() && !document.permission_mode_enabled;
    json!({
        "ok": true,
        "certificate_id": certificate_id,
        "paired": client.is_some(),
        "may_view": unrestricted || permissions.is_some_and(|permissions| permissions.view_streams || permissions.launch_apps),
        "may_launch": unrestricted || permissions.is_some_and(|permissions| permissions.launch_apps),
        "may_terminate": unrestricted || permissions.is_some_and(|permissions| permissions.terminate_streams),
        "name": client.map(|client| client.name.as_str()),
    })
}

#[cfg(test)]
mod authorization_tests {
    use super::*;

    #[test]
    fn expired_certificate_record_never_retains_observed_permissions() {
        let pem = include_str!("../tests/fixtures/client-cert.pem");
        let certificate = crypto::parse_certificate(pem, None).unwrap();
        let expires = certificate
            .certificate
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration()
            .as_secs();
        let document = Document {
            identity: None,
            clients: vec![Client {
                certificate_id: certificate.fingerprint.clone(),
                client_id: "metadata".into(),
                name: "Expired client".into(),
                certificate_pem: pem.into(),
                paired_at: expires - 1,
                permissions: ClientPermissions::default(),
                config_overrides: ClientConfigOverrides::default(),
            }],
            permission_mode_enabled: false,
        };
        let before = client_authorization(&document, &certificate.fingerprint, expires - 1);
        assert_eq!(before["paired"], true);
        let expired = client_authorization(&document, &certificate.fingerprint, expires);
        assert_eq!(expired["paired"], false);
        for permission in ["may_view", "may_launch", "may_terminate"] {
            assert_eq!(expired[permission], false);
        }
        assert!(expired["name"].is_null());
    }
}
