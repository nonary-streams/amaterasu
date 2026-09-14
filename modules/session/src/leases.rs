use crate::protocol::{DomainError, DomainResult, LeaseMode};
use std::collections::BTreeMap;

const MAX_RESOURCES: usize = 1024;

#[derive(Default)]
pub struct LeaseRegistry {
    owners: BTreeMap<String, BTreeMap<u64, LeaseMode>>,
}

impl LeaseRegistry {
    pub fn acquire(&mut self, session: u64, resource: &str, mode: LeaseMode) -> DomainResult<bool> {
        if resource.is_empty() || resource.len() > 256 {
            return Err(DomainError::new(
                "invalid_resource",
                "resource must contain 1 to 256 bytes",
            ));
        }
        if let Some(owners) = self.owners.get(resource) {
            if let Some(existing) = owners.get(&session) {
                return if *existing == mode {
                    Ok(false)
                } else {
                    Err(DomainError::new(
                        "lease_mode_conflict",
                        "release the existing lease before changing its mode",
                    ))
                };
            }
            if mode == LeaseMode::Exclusive
                || owners.values().any(|held| *held == LeaseMode::Exclusive)
            {
                return Err(DomainError::new(
                    "resource_busy",
                    "resource has incompatible owners",
                ));
            }
        } else if self.owners.len() >= MAX_RESOURCES {
            return Err(DomainError::new(
                "resource_limit",
                "resource registry is full",
            ));
        }
        self.owners
            .entry(resource.to_owned())
            .or_default()
            .insert(session, mode);
        Ok(true)
    }

    pub fn release(&mut self, session: u64, resource: &str) -> bool {
        let Some(owners) = self.owners.get_mut(resource) else {
            return false;
        };
        let removed = owners.remove(&session).is_some();
        if owners.is_empty() {
            self.owners.remove(resource);
        }
        removed
    }
}
