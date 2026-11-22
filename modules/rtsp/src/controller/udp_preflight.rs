use super::{
    Controller,
    rpc::{Call, Outcome},
};
use amaterasu_module_sdk::{
    RuntimeClient,
    transport::udp_preflight::{ProbeRequest, checked_result},
};

const UDP_PROBE_TIMEOUT_MS: u64 = 4_000;

pub(super) struct NetworkProbe {
    call: Call,
    deadline: u64,
    cancelled: bool,
}

impl NetworkProbe {
    pub(super) fn cancel(&mut self) {
        if !self.cancelled {
            self.call.cancel_read_only();
            self.cancelled = true;
        }
    }

    pub(super) fn into_cancelled_call(mut self) -> Call {
        self.cancel();
        self.call
    }
}

impl Controller {
    pub(super) fn reconcile_network_port_with_owner(&mut self, runtime: &RuntimeClient, now: u64) {
        let mut probe = self.network_probe.take();
        if let Some(pending) = probe.as_mut() {
            let timed_out = now >= pending.deadline && !pending.cancelled;
            if timed_out || !self.network_update_idle() {
                pending.cancel();
            }
            if pending.cancelled {
                if matches!(pending.call.poll(runtime), Outcome::Waiting) {
                    self.network_probe = probe;
                }
                if timed_out {
                    self.finish_network_reconciliation(now, Err("udp_probe_timeout"));
                }
                return;
            }
        }
        if self.network_base_port.is_none() || now < self.network_retry_at {
            return;
        }

        let result = if let Some(mut pending) = probe {
            match pending.call.poll(runtime) {
                Outcome::Waiting => {
                    self.network_probe = Some(pending);
                    Ok(false)
                }
                Outcome::Result(payload) => checked_result(&payload)
                    .and_then(|()| self.try_apply_network_port(|_| Ok(true))),
                Outcome::SettledFailure | Outcome::Uncertain => Err("udp_probe_unavailable"),
            }
        } else {
            let mut endpoints = None;
            let result = self.try_apply_network_port(|selected| {
                endpoints = Some(selected);
                Ok(false)
            });
            if let Some(endpoints) = endpoints {
                let Some(next) = self.network_probe_sequence.checked_add(1) else {
                    self.finish_network_reconciliation(now, Err("udp_probe_unavailable"));
                    return;
                };
                self.network_probe_sequence = next;
                let mut call = Call::for_service(
                    &self.settings.stream_service,
                    ProbeRequest::for_stream_owner(endpoints),
                    format!("rtsp-network-{}-{next}", self.runtime_epoch),
                    None,
                )
                .with_timeout(UDP_PROBE_TIMEOUT_MS);
                match call.poll(runtime) {
                    Outcome::Waiting => {
                        self.network_probe = Some(NetworkProbe {
                            call,
                            deadline: now.saturating_add(UDP_PROBE_TIMEOUT_MS),
                            cancelled: false,
                        });
                    }
                    _ => {
                        self.finish_network_reconciliation(now, Err("udp_probe_unavailable"));
                        return;
                    }
                }
            }
            result
        };
        self.finish_network_reconciliation(now, result);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Endpoint, NETWORK_TEST_LOCK, client, free_base_port};
    use super::super::*;
    use amaterasu_module_sdk::transport::udp_preflight::{Endpoints, ProbeRequest, result_payload};

    #[test]
    fn selected_owner_rejection_in_the_third_slot_preserves_the_listener_and_late_results() {
        let _guard = NETWORK_TEST_LOCK.lock().unwrap();
        let mut controller = Controller::new(Settings {
            enabled: true,
            bind: "127.0.0.1:0".parse().unwrap(),
            stream_service: "replacement.stream-owner/1".into(),
            max_live_tickets: 3,
            ..Default::default()
        })
        .unwrap();
        let old_listener = controller.local.unwrap();
        let first_port = free_base_port();
        let second_port = free_base_port();
        let final_port = free_base_port();
        let mut endpoint = Endpoint {
            verifier_pending: true,
            ..Default::default()
        };
        let runtime = client(&mut endpoint);
        controller
            .set_network_policy(first_port, None, None)
            .unwrap();
        controller.reconcile_network_port_with_owner(&runtime, 0);
        let command = endpoint.commands.last().unwrap();
        let Command::Request {
            service, payload, ..
        } = command
        else {
            panic!("expected a request to the selected stream owner");
        };
        assert_eq!(service, "replacement.stream-owner/1");
        let ProbeRequest::ProbeUdpEndpoints { endpoints } =
            serde_json::from_value(payload.clone()).unwrap()
        else {
            panic!("expected a transport probe");
        };
        let expected_endpoint = |offset| SocketAddr::from(([127, 0, 0, 1], first_port + offset));
        assert_eq!(
            endpoints,
            (0..3)
                .map(|slot| Endpoints {
                    video: expected_endpoint(9 + slot * 3),
                    control: expected_endpoint(10 + slot * 3),
                    audio: expected_endpoint(11 + slot * 3),
                })
                .collect::<Vec<_>>()
        );
        // The replacement transport rejects an address even though RTSP's
        // network provider can bind it. Activation must honor the owner.
        let disputed_address = endpoints[2].control;
        assert!(controller.network.bind_udp(disputed_address).is_ok());
        endpoint.verifier_pending = false;
        endpoint.verifier_reply = Some(result_payload(Err("udp_bind_failed")));
        controller.reconcile_network_port_with_owner(&runtime, 1);
        assert_eq!(controller.local, Some(old_listener));
        assert_eq!(controller.network_error, Some("udp_bind_failed"));

        // A result for a superseded target must be cancelled, even if a success
        // is already available when RTSP gets the next desired policy.
        controller
            .set_network_policy(second_port, None, None)
            .unwrap();
        controller.reconcile_network_port_with_owner(&runtime, 2);
        controller
            .set_network_policy(final_port, None, None)
            .unwrap();
        endpoint.verifier_reply = Some(result_payload(Ok(())));
        controller.reconcile_network_port_with_owner(&runtime, 3);
        assert_eq!(controller.local, Some(old_listener));
        assert!(
            endpoint
                .commands
                .iter()
                .any(|command| matches!(command, Command::Cancel { request: 71 }))
        );
        controller.reconcile_network_port_with_owner(&runtime, 4);
        controller.reconcile_network_port_with_owner(&runtime, 5);
        assert_eq!(controller.local.unwrap().port(), final_port + 21);
        assert_eq!(controller.active_network_base_port, Some(final_port));
        assert_eq!(controller.network_error, None);
    }

    #[test]
    fn network_probe_cancellation_is_retained_until_quiescence_settles() {
        let _guard = NETWORK_TEST_LOCK.lock().unwrap();
        let mut controller = Controller::new(Settings {
            enabled: true,
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        })
        .unwrap();
        let mut endpoint = Endpoint {
            verifier_pending: true,
            deny_settlement: true,
            ..Default::default()
        };
        let runtime = client(&mut endpoint);
        controller
            .set_network_policy(free_base_port(), None, None)
            .unwrap();
        controller.reconcile_network_port_with_owner(&runtime, 0);
        assert!(!controller.quiesce(&runtime, 1));
        assert!(controller.network_probe.is_none());
        assert_eq!(controller.shutdown_settlement.len(), 1);
        endpoint.deny_settlement = false;
        endpoint.verifier_reply = Some(result_payload(Ok(())));
        controller.tick(&runtime, 2).unwrap();
        assert!(controller.shutdown_settlement.is_empty());
        assert_eq!(controller.active_network_base_port, None);
    }
}
