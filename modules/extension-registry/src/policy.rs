//! Trusted installation policy and broker approval matching.
use crate::settings::Scope;
use amaterasu_configuration_schema::Fields;
use amaterasu_contracts::{ApiMethod, ApiMethodApproval};
use amaterasu_extension_api::{ExtensionDescriptor, MethodRef, validate_descriptor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub descriptors: Vec<InstalledDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledDescriptor {
    pub descriptor: ExtensionDescriptor,
    pub descriptor_sha256: String,
    pub grants: Vec<Grant>,
    pub approved_methods: Vec<ApiMethod>,
    #[serde(default)]
    pub migrations: Vec<MigrationPlan>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationPlan {
    pub from_schema_revision: u64,
    pub old_fields: Fields,
    pub old_allowed: BTreeMap<String, BTreeSet<String>>,
    pub renames: BTreeMap<String, String>,
    pub removed: BTreeSet<String>,
    pub replacements: Vec<crate::settings::Layer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub caller: String,
    pub scope: GrantedScope,
    pub read: bool,
    pub write: bool,
}

/// Installation-only permissions. Wildcards never appear in a stored layer or
/// a settings request; they grant a trusted caller access to a class of scopes.
#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GrantedScope {
    Host,
    Application(String),
    PairedClient(String),
    Session(String),
    AllApplications,
    AllPairedClients,
    AllSessions,
}

impl GrantedScope {
    pub(crate) fn matches(&self, scope: &Scope) -> bool {
        match (self, scope) {
            (Self::Host, Scope::Host) => true,
            (Self::Application(granted), Scope::Application(requested))
            | (Self::PairedClient(granted), Scope::PairedClient(requested))
            | (Self::Session(granted), Scope::Session(requested)) => granted == requested,
            (Self::AllApplications, Scope::Application(_))
            | (Self::AllPairedClients, Scope::PairedClient(_))
            | (Self::AllSessions, Scope::Session(_)) => true,
            _ => false,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Application(id) | Self::PairedClient(id) | Self::Session(id) => {
                !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
            }
            _ => true,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.descriptors.len() > 128 {
            return Err("too many descriptors".into());
        }
        let mut providers = BTreeSet::new();
        for item in &self.descriptors {
            validate_descriptor(&item.descriptor)?;
            if !providers.insert(&item.descriptor.provider) || item.grants.len() > 512 {
                return Err("duplicate provider or too many grants".into());
            }
            let bytes = serde_json::to_vec(&item.descriptor).map_err(|error| error.to_string())?;
            let digest = format!("{:x}", Sha256::digest(bytes));
            if digest != item.descriptor_sha256 {
                return Err("descriptor digest mismatch".into());
            }
            let mut migration_revisions = BTreeSet::new();
            for plan in &item.migrations {
                if plan.from_schema_revision == 0
                    || plan.from_schema_revision >= item.descriptor.revision
                    || !migration_revisions.insert(plan.from_schema_revision)
                {
                    return Err("invalid or duplicate migration plan".into());
                }
            }
            for grant in &item.grants {
                if grant.caller.is_empty()
                    || grant.caller.len() > 128
                    || (!grant.read && !grant.write)
                    || !grant.scope.valid()
                {
                    return Err("invalid descriptor grant".into());
                }
            }
        }
        Ok(())
    }
}

impl InstalledDescriptor {
    pub fn allows(&self, caller: &str, scope: &Scope, write: bool) -> bool {
        self.grants.iter().any(|grant| {
            grant.caller == caller
                && grant.scope.matches(scope)
                && if write { grant.write } else { grant.read }
        })
    }
}

/// Approval must come from the broker for the current parent request. A
/// descriptor or its digest cannot grant a method or choose its provider.
pub fn approved(
    item: &InstalledDescriptor,
    reference: &MethodRef,
    approvals: &[ApiMethodApproval],
) -> bool {
    let matches = |method: &ApiMethod| {
        method.service == reference.service
            && method.method == reference.method
            && method.interface_major == reference.interface_major
            && method.interface_minor == reference.interface_minor
    };
    let Some(expected) = item.approved_methods.iter().find(|method| matches(method)) else {
        return false;
    };
    approvals.iter().any(|approval| {
        approval.provider == item.descriptor.provider
            && matches(&approval.method)
            && approval.method == *expected
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn reference() -> MethodRef {
        MethodRef {
            service: "provider.api/1".into(),
            method: "validate".into(),
            interface_major: 1,
            interface_minor: 2,
            schema_revision: 1,
        }
    }

    #[test]
    fn approved_schema_must_match_exact_installed_pin() {
        let descriptor = serde_json::from_value(json!({"provider":"owner","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"provider.api/1","method":"validate","interface_major":1,"interface_minor":2,"schema_revision":1}})).unwrap();
        let method: ApiMethod = serde_json::from_value(json!({"service":"provider.api/1","method":"validate","interface_major":1,"interface_minor":2,"request":{"required":{},"optional":{},"allow_extra":false},"response":{"required":{},"optional":{},"allow_extra":false},"privilege":"user"})).unwrap();
        let item = InstalledDescriptor {
            descriptor,
            descriptor_sha256: String::new(),
            grants: vec![],
            approved_methods: vec![method.clone()],
            migrations: vec![],
        };
        let mut approval = ApiMethodApproval {
            provider: "owner".into(),
            callers: Default::default(),
            method,
        };
        assert!(approved(
            &item,
            &reference(),
            std::slice::from_ref(&approval)
        ));
        approval.method.request.allow_extra = true;
        assert!(!approved(
            &item,
            &reference(),
            std::slice::from_ref(&approval)
        ));
        approval.method.request.allow_extra = false;
        approval.provider = "other".into();
        assert!(!approved(&item, &reference(), &[approval]));
    }

    #[test]
    fn installed_grants_never_authorize_different_identity_or_scope() {
        let descriptor = serde_json::from_value(json!({"provider":"owner","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"provider.api/1","method":"validate","interface_major":1,"interface_minor":2,"schema_revision":1}})).unwrap();
        let item = InstalledDescriptor {
            descriptor,
            descriptor_sha256: String::new(),
            approved_methods: vec![],
            migrations: vec![],
            grants: vec![Grant {
                caller: "management".into(),
                scope: GrantedScope::Host,
                read: true,
                write: false,
            }],
        };
        assert!(item.allows("management", &Scope::Host, false));
        assert!(!item.allows("management", &Scope::Host, true));
        assert!(!item.allows("other", &Scope::Host, false));
        assert!(!item.allows("management", &Scope::Session("s".into()), false));
    }

    #[test]
    fn class_grants_match_only_the_declared_kind_and_never_become_request_scopes() {
        for (document, selected, other) in [
            (
                "all_applications",
                Scope::Application("app".into()),
                Scope::PairedClient("app".into()),
            ),
            (
                "all_paired_clients",
                Scope::PairedClient("client".into()),
                Scope::Session("client".into()),
            ),
            (
                "all_sessions",
                Scope::Session("session".into()),
                Scope::Application("session".into()),
            ),
        ] {
            let value = json!({"kind":document});
            let grant: GrantedScope = serde_json::from_value(value.clone()).unwrap();
            assert!(grant.matches(&selected));
            assert!(!grant.matches(&other));
            assert!(!grant.matches(&Scope::Host));
            assert!(serde_json::from_value::<Scope>(value).is_err());
        }
        assert!(
            serde_json::from_value::<GrantedScope>(json!({"kind":"all_sessions","id":"ignored"}))
                .is_err()
        );
    }
}
