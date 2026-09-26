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
    Tracked,
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
    initially_selected_roles: [bool; 3],
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

    #[cfg(test)]
    pub fn acquire(
        &mut self,
        endpoint: &str,
        current: &DefaultEndpoints,
    ) -> Result<AcquirePlan, &'static str> {
        self.acquire_with_tracking(endpoint, current, false)
    }

    pub fn acquire_with_tracking(
        &mut self,
        endpoint: &str,
        current: &DefaultEndpoints,
        route_virtual_sink: bool,
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

        // The pinned source skips set_sink when its selected sink equals the
        // current host sink. Ordinary opens need no lease. A virtual open keeps
        // a passive lease so later changes to its selected roles can be
        // reasserted without altering other roles at the first poll.
        if current[DefaultRole::Console.index()].as_deref() == Some(endpoint) {
            if route_virtual_sink {
                self.active = Some(ActiveLease {
                    endpoint: endpoint.to_owned(),
                    original: current.clone(),
                    initially_selected_roles: std::array::from_fn(|index| {
                        current[index].as_deref() == Some(endpoint)
                    }),
                    assigned_roles: [false; 3],
                    references: 1,
                });
                return Ok(AcquirePlan::Tracked);
            }
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
            initially_selected_roles: std::array::from_fn(|index| {
                current[index].as_deref() == Some(endpoint)
            }),
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
        if active.endpoint != endpoint || active.references == 0 {
            return Err("audio_route_unavailable");
        }
        active.assigned_roles[role.index()] = true;
        Ok(())
    }

    /// A failed platform call can still have changed the role. Record the
    /// attempt first so final cleanup checks whether the target was reached.
    pub fn attempt_assignment(
        &mut self,
        endpoint: &str,
        role: DefaultRole,
        assign: impl FnOnce() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        self.record_assignment(endpoint, role)?;
        assign()
    }

    /// Reassert an active virtual sink after reading the current role. If
    /// that sink was the original default, preserve the newer live choice for
    /// final release. Record the attempt before the platform call because a
    /// failed call can still have changed the default.
    pub fn attempt_refresh_assignment(
        &mut self,
        endpoint: &str,
        role: DefaultRole,
        current: Option<&str>,
        assign: impl FnOnce() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let active = self.active.as_mut().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references == 0 {
            return Err("audio_route_unavailable");
        }
        let index = role.index();
        if active.initially_selected_roles[index]
            && let Some(chosen) = current.filter(|current| *current != endpoint)
        {
            active.original[index] = Some(chosen.to_owned());
        }
        active.assigned_roles[index] = true;
        assign()
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

    /// An absent default cannot establish whether our selected endpoint has
    /// relinquished a role. Keep its original endpoint until Windows reports
    /// either our endpoint again or a different live choice.
    pub fn pending_restore_complete(&self, current: &DefaultEndpoints) -> bool {
        let Some(active) = self.active.as_ref().filter(|active| active.references == 0) else {
            return false;
        };
        restore_assignments(active, current).is_empty()
            && DefaultRole::ALL.into_iter().all(|role| {
                !active.assigned_roles[role.index()]
                    || active.original[role.index()].as_deref() == Some(&active.endpoint)
                    || current[role.index()].is_some()
            })
    }

    /// A later explicit route to a different endpoint supersedes restoration
    /// of roles that still have no default. Never relinquish a role that still
    /// points to our selected endpoint; it must be restored first. Live roles
    /// chosen by another actor become the new route's own snapshot.
    pub fn abandon_absent_restore_for_new_route(
        &mut self,
        new_endpoint: &str,
        current: &DefaultEndpoints,
    ) -> bool {
        let Some(active) = self.active.as_ref().filter(|active| active.references == 0) else {
            return false;
        };
        if active.endpoint == new_endpoint
            || !restore_assignments(active, current).is_empty()
            || !DefaultRole::ALL.into_iter().any(|role| {
                active.assigned_roles[role.index()]
                    && active.original[role.index()].as_deref() != Some(&active.endpoint)
                    && current[role.index()].is_none()
            })
        {
            return false;
        }
        self.active = None;
        true
    }

    /// Resume a pending lease for the same explicitly selected endpoint. Only
    /// roles still absent are reassigned; a live choice relinquishes the old
    /// role and becomes the snapshot preserved for this new capture.
    pub fn resume_absent_restore(
        &mut self,
        endpoint: &str,
        current: &DefaultEndpoints,
    ) -> Option<Vec<RoleAssignment>> {
        let active = self.active.as_mut().filter(|active| {
            active.references == 0
                && active.endpoint == endpoint
                && restore_assignments(active, current).is_empty()
        })?;
        let assignments: Vec<_> = DefaultRole::ALL
            .into_iter()
            .filter(|role| {
                active.assigned_roles[role.index()]
                    && active.original[role.index()].as_deref() != Some(endpoint)
                    && current[role.index()].is_none()
            })
            .map(|role| RoleAssignment {
                role,
                endpoint: endpoint.to_owned(),
            })
            .collect();
        if assignments.is_empty() {
            return None;
        }
        for role in DefaultRole::ALL {
            if let Some(live_endpoint) = current[role.index()].as_ref()
                && live_endpoint != endpoint
            {
                active.original[role.index()] = Some(live_endpoint.clone());
                active.assigned_roles[role.index()] = false;
            }
        }
        active.references = 1;
        Some(assignments)
    }

    /// A routed virtual sink remains selected while its capture is active.
    /// Reassert a role this lease assigned even when it becomes unset; leave
    /// roles that were absent at acquisition alone. Preserve the initial
    /// snapshot for final restoration.
    pub fn refresh_assignments(
        &self,
        endpoint: &str,
        current: &DefaultEndpoints,
    ) -> Result<Vec<RoleAssignment>, &'static str> {
        let active = self.active.as_ref().ok_or("audio_route_unavailable")?;
        if active.endpoint != endpoint || active.references == 0 {
            return Err("audio_route_unavailable");
        }
        Ok(DefaultRole::ALL
            .into_iter()
            .filter(|role| {
                (active.assigned_roles[role.index()]
                    || active.original[role.index()].as_deref() == Some(endpoint))
                    && current[role.index()].as_deref() != Some(endpoint)
            })
            .map(|role| RoleAssignment {
                role,
                endpoint: endpoint.to_owned(),
            })
            .collect())
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
    fn virtual_sink_reasserts_changed_roles_then_restores_initial_defaults() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.virtual", &original).unwrap();
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }

        let changed = endpoints("render.user", "render.virtual", "render.virtual");
        assert_eq!(
            policy
                .refresh_assignments("render.virtual", &changed)
                .unwrap(),
            vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.virtual".into(),
            }]
        );
        assert_eq!(
            policy.release(
                "render.virtual",
                &endpoints("render.virtual", "render.virtual", "render.virtual")
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
    fn virtual_sink_reasserts_an_unset_owned_role_but_not_an_initially_absent_role() {
        let original = [
            Some("render.console".into()),
            Some("render.multimedia".into()),
            None,
        ];
        let mut policy = RoutePolicy::new();
        let AcquirePlan::Assign(assignments) = policy
            .acquire_with_tracking("render.virtual", &original, true)
            .unwrap()
        else {
            panic!("virtual sink must route the available roles");
        };
        for assignment in assignments {
            policy
                .record_assignment("render.virtual", assignment.role)
                .unwrap();
        }
        let missing = [None, Some("render.virtual".into()), None];
        assert_eq!(
            policy.refresh_assignments("render.virtual", &missing),
            Ok(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.virtual".into(),
            }])
        );
        let routed = [Some("render.virtual".into()), missing[1].clone(), None];
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
            ]))
        );
    }

    #[test]
    fn virtual_sink_already_default_tracks_shared_reassertion_and_final_release() {
        let original = endpoints("render.virtual", "render.multimedia", "render.comms");
        let changed = endpoints("render.user", "render.user-multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert_eq!(
            policy.acquire_with_tracking("render.virtual", &original, true),
            Ok(AcquirePlan::Tracked)
        );
        assert_eq!(
            policy.refresh_assignments("render.virtual", &original),
            Ok(Vec::new())
        );
        assert_eq!(
            policy.acquire_with_tracking("render.virtual", &original, true),
            Ok(AcquirePlan::Shared)
        );
        assert_eq!(
            policy.refresh_assignments("render.virtual", &changed),
            Ok(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.virtual".into(),
            }])
        );
        policy
            .attempt_refresh_assignment(
                "render.virtual",
                DefaultRole::Console,
                Some("render.user"),
                || Ok(()),
            )
            .unwrap();
        let reasserted = endpoints("render.virtual", "render.user-multimedia", "render.comms");
        assert_eq!(
            policy.release("render.virtual", &reasserted),
            Ok(ReleasePlan::Shared)
        );
        assert_eq!(
            policy.release("render.virtual", &reasserted),
            Ok(ReleasePlan::Restore(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.user".into(),
            }]))
        );
        policy.complete_restore(false);
        let restored = endpoints("render.user", "render.user-multimedia", "render.comms");
        assert!(policy.pending_restore_complete(&restored));
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn already_selected_virtual_route_recovers_failed_refresh_and_restores_new_default() {
        let virtual_sink = "render.virtual";
        let mut defaults = endpoints(virtual_sink, "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert_eq!(
            policy.acquire_with_tracking(virtual_sink, &defaults, true),
            Ok(AcquirePlan::Tracked)
        );

        // A user changes Console while this capture is active. A policy
        // interface failure must not forget either the choice or the retry.
        defaults[DefaultRole::Console.index()] = Some("render.user".into());
        let refresh = policy.refresh_assignments(virtual_sink, &defaults).unwrap();
        assert_eq!(refresh.len(), 1);
        assert_eq!(refresh[0].role, DefaultRole::Console);
        assert_eq!(
            policy.refresh_assignments(virtual_sink, &defaults).unwrap(),
            refresh
        );
        policy
            .attempt_refresh_assignment(
                virtual_sink,
                DefaultRole::Console,
                Some("render.user"),
                || Err("audio_route_change_failed"),
            )
            .unwrap_err();
        assert_eq!(
            policy.refresh_assignments(virtual_sink, &defaults).unwrap(),
            refresh
        );

        policy
            .attempt_refresh_assignment(
                virtual_sink,
                DefaultRole::Console,
                Some("render.user"),
                || {
                    defaults[DefaultRole::Console.index()] = Some(virtual_sink.into());
                    Ok(())
                },
            )
            .unwrap();
        assert!(
            policy
                .refresh_assignments(virtual_sink, &defaults)
                .unwrap()
                .is_empty()
        );
        defaults[DefaultRole::Console.index()] = Some("render.latest".into());
        assert_eq!(
            policy.refresh_assignments(virtual_sink, &defaults).unwrap(),
            refresh
        );
        policy
            .attempt_refresh_assignment(
                virtual_sink,
                DefaultRole::Console,
                Some("render.latest"),
                || {
                    defaults[DefaultRole::Console.index()] = Some(virtual_sink.into());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(
            policy.release(virtual_sink, &defaults),
            Ok(ReleasePlan::Restore(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.latest".into(),
            }]))
        );
        defaults[DefaultRole::Console.index()] = Some("render.latest".into());
        assert!(policy.pending_restore_complete(&defaults));
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn virtual_sink_retries_an_unset_initial_default_without_claiming_other_roles() {
        let original = [
            Some("render.virtual".into()),
            Some("render.user".into()),
            None,
        ];
        let mut current = [None, original[1].clone(), None];
        let mut policy = RoutePolicy::new();
        assert_eq!(
            policy.acquire_with_tracking("render.virtual", &original, true),
            Ok(AcquirePlan::Tracked)
        );

        let expected = vec![RoleAssignment {
            role: DefaultRole::Console,
            endpoint: "render.virtual".into(),
        }];
        assert_eq!(
            policy.refresh_assignments("render.virtual", &current),
            Ok(expected.clone())
        );
        assert_eq!(
            policy.attempt_assignment("render.virtual", DefaultRole::Console, || {
                Err("audio_route_change_failed")
            }),
            Err("audio_route_change_failed")
        );
        assert_eq!(
            policy.refresh_assignments("render.virtual", &current),
            Ok(expected)
        );
        policy
            .attempt_assignment("render.virtual", DefaultRole::Console, || {
                current[DefaultRole::Console.index()] = Some("render.virtual".into());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            policy.refresh_assignments("render.virtual", &current),
            Ok(Vec::new())
        );
        assert_eq!(
            policy.release("render.virtual", &current),
            Ok(ReleasePlan::Restore(Vec::new()))
        );
        assert!(policy.pending_restore_complete(&current));
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
        assert_eq!(current[DefaultRole::Multimedia.index()], original[1]);
        assert_eq!(current[DefaultRole::Communications.index()], None);
    }

    #[test]
    fn virtual_sink_with_every_role_already_selected_tracks_future_changes() {
        let original = endpoints("render.virtual", "render.virtual", "render.virtual");
        let mut policy = RoutePolicy::new();
        assert_eq!(
            policy.acquire_with_tracking("render.virtual", &original, true),
            Ok(AcquirePlan::Tracked)
        );
        let changed = endpoints("render.virtual", "render.user", "render.virtual");
        assert_eq!(
            policy.refresh_assignments("render.virtual", &changed),
            Ok(vec![RoleAssignment {
                role: DefaultRole::Multimedia,
                endpoint: "render.virtual".into(),
            }])
        );
        policy
            .record_assignment("render.virtual", DefaultRole::Multimedia)
            .unwrap();
        assert_eq!(
            policy.release("render.virtual", &original),
            Ok(ReleasePlan::Restore(Vec::new()))
        );
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
    fn failed_assignment_that_reached_target_remains_owned_for_restore() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.selected", &original).unwrap();

        let mut platform_called = false;
        assert_eq!(
            policy.attempt_assignment("render.selected", DefaultRole::Console, || {
                platform_called = true;
                Err("audio_route_change_failed")
            }),
            Err("audio_route_change_failed")
        );
        assert!(platform_called);
        policy.abort_acquire("render.selected").unwrap();

        let changed = endpoints("render.selected", "render.multimedia", "render.comms");
        assert_eq!(
            policy.pending_restore_assignments(&changed),
            Some(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.console".into(),
            }])
        );
        // If the platform returned an error before changing the role, cleanup
        // has nothing to restore and must leave the user's endpoint untouched.
        assert_eq!(policy.pending_restore_assignments(&original), Some(vec![]));
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

    #[test]
    fn missing_virtual_endpoint_keeps_restore_pending_until_role_recovers() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert!(matches!(
            policy.acquire_with_tracking("render.virtual", &original, true),
            Ok(AcquirePlan::Assign(_))
        ));
        policy
            .record_assignment("render.virtual", DefaultRole::Console)
            .unwrap();

        let missing = [None, original[1].clone(), original[2].clone()];
        assert_eq!(
            policy.release("render.virtual", &missing),
            Ok(ReleasePlan::Restore(Vec::new()))
        );
        assert!(!policy.pending_restore_complete(&missing));
        policy.complete_restore(policy.pending_restore_complete(&missing));
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.virtual")
        );

        let returned = endpoints("render.virtual", "render.multimedia", "render.comms");
        assert_eq!(
            policy.pending_restore_assignments(&returned),
            Some(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.console".into(),
            }])
        );
        assert!(!policy.pending_restore_complete(&returned));
        policy.complete_restore(policy.pending_restore_complete(&original));
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn new_route_supersedes_absent_restore_and_restores_its_own_live_roles() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert!(matches!(
            policy.acquire("render.virtual", &original),
            Ok(AcquirePlan::Assign(_))
        ));
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }
        let missing = [None, Some("render.live".into()), Some("render.user".into())];
        assert_eq!(
            policy.release("render.virtual", &missing),
            Ok(ReleasePlan::Restore(Vec::new()))
        );
        assert!(!policy.pending_restore_complete(&missing));
        assert!(!policy.abandon_absent_restore_for_new_route("render.virtual", &missing));
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.virtual")
        );

        // An explicit new route takes over only after all live roles have
        // stopped pointing to the old selection.
        assert!(policy.abandon_absent_restore_for_new_route("render.new", &missing));
        assert_eq!(policy.endpoint_with_pending_restore(), None);
        assert!(matches!(
            policy.acquire("render.new", &missing),
            Ok(AcquirePlan::Assign(_))
        ));
        policy
            .record_assignment("render.new", DefaultRole::Multimedia)
            .unwrap();
        policy
            .record_assignment("render.new", DefaultRole::Communications)
            .unwrap();
        assert_eq!(
            policy.release(
                "render.new",
                &[None, Some("render.new".into()), Some("render.new".into())]
            ),
            Ok(ReleasePlan::Restore(vec![
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.live".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Communications,
                    endpoint: "render.user".into(),
                },
            ]))
        );
    }

    #[test]
    fn new_route_waits_while_old_selected_endpoint_still_owns_a_live_role() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.virtual", &original).unwrap();
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }
        let partly_missing = [
            None,
            Some("render.virtual".into()),
            Some("render.user".into()),
        ];
        policy.release("render.virtual", &partly_missing).unwrap();
        assert!(!policy.abandon_absent_restore_for_new_route("render.new", &partly_missing));
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.virtual")
        );

        let returned = endpoints("render.virtual", "render.virtual", "render.user");
        assert!(!policy.abandon_absent_restore_for_new_route("render.new", &returned));
        assert_eq!(
            policy.pending_restore_assignments(&returned),
            Some(vec![
                RoleAssignment {
                    role: DefaultRole::Console,
                    endpoint: "render.console".into(),
                },
                RoleAssignment {
                    role: DefaultRole::Multimedia,
                    endpoint: "render.multimedia".into(),
                },
            ])
        );
        let restored = endpoints("render.console", "render.multimedia", "render.user");
        assert!(policy.pending_restore_complete(&restored));
        policy.complete_restore(true);
        assert!(matches!(
            policy.acquire("render.new", &restored),
            Ok(AcquirePlan::Assign(_))
        ));
    }

    #[test]
    fn same_route_reacquires_absent_role_and_keeps_its_original_restore_target() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        assert!(matches!(
            policy.acquire("render.virtual", &original),
            Ok(AcquirePlan::Assign(_))
        ));
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }
        let missing = [
            None,
            Some("render.user".into()),
            Some("render.other".into()),
        ];
        assert_eq!(
            policy.release("render.virtual", &missing),
            Ok(ReleasePlan::Restore(Vec::new()))
        );
        assert!(!policy.pending_restore_complete(&missing));
        policy.complete_restore(false);
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.virtual")
        );
        assert_eq!(
            policy.resume_absent_restore("render.virtual", &missing),
            Some(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.virtual".into(),
            }])
        );
        policy
            .record_assignment("render.virtual", DefaultRole::Console)
            .unwrap();
        let routed = endpoints("render.virtual", "render.user", "render.other");
        assert_eq!(
            policy.refresh_assignments("render.virtual", &routed),
            Ok(Vec::new())
        );
        assert_eq!(
            policy.release("render.virtual", &routed),
            Ok(ReleasePlan::Restore(vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.console".into(),
            }]))
        );
        let restored = endpoints("render.console", "render.user", "render.other");
        assert!(policy.pending_restore_complete(&restored));
        policy.complete_restore(true);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn same_route_does_not_resume_while_it_still_owns_a_live_role() {
        let original = endpoints("render.console", "render.multimedia", "render.comms");
        let mut policy = RoutePolicy::new();
        policy.acquire("render.virtual", &original).unwrap();
        for role in DefaultRole::ALL {
            policy.record_assignment("render.virtual", role).unwrap();
        }
        let partly_missing = [
            None,
            Some("render.virtual".into()),
            Some("render.user".into()),
        ];
        policy.release("render.virtual", &partly_missing).unwrap();
        assert_eq!(
            policy.resume_absent_restore("render.virtual", &partly_missing),
            None
        );
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some("render.virtual")
        );
    }
}
