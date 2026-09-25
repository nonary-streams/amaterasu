//! Testable policy for leases that temporarily assign Windows render defaults.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultRole {
    Console,
    Multimedia,
    Communications,
}

impl DefaultRole {
    pub const ALL: [Self; 3] = [Self::Console, Self::Multimedia, Self::Communications];

    pub const fn index(self) -> usize {
        match self {
            Self::Console => 0,
            Self::Multimedia => 1,
            Self::Communications => 2,
        }
    }
}

pub type DefaultEndpoints = [Option<String>; 3];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleAssignment {
    pub role: DefaultRole,
    pub endpoint: String,
}

/// A route lease may only become active when every role it planned to change
/// actually resolves to the selected endpoint. Another actor can change a
/// role between the initial snapshot and the final platform query.
pub fn assignments_reached_target(
    assignments: &[RoleAssignment],
    current: &DefaultEndpoints,
) -> bool {
    assignments.iter().all(|assignment| {
        current[assignment.role.index()].as_deref() == Some(assignment.endpoint.as_str())
    })
}

#[derive(Debug, Eq, PartialEq)]
pub enum AcquirePlan {
    Unchanged,
    Shared,
    Assign(Vec<RoleAssignment>),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReleasePlan {
    Shared,
    Restore(Vec<RoleAssignment>),
}

struct ActiveLease {
    endpoint: String,
    original: DefaultEndpoints,
    assigned_roles: [bool; 3],
    references: usize,
}

#[derive(Default)]
pub struct RoutePolicy {
    active: Option<ActiveLease>,
}

impl RoutePolicy {
    pub const fn new() -> Self {
        Self { active: None }
    }

    pub fn acquire(
        &mut self,
        endpoint: &str,
        current: &DefaultEndpoints,
    ) -> Result<AcquirePlan, &'static str> {
        if let Some(active) = &mut self.active {
            if active.references == 0 {
                return Err("audio_route_restore_failed");
            }
            if active.endpoint != endpoint {
                return Err("audio_route_conflict");
            }
            active.references = active
                .references
                .checked_add(1)
                .ok_or("audio_route_conflict")?;
            return Ok(AcquirePlan::Shared);
        }

        // Vibeshine leaves all roles alone when the selected endpoint is the
        // current console default, even when multimedia or communications use
        // another device.
        if current[DefaultRole::Console.index()].as_deref() == Some(endpoint) {
            return Ok(AcquirePlan::Unchanged);
        }

        let assignments: Vec<_> = DefaultRole::ALL
            .into_iter()
            .filter_map(|role| {
                current[role.index()]
                    .as_ref()
                    .filter(|current| current.as_str() != endpoint)
                    .map(|_| RoleAssignment {
                        role,
                        endpoint: endpoint.to_owned(),
                    })
            })
            .collect();
        if assignments.is_empty() {
            return Ok(AcquirePlan::Unchanged);
        }

        self.active = Some(ActiveLease {
            endpoint: endpoint.to_owned(),
            original: current.clone(),
            assigned_roles: [false; 3],
            references: 1,
        });
        Ok(AcquirePlan::Assign(assignments))
    }

    /// Records one role after the platform successfully applies the assignment.
    pub fn record_assignment(
        &mut self,
        endpoint: &str,
        role: DefaultRole,
    ) -> Result<(), &'static str> {
        let active = self.active.as_mut().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references != 1 {
            return Err("audio_route_unavailable");
        }
        active.assigned_roles[role.index()] = true;
        Ok(())
    }

    pub fn release(
        &mut self,
        endpoint: &str,
        current: &DefaultEndpoints,
    ) -> Result<ReleasePlan, &'static str> {
        let active = self.active.as_mut().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references == 0 {
            return Err("audio_route_unavailable");
        }
        active.references -= 1;
        if active.references != 0 {
            return Ok(ReleasePlan::Shared);
        }
        Ok(ReleasePlan::Restore(restore_assignments(active, current)))
    }

    /// Drops a lease when the platform cannot query current defaults. The
    /// final lease stays pending so a later acquisition can attempt recovery.
    pub fn release_without_snapshot(&mut self, endpoint: &str) -> Result<(), &'static str> {
        let active = self.active.as_mut().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references == 0 {
            return Err("audio_route_unavailable");
        }
        active.references -= 1;
        Ok(())
    }

    /// Converts an unsuccessful first assignment into pending cleanup.
    pub fn abort_acquire(&mut self, endpoint: &str) -> Result<(), &'static str> {
        let active = self.active.as_mut().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references != 1 {
            return Err("audio_route_unavailable");
        }
        active.references = 0;
        Ok(())
    }

    pub fn pending_restore_assignments(
        &self,
        current: &DefaultEndpoints,
    ) -> Option<Vec<RoleAssignment>> {
        let active = self.active.as_ref()?;
        (active.references == 0).then(|| restore_assignments(active, current))
    }

    pub fn complete_restore(&mut self, succeeded: bool) {
        if succeeded
            && self
                .active
                .as_ref()
                .is_some_and(|active| active.references == 0)
        {
            self.active = None;
        }
    }

    pub fn endpoint_with_pending_restore(&self) -> Option<&str> {
        self.active
            .as_ref()
            .filter(|active| active.references == 0)
            .map(|active| active.endpoint.as_str())
    }
}

fn restore_assignments(active: &ActiveLease, current: &DefaultEndpoints) -> Vec<RoleAssignment> {
    DefaultRole::ALL
        .into_iter()
        .filter_map(|role| {
            let original = active.original[role.index()].as_ref()?;
            (active.assigned_roles[role.index()]
                && original != &active.endpoint
                && current[role.index()].as_deref() == Some(active.endpoint.as_str()))
            .then(|| RoleAssignment {
                role,
                endpoint: original.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_role_change_rejects_acquire_and_rolls_back_only_owned_roles() {
        let original = [
            Some("render.console".into()),
            Some("render.multimedia".into()),
            Some("render.communications".into()),
        ];
        let mut policy = RoutePolicy::new();
        let AcquirePlan::Assign(assignments) =
            policy.acquire("render.selected", &original).unwrap()
        else {
            panic!("expected role assignment");
        };
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Communications)
            .unwrap();
        let concurrent_change = [
            Some("render.selected".into()),
            Some("render.other".into()),
            Some("render.selected".into()),
        ];
        assert!(!assignments_reached_target(
            &assignments,
            &concurrent_change
        ));
        policy.abort_acquire("render.selected").unwrap();
        assert_eq!(
            policy
                .pending_restore_assignments(&concurrent_change)
                .unwrap(),
            vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.communications".into(),
                },
            ]
        );
        let restored = [
            Some("render.console".into()),
            Some("render.other".into()),
            Some("render.communications".into()),
        ];
        assert!(
            policy
                .pending_restore_assignments(&restored)
                .unwrap()
                .is_empty()
        );
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    fn endpoints(console: &str, multimedia: &str, communications: &str) -> DefaultEndpoints {
        [console, multimedia, communications].map(|endpoint| Some(endpoint.into()))
    }

    fn assignments(endpoint: &str) -> Vec<RoleAssignment> {
        DefaultRole::ALL
            .into_iter()
            .map(|role| RoleAssignment {
                role,
                endpoint: endpoint.into(),
            })
            .collect()
    }

    #[test]
    fn routes_all_available_roles_and_restores_them_after_the_final_lease() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();

        assert_eq!(
            policy.acquire("render.selected", &original),
            Ok(AcquirePlan::Assign(assignments("render.selected")))
        );
        for role in DefaultRole::ALL {
            policy.record_assignment("render.selected", role).unwrap();
        }
        assert_eq!(
            policy.release(
                "render.selected",
                &endpoints("render.selected", "render.selected", "render.selected")
            ),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into()
                },
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into()
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into()
                },
            ]))
        );
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn virtual_sink_route_lease_restores_all_roles_after_capture_closes() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let routed = endpoints("render.virtual", "render.virtual", "render.virtual");
        let mut policy = RoutePolicy::new();

        assert!(matches!(
            policy.acquire("render.virtual", &original),
            Ok(AcquirePlan::Assign(_))
        ));
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }
        assert_eq!(
            policy.release("render.virtual", &routed),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ]))
        );
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn a_same_target_lease_shares_switch_and_restores_only_after_last_release() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let routed = endpoints("render.selected", "render.selected", "render.selected");
        let mut policy = RoutePolicy::new();

        assert!(matches!(
            policy.acquire("render.selected", &original),
            Ok(AcquirePlan::Assign(_))
        ));
        for role in DefaultRole::ALL {
            policy.record_assignment("render.selected", role).unwrap();
        }
        assert_eq!(
            policy.acquire("render.selected", &routed),
            Ok(AcquirePlan::Shared)
        );
        assert_eq!(
            policy.release("render.selected", &routed),
            Ok(ReleasePlan::Shared)
        );
        assert!(matches!(
            policy.acquire("render.other", &routed),
            Err("audio_route_conflict")
        ));
        assert!(matches!(
            policy.release("render.selected", &routed),
            Ok(ReleasePlan::Restore(_))
        ));
    }

    #[test]
    fn restoration_preserves_roles_changed_by_the_user() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let after_user_change =
            endpoints("render.selected", "render.user-choice", "render.selected");
        let mut policy = RoutePolicy::new();

        policy.acquire("render.selected", &original).unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Communications)
            .unwrap();
        assert_eq!(
            policy.release("render.selected", &after_user_change),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into()
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into()
                },
            ]))
        );
    }

    #[test]
    fn restoration_tracks_only_roles_this_lease_assigned() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let changed_after_snapshot =
            endpoints("render.selected", "render.selected", "render.selected");
        let mut policy = RoutePolicy::new();

        policy.acquire("render.selected", &original).unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Communications)
            .unwrap();

        assert_eq!(
            policy.release("render.selected", &changed_after_snapshot),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ]))
        );
    }

    #[test]
    fn same_console_endpoint_does_not_change_multimedia_or_communications_roles() {
        let current = endpoints("render.selected", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert_eq!(
            policy.acquire("render.selected", &current),
            Ok(AcquirePlan::Unchanged)
        );
    }

    #[test]
    fn partial_switch_rollback_only_restores_roles_that_reached_the_target() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let partially_routed = endpoints("render.selected", "render.multimedia", "render.selected");
        let mut policy = RoutePolicy::new();

        policy.acquire("render.selected", &original).unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Communications)
            .unwrap();
        assert_eq!(
            policy.release("render.selected", &partially_routed),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into()
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into()
                },
            ]))
        );
    }

    #[test]
    fn failed_partial_acquire_can_restore_reached_roles_without_overwriting_others() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let partially_routed = endpoints("render.selected", "render.multimedia", "render.selected");
        let mut policy = RoutePolicy::new();

        policy.acquire("render.selected", &original).unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        policy
            .record_assignment("render.selected", DefaultRole::Communications)
            .unwrap();
        policy.abort_acquire("render.selected").unwrap();

        assert_eq!(
            policy.pending_restore_assignments(&partially_routed),
            Some(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ])
        );
    }

    #[test]
    fn failed_default_query_releases_the_lease_into_recoverable_pending_state() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.selected", &original).unwrap();
        for role in DefaultRole::ALL {
            policy.record_assignment("render.selected", role).unwrap();
        }

        policy.release_without_snapshot("render.selected").unwrap();

        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.selected")
        );
        let current = endpoints("render.selected", "render.selected", "render.selected");
        assert_eq!(
            policy.pending_restore_assignments(&current),
            Some(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ])
        );
    }

    #[test]
    fn pending_restore_retries_owned_roles_and_releases_user_changed_roles() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.selected", &original).unwrap();
        for role in DefaultRole::ALL {
            policy.record_assignment("render.selected", role).unwrap();
        }
        policy.abort_acquire("render.selected").unwrap();

        let still_owned = endpoints("render.selected", "render.selected", "render.selected");
        assert_eq!(
            policy.pending_restore_assignments(&still_owned),
            Some(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ])
        );
        policy.complete_restore(false);
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.selected")
        );

        let user_changed = endpoints("render.user-choice", "render.selected", "render.selected");
        assert_eq!(
            policy.pending_restore_assignments(&user_changed),
            Some(vec![
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.comms".into(),
                },
            ])
        );

        let no_longer_owned = endpoints("render.user-choice", "render.multimedia", "render.comms");
        assert_eq!(
            policy.pending_restore_assignments(&no_longer_owned),
            Some(Vec::new())
        );
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }
}
