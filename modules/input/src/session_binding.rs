//! Built-in input uses the public SDK lifecycle binding helpers.
pub(crate) use amaterasu_module_sdk::input::session_binding::{
    AcquireJob, BatchActivationJob, Bindings, RevokeRequest,
};

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_extension_api::LifecycleTarget;
    use amaterasu_module_sdk::input::session_binding::Authority;
    use amaterasu_module_sdk::{Command, ModuleResult, Response};
    use serde_json::json;
    use std::time::Instant;
    const LIFECYCLE_PRINCIPAL: &str = "amaterasu.session-lifecycle";
    const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
    use std::time::Duration;
    fn target() -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        }
    }
    #[test]
    fn binding_enforces_exact_lease_and_revocation_idempotence() {
        let mut bindings = Bindings::default();
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .bind(
                "token",
                "owner",
                "lease",
                &Authority {
                    target: target(),
                    geometry_revision: 1,
                    batch_epoch: "epoch".into(),
                    active: true,
                },
            )
            .unwrap();
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_ok()
        );
        assert_eq!(
            bindings.status(LIFECYCLE_PRINCIPAL, "token", &target()),
            Ok(("owner", "lease", 1))
        );
        assert!(
            bindings
                .status(LIFECYCLE_SERVICE, "token", &target())
                .is_err()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_SERVICE, "forged", "token", target())
                .is_err()
        );
        assert!(bindings.authorize_batch("owner", "lease", None).is_err());
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("wrong"))
                .is_err()
        );
        assert!(
            bindings
                .revoke("attacker", "revoke", "token", target())
                .is_err()
        );
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_ok()
        );
        assert_eq!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
                .unwrap(),
            ("owner".into(), Some("lease".into()))
        );
        assert!(
            bindings
                .authorize_batch("owner", "lease", Some("token"))
                .is_err()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
                .is_ok()
        );
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "other", "token", target())
                .is_err()
        );
        assert!(bindings.authorize_batch("owner", "legacy", None).is_ok());
    }
    #[test]
    fn active_validation_and_source_sequence_are_fenced_by_target_and_revoke() {
        let mut bindings = Bindings::default();
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .bind(
                "token",
                "owner",
                "lease",
                &Authority {
                    target: target(),
                    geometry_revision: 4,
                    batch_epoch: "epoch".into(),
                    active: false,
                },
            )
            .unwrap();
        assert!(!bindings.batch_active("owner", "lease", "token").unwrap());
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:1"),
            Err("binding_inactive")
        );
        let mut authority = Authority {
            target: target(),
            geometry_revision: 5,
            batch_epoch: "epoch".into(),
            active: true,
        };
        assert!(
            bindings
                .activate("owner", "lease", "token", &authority)
                .is_err()
        );
        authority.geometry_revision = 4;
        bindings
            .activate("owner", "lease", "token", &authority)
            .unwrap();
        assert!(bindings.batch_active("owner", "lease", "token").unwrap());
        bindings
            .admit_sequence("owner", "lease", "token", "epoch:1")
            .unwrap();
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:1"),
            Err("receipt_expired")
        );
        assert_eq!(
            bindings.admit_sequence("owner", "lease", "token", "epoch:3"),
            Err("sequence_conflict")
        );
        bindings
            .revoke(LIFECYCLE_PRINCIPAL, "revoke", "token", target())
            .unwrap();
        assert!(
            bindings
                .activate("owner", "lease", "token", &authority)
                .is_err()
        );
        assert!(
            bindings
                .admit_sequence("owner", "lease", "token", "epoch:2")
                .is_err()
        );
    }

    #[test]
    fn revoke_before_authority_cannot_bind_and_unknown_token_never_claims_retirement() {
        let mut bindings = Bindings::default();
        assert!(
            bindings
                .revoke(LIFECYCLE_PRINCIPAL, "r", "unknown", target())
                .is_err()
        );
        bindings.begin_acquire("token", "owner").unwrap();
        bindings
            .revoke(LIFECYCLE_PRINCIPAL, "r", "token", target())
            .unwrap();
        assert!(
            bindings
                .bind(
                    "token",
                    "owner",
                    "lease",
                    &Authority {
                        target: target(),
                        geometry_revision: 1,
                        batch_epoch: "epoch".into(),
                        active: true
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn acquire_uses_parent_bound_authority_and_rejects_forged_response() {
        let mut job = AcquireJob::new(
            7,
            "owner".into(),
            "token".into(),
            json!({}),
            Instant::now() + Duration::from_secs(1),
            "boot".into(),
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(7));
                    assert_eq!(payload["token"], "token");
                    assert_eq!(payload["input_boot_epoch"], "boot");
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":target(),"geometry_revision":1,"batch_epoch":"epoch","active":true,"allowed_actions":[]}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert_eq!(job.advance(&mut send).unwrap().unwrap().target, target());
        let mut denied = AcquireJob::new(
            7,
            "owner".into(),
            "token".into(),
            json!({}),
            Instant::now() + Duration::from_secs(1),
            "boot".into(),
        )
        .unwrap();
        let mut send = |command| -> ModuleResult<Response> {
            match command {
                Command::Request { .. } => Ok(Response::Accepted { id: 1 }),
                Command::TakeResult { .. } => Ok(Response::Result {
                    payload: json!({"status":"succeeded","target":{"kind":"host","host_epoch":"forged"},"geometry_revision":1,"batch_epoch":"epoch","active":true}),
                }),
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        denied.advance(&mut send).unwrap();
        assert!(denied.advance(&mut send).is_err());
    }
}
