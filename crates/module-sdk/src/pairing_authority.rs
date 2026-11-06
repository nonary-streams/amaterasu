//! Read-only observations from the selected pairing authority.
//!
//! A fingerprint identifies a previously accepted certificate record. This
//! operation does not authenticate a connection, prove private-key possession,
//! or grant input/stream access. Consumers must recheck current authority before
//! effects and retain exact resource ownership until cleanup is confirmed.
use crate::{Command, ModuleResult, requests::RequestOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SERVICE: &str = "amaterasu.pairing/1";
pub const MAX_RESPONSE_BYTES: usize = 2_048;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    ClientAuthorization { certificate_id: String },
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        let Self::ClientAuthorization { certificate_id } = self;
        valid_fingerprint(certificate_id)
            .then_some(())
            .ok_or("invalid_certificate_fingerprint")
    }
}

/// Effective certificate-bound privileges at one committed authority revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    pub ok: bool,
    pub certificate_id: String,
    pub paired: bool,
    /// Older providers omit this field; absence grants no listing access.
    #[serde(default)]
    pub may_list: bool,
    pub may_view: bool,
    pub may_launch: bool,
    pub may_terminate: bool,
    pub revision: u64,
    pub name: Option<String>,
}

impl Authorization {
    /// Match the queried fingerprint and validate all observations. An unknown
    /// or expired certificate has no effective privileges and no display label.
    pub fn parse(expected_certificate_id: &str, payload: &Value) -> Result<Self, &'static str> {
        if !valid_fingerprint(expected_certificate_id)
            || !serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
            || payload.get("name").is_none()
        {
            return Err("invalid_authorization_response");
        }
        let response: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "invalid_authorization_response")?;
        if !response.ok
            || response.certificate_id != expected_certificate_id
            || (!response.paired
                && (response.may_list
                    || response.may_view
                    || response.may_launch
                    || response.may_terminate
                    || response.name.is_some()))
            || response.name.as_ref().is_some_and(|name| {
                name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
            })
            || (response.may_launch && !response.may_view)
        {
            return Err("invalid_authorization_response");
        }
        Ok(response)
    }
}

/// Build a bounded asynchronous request to the selected authority. Retrying an
/// uncertain admission requires the exact same command and operation identity;
/// consumers still own result consumption, cancellation and retirement.
pub fn command(options: RequestOptions<'_>, request: &Request) -> ModuleResult<Command> {
    if options.service != SERVICE {
        return Err("pairing_authority_service_mismatch".into());
    }
    request.validate()?;
    if options.timeout_ms == 0
        || options.timeout_ms > 30_000
        || options.operation_id.is_empty()
        || options.operation_id.len() > 128
        || options.operation_id.chars().any(char::is_control)
    {
        return Err("invalid_authorization_request_options".into());
    }
    Ok(Command::Request {
        service: SERVICE.into(),
        payload: serde_json::to_value(request).map_err(|error| error.to_string())?,
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.into(),
        parent: options.parent,
    })
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(fingerprint: &str) -> Value {
        json!({"ok":true,"certificate_id":fingerprint,"paired":true,"may_list":true,"may_view":true,"may_launch":false,"may_terminate":false,"revision":7,"name":"Player"})
    }

    #[test]
    fn unchanged_consumer_accepts_independent_authority_implementations() {
        fn consume(query: &str, invoke: impl FnOnce(Command) -> Value) -> Authorization {
            let request = Request::ClientAuthorization {
                certificate_id: query.into(),
            };
            let command = command(
                RequestOptions {
                    service: SERVICE,
                    timeout_ms: 1_000,
                    operation_id: "authorization-test",
                    parent: None,
                },
                &request,
            )
            .unwrap();
            Authorization::parse(query, &invoke(command)).unwrap()
        }
        let fingerprint = "ab".repeat(32);
        let first = consume(&fingerprint, |command| {
            let Command::Request { payload: query, .. } = command else {
                panic!("request");
            };
            assert_eq!(
                query,
                json!({"action":"client_authorization","certificate_id":fingerprint})
            );
            payload(&fingerprint)
        });
        let replacement = consume(&fingerprint, |command| {
            let Command::Request {
                service,
                payload: query,
                ..
            } = command
            else {
                panic!("request");
            };
            assert_eq!(service, SERVICE);
            json!({"ok":true,"certificate_id":query["certificate_id"],"paired":false,"may_list":false,"may_view":false,"may_launch":false,"may_terminate":false,"revision":8,"name":null})
        });
        assert!(first.paired && first.may_list && first.may_view && !first.may_launch);
        assert!(!replacement.paired && !replacement.may_list);
        assert_eq!(replacement.revision, 8);
    }

    #[test]
    fn rejects_wrong_identity_unbounded_or_inconsistent_authority_facts() {
        let fingerprint = "ab".repeat(32);
        let valid = payload(&fingerprint);
        for (key, value) in [
            ("certificate_id", json!("cd".repeat(32))),
            ("ok", json!(false)),
            ("paired", json!(false)),
            ("may_view", json!(false)),
            ("name", json!("x".repeat(129))),
            ("name", json!("player\n")),
            ("extra", json!(1)),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            // View-only denial is a valid observation unless launch remains
            // allowed, so make this specific case internally inconsistent.
            if key == "may_view" {
                invalid["may_launch"] = json!(true);
            }
            assert!(
                Authorization::parse(&fingerprint, &invalid).is_err(),
                "{invalid}"
            );
        }
        let mut missing = valid;
        missing.as_object_mut().unwrap().remove("name");
        assert!(Authorization::parse(&fingerprint, &missing).is_err());
        assert!(
            Request::ClientAuthorization {
                certificate_id: "AB".repeat(32)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn absent_list_bit_from_older_authority_defaults_to_least_privilege() {
        let fingerprint = "ab".repeat(32);
        let mut legacy = payload(&fingerprint);
        legacy.as_object_mut().unwrap().remove("may_list");
        let observation = Authorization::parse(&fingerprint, &legacy).unwrap();
        assert!(!observation.may_list);
        assert!(observation.may_view);
        let mut unpaired = legacy;
        unpaired["paired"] = json!(false);
        unpaired["may_view"] = json!(false);
        unpaired["name"] = Value::Null;
        unpaired["may_list"] = json!(true);
        assert!(Authorization::parse(&fingerprint, &unpaired).is_err());
    }
}
