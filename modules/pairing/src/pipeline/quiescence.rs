use super::*;

impl Pipeline {
    /// Retire accepted state operations without cancelling delivered writes.
    /// Unknown write admission and broker expiry remain fenced: neither proves
    /// that a provider has stopped executing the operation.
    pub(crate) fn quiesce_broker(&mut self, mut send: impl FnMut(Command) -> Transport) -> bool {
        let mut ready = true;
        if let Some(mut pending) = self.pending.take() {
            let mutation = matches!(&pending.stage, Stage::Saving { .. });
            if pending.provider_retirement_uncertain {
                self.pending = Some(pending);
                ready = false;
            } else {
            match pending.state_request {
                Some(request) => {
                    let result = match pending.result.take() {
                        Some(result) => Some(result),
                        None => match send(Command::TakeResult { request }) {
                            Ok(result @ Response::Result { .. })
                            | Ok(result @ Response::Error { .. }) => Some(result),
                            _ => None,
                        },
                    };
                    if let Some(result) = result {
                        if result_released(send(Command::ReleaseResult { request })) {
                            if mutation && matches!(result, Response::Error { .. }) {
                                self.identity.invalidate();
                                self.recovery_needed = true;
                                pending.provider_retirement_uncertain = true;
                                self.pending = Some(pending);
                                ready = false;
                            } else if mutation {
                                self.identity.invalidate();
                                self.recovery_needed = true;
                            }
                        } else {
                            pending.result = Some(result);
                            self.pending = Some(pending);
                            ready = false;
                        }
                    } else {
                        self.pending = Some(pending);
                        ready = false;
                    }
                }
                None if mutation && pending.admission_uncertain => {
                    match lookup(&pending.command, pending.first_sent, &mut send) {
                        Lookup::Found(id) => {
                            pending.state_request = Some(id);
                            self.pending = Some(pending);
                            ready = false;
                        }
                        Lookup::NotAdmitted => {}
                        Lookup::Unknown => {
                            self.pending = Some(pending);
                            ready = false;
                        }
                    }
                }
                None => {}
            }
            }
        }
        if let HostConfigurationState::WaitingMigrationMarkerDelete { request, .. } =
            &self.host_configuration
        {
            match send(Command::TakeResult { request: *request }) {
                Ok(Response::Result { .. }) => {
                    self.host_configuration = HostConfigurationState::Disabled;
                }
                _ => ready = false,
            }
        }
        if let HostConfigurationState::SubmittingMigrationMarkerDelete {
            command,
            first_sent,
            ..
        } = &self.host_configuration
        {
            match lookup(command, *first_sent, &mut send) {
                Lookup::Found(id) => {
                    let HostConfigurationState::SubmittingMigrationMarkerDelete {
                        paths,
                        expected_revision,
                        ..
                    } = std::mem::replace(
                        &mut self.host_configuration,
                        HostConfigurationState::Disabled,
                    )
                    else {
                        unreachable!()
                    };
                    self.host_configuration =
                        HostConfigurationState::WaitingMigrationMarkerDelete {
                            request: id,
                            paths,
                            expected_revision,
                        };
                    ready = false;
                }
                Lookup::NotAdmitted => self.host_configuration = HostConfigurationState::Disabled,
                Lookup::Unknown => ready = false,
            }
        }
        if let Some((request, _, _)) = self.recovery_request {
            match send(Command::TakeResult { request }) {
                Ok(Response::Result { .. } | Response::Error { .. }) => {
                    self.recovery_request = None;
                }
                _ => ready = false,
            }
        }
        if let Some(stage) = self.pairing_notice.take() {
            self.pairing_notice = match stage {
                PairingNoticeStage::Submitting {
                    command,
                    first_sent,
                } => match lookup(&command, first_sent, &mut send) {
                    Lookup::Found(id) => Some(PairingNoticeStage::Waiting(id)),
                    Lookup::NotAdmitted => None,
                    Lookup::Unknown => Some(PairingNoticeStage::Submitting {
                        command,
                        first_sent,
                    }),
                },
                PairingNoticeStage::Waiting(request) => {
                    match send(Command::TakeResult { request }) {
                        Ok(Response::Result { .. }) => None,
                        _ => Some(PairingNoticeStage::Waiting(request)),
                    }
                }
                PairingNoticeStage::Releasing(request) => {
                    match send(Command::ReleaseResult { request }) {
                        Ok(Response::Ok) => None,
                        Ok(Response::Error { code, .. }) if code == "not_found" => None,
                        _ => Some(PairingNoticeStage::Releasing(request)),
                    }
                }
            };
            ready &= self.pairing_notice.is_none();
        }
        ready
    }
}

pub(super) enum Lookup {
    Found(u64),
    NotAdmitted,
    Unknown,
}

/// Lookup never admits work. A not-found result proves non-admission only while
/// the runtime's retained-operation horizon has not elapsed since submission.
pub(super) fn lookup(
    command: &Command,
    first_sent: Instant,
    send: &mut impl FnMut(Command) -> Transport,
) -> Lookup {
    let Command::Request {
        service,
        payload,
        operation_id,
        parent,
        ..
    } = command
    else {
        return Lookup::Unknown;
    };
    let result = send(Command::LookupRequest {
        service: service.clone(),
        payload: payload.clone(),
        operation_id: operation_id.clone(),
        parent: *parent,
    });
    match result {
        Ok(Response::Accepted { id }) => Lookup::Found(id),
        Ok(Response::Error { code, .. })
            if code == "not_found" && first_sent.elapsed() < Duration::from_secs(300) =>
        {
            Lookup::NotAdmitted
        }
        _ => Lookup::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save_request() -> Request {
        Request {
            id: 42,
            caller: "console".into(),
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }

    fn save_stage() -> Stage {
        Stage::Saving {
            response: json!({"ok":true}),
            snapshot: None,
            expected_ack_revision: 1,
        }
    }

    #[test]
    fn uncertain_save_admission_is_retried_exactly_and_fenced_during_shutdown() {
        let mut pipeline = Pipeline::new(Settings::default());
        let mut submitted = None;
        pipeline
            .submit(
                save_request(),
                save_stage(),
                json!({"action":"set"}),
                &mut |command| {
                    assert!(matches!(
                        command,
                        Command::Request {
                            parent: Some(42),
                            ..
                        }
                    ));
                    submitted = Some(command);
                    Err("lost admission response".into())
                },
            )
            .unwrap();
        pipeline.prepare_shutdown();
        let pending = pipeline.pending.take().unwrap();
        pipeline
            .advance(pending, 0, &mut |_| {
                panic!("must not replay after shutdown announcement")
            })
            .unwrap();
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Err("lookup unavailable".into())
        }));
        // The following retry simulates an ordinary tick before shutdown.
        pipeline.shutdown_announced = false;
        let original = submitted.unwrap();
        let pending = pipeline.pending.take().unwrap();
        pipeline
            .advance(pending, 0, &mut |command| {
                if matches!(command, Command::CheckRequest { request: 42 }) {
                    return Ok(Response::Ok);
                }
                assert_eq!(
                    serde_json::to_value(command).unwrap(),
                    serde_json::to_value(&original).unwrap()
                );
                Ok(Response::Accepted { id: 19 })
            })
            .unwrap();
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::TakeResult { request: 19 }));
            Ok(Response::Pending)
        }));
        assert!(!pipeline.quiesce_broker(|command| match command {
            Command::TakeResult { request: 19 } => Ok(Response::Result {
                payload: json!({"ok":true,"revision":1}),
            }),
            Command::ReleaseResult { request: 19 } => Err("lost release acknowledgement".into()),
            _ => panic!("unexpected command"),
        }));
        assert!(pipeline.pending.as_ref().unwrap().result.is_some());
        assert!(pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::ReleaseResult { request: 19 }));
            Ok(Response::Ok)
        }));
        assert!(pipeline.pending.is_none());
    }

    #[test]
    fn accepted_write_survives_result_transport_failure_and_expiry() {
        let mut pipeline = Pipeline::new(Settings::default());
        pipeline
            .submit(
                save_request(),
                save_stage(),
                json!({"action":"set"}),
                &mut |_| Ok(Response::Accepted { id: 19 }),
            )
            .unwrap();
        let pending = pipeline.pending.take().unwrap();
        pipeline
            .advance(pending, 0, &mut |_| Err("lost result".into()))
            .unwrap();
        assert!(!pipeline.quiesce_broker(|_| Ok(Response::error("deadline_exceeded", "expired"))));
        assert!(pipeline.pending.is_some());
    }

    #[test]
    fn cancelled_caller_never_replays_an_unadmitted_write() {
        let mut pipeline = Pipeline::new(Settings::default());
        pipeline
            .submit(
                save_request(),
                save_stage(),
                json!({"action":"set"}),
                &mut |_| Err("lost admission".into()),
            )
            .unwrap();
        let pending = pipeline.pending.take().unwrap();
        pipeline
            .advance(pending, 0, &mut |command| match command {
                Command::CheckRequest { request: 42 } => {
                    Ok(Response::error("request_ended", "cancelled"))
                }
                Command::LookupRequest { .. } => Ok(Response::error("not_found", "not admitted")),
                Command::Complete { request: 42, .. } => {
                    Ok(Response::error("request_ended", "cancelled"))
                }
                _ => panic!("must not replay a mutation after cancellation"),
            })
            .unwrap();
        assert!(pipeline.pending.is_none());
    }

    #[test]
    fn shutdown_announcement_does_not_advance_background_identity_migration() {
        let mut pipeline = Pipeline::new(Settings::default());
        pipeline.host_configuration = HostConfigurationState::WaitingPairingState {
            request: 74,
            snapshot: PairingStateSnapshot {
                paths: EffectiveIdentityPaths {
                    provider_revision: 1,
                    certificate_path: "cert".into(),
                    private_key_path: "key".into(),
                },
                revision: None,
                cursor: None,
                values: Map::new(),
            },
        };
        pipeline.prepare_shutdown();
        assert!(
            !pipeline
                .advance_host_configuration(0, &mut |_| panic!("must not start migration work"))
                .unwrap()
        );
        assert!(matches!(
            pipeline.host_configuration,
            HostConfigurationState::WaitingPairingState { request: 74, .. }
        ));
    }

    #[test]
    fn uncertain_marker_delete_is_neither_reissued_nor_retired_on_shutdown() {
        let mut pipeline = Pipeline::new(Settings::default());
        pipeline.host_configuration = HostConfigurationState::SubmittingMigrationMarkerDelete {
            command: Command::Request {
                service: STATE_SERVICE.into(),
                payload: json!({"action":"delete"}),
                timeout_ms: 30_000,
                operation_id: "pairing.marker".into(),
                parent: None,
            },
            paths: EffectiveIdentityPaths {
                provider_revision: 0,
                certificate_path: "cert".into(),
                private_key_path: "key".into(),
            },
            expected_revision: 1,
            first_sent: Instant::now(),
            admission_uncertain: true,
        };
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Err("lookup unavailable".into())
        }));
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::Accepted { id: 75 })
        }));
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::TakeResult { request: 75 }));
            Err("lost result".into())
        }));
        assert!(!pipeline.quiesce_broker(|command| match command {
            Command::TakeResult { request: 75 } => Ok(Response::Result {
                payload: json!({"ok":true,"deleted":true,"revision":2}),
            }),
            Command::ReleaseResult { request: 75 } => Err("lost release acknowledgement".into()),
            _ => panic!("unexpected command"),
        }));
        assert!(matches!(
            pipeline.host_configuration,
            HostConfigurationState::WaitingMigrationMarkerDelete { request: 75, .. }
        ));
        assert!(pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::ReleaseResult { request: 75 }));
            Ok(Response::Ok)
        }));
    }

    #[test]
    fn lookup_settles_lost_admission_without_reissuing_a_write() {
        let mut pipeline = Pipeline::new(Settings::default());
        pipeline
            .submit(
                save_request(),
                save_stage(),
                json!({"action":"set"}),
                &mut |_| Err("lost admission".into()),
            )
            .unwrap();
        assert!(!pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::Accepted { id: 19 })
        }));
        assert!(!pipeline.quiesce_broker(|command| match command {
            Command::TakeResult { request: 19 } => Ok(Response::Result {
                payload: json!({"ok":true,"revision":1}),
            }),
            Command::ReleaseResult { request: 19 } => Err("lost release acknowledgement".into()),
            _ => panic!("unexpected command"),
        }));
        assert!(pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::ReleaseResult { request: 19 }));
            Ok(Response::Ok)
        }));

        pipeline
            .submit(
                save_request(),
                save_stage(),
                json!({"action":"set"}),
                &mut |_| Err("lost admission".into()),
            )
            .unwrap();
        assert!(pipeline.quiesce_broker(|command| {
            assert!(matches!(command, Command::LookupRequest { .. }));
            Ok(Response::error("not_found", "not admitted"))
        }));
    }
}
