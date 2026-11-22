use super::{Pipeline, rpc};
use crate::providers::ProviderServices;
use amaterasu_module_sdk::{
    Command, Response,
    transport::udp_preflight::{
        Endpoints, ProbeRequest, checked_result, result_payload, validate_endpoints,
    },
};

const PROBE_TIMEOUT_MS: u64 = 3_000;

pub(super) struct UdpPreflight {
    request: u64,
    deadline: u64,
    pending: rpc::Pending,
    control: Option<rpc::Pending>,
    cancelling: bool,
}

impl UdpPreflight {
    fn new(
        providers: &ProviderServices,
        request: u64,
        deadline: u64,
        endpoints: &[Endpoints],
    ) -> Self {
        let media = endpoints
            .iter()
            .flat_map(|set| [set.video, set.audio])
            .collect();
        let control = endpoints.iter().map(|set| set.control).collect();
        Self {
            request,
            deadline,
            pending: rpc::Pending::new(
                &providers.media_transport,
                ProbeRequest::for_transport(media),
                format!("stream-network-{request}-media"),
                Some(request),
            ),
            control: Some(rpc::Pending::new(
                &providers.control_transport,
                ProbeRequest::for_transport(control),
                format!("stream-network-{request}-control"),
                Some(request),
            )),
            cancelling: false,
        }
    }

    pub(super) fn child_request(&self) -> Option<u64> {
        self.pending.request_id()
    }

    fn advance(
        &mut self,
        now: u64,
        closing: bool,
        send: &mut rpc::Send<'_>,
    ) -> Option<Result<(), &'static str>> {
        let abandoned = !matches!(
            send(Command::CheckRequest {
                request: self.request
            }),
            Ok(Response::Ok)
        );
        if closing || abandoned || now >= self.deadline {
            if !self.cancelling {
                self.pending.cancel();
                self.cancelling = true;
            }
            if self.pending.request_id().is_none() {
                return Some(Err("udp_probe_timeout"));
            }
        }

        match self.pending.poll(send, now) {
            rpc::Progress::Waiting => None,
            rpc::Progress::Failed | rpc::Progress::Unresolved => Some(Err(if self.cancelling {
                "udp_probe_timeout"
            } else {
                "udp_probe_unavailable"
            })),
            rpc::Progress::Result(payload) => {
                if !self.pending.release_result(send) {
                    return (now >= self.deadline).then_some(Err("udp_probe_timeout"));
                }
                if self.cancelling {
                    return Some(Err("udp_probe_timeout"));
                }
                if let Err(code) = checked_result(&payload) {
                    return Some(Err(code));
                }
                if let Some(control) = self.control.take() {
                    self.pending = control;
                    None
                } else {
                    Some(Ok(()))
                }
            }
        }
    }
}

impl Pipeline {
    pub(super) fn begin_udp_preflight(
        &mut self,
        input: super::actions::Incoming,
        endpoints: Vec<Endpoints>,
        send: &mut rpc::Send<'_>,
    ) {
        let result = (|| {
            self.authorize_open(&input.owner)?;
            if self.shutdown_admission_closed()
                || self.package_barrier_pending
                || self.udp_preflight.is_some()
            {
                return Err("udp_probe_unavailable");
            }
            validate_endpoints(&endpoints, self.settings.allow_remote)?;
            self.udp_preflight = Some(UdpPreflight::new(
                &self.settings.providers,
                input.request,
                input
                    .now
                    .saturating_add(input.remaining.min(PROBE_TIMEOUT_MS)),
                &endpoints,
            ));
            Ok(())
        })();
        if result.is_err() {
            rpc::complete(send, input.request, result_payload(result));
        }
    }

    pub(super) fn progress_udp_preflight(&mut self, now: u64, send: &mut rpc::Send<'_>) {
        let Some(mut probe) = self.udp_preflight.take() else {
            return;
        };
        if let Some(result) = probe.advance(now, self.shutdown_admission_closed(), send) {
            rpc::complete(send, probe.request, result_payload(result));
        } else {
            self.udp_preflight = Some(probe);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Settings;
    use serde_json::json;

    #[test]
    fn preflight_requests_the_same_replacement_media_and_control_owners_as_stream_open() {
        let mut pipeline = Pipeline::new(
            Settings {
                enabled: true,
                allowed_principals: vec!["amaterasu.rtsp".into()],
                providers: ProviderServices {
                    media_transport: "example.media/1".into(),
                    control_transport: "example.control/1".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            "amaterasu.stream-session/1".into(),
        )
        .unwrap();
        let addresses = [
            ("127.0.0.1:35500", "127.0.0.1:35501", "127.0.0.1:35502"),
            ("[::1]:35500", "[::1]:35501", "[::1]:35502"),
        ];
        let endpoints = addresses.map(|(video, control, audio)| Endpoints {
            video: video.parse().unwrap(),
            audio: audio.parse().unwrap(),
            control: control.parse().unwrap(),
        });
        let mut requests = Vec::new();
        let mut reply = None;
        let mut send = |command| match command {
            Command::CheckRequest { request: 15 } => Ok(Response::Ok),
            Command::Request {
                service,
                payload,
                parent: Some(15),
                ..
            } => {
                requests.push((service, payload));
                Ok(Response::Accepted {
                    id: 15 + requests.len() as u64,
                })
            }
            Command::TakeResult { request: 16 } => Ok(Response::Result {
                payload: result_payload(Ok(())),
            }),
            Command::TakeResult { request: 17 } => Ok(Response::Result {
                payload: result_payload(Err("udp_bind_failed")),
            }),
            Command::ReleaseResult { request: 16 | 17 } => Ok(Response::Ok),
            Command::Complete {
                request: 15,
                payload,
            } => {
                reply = Some(payload);
                Ok(Response::Ok)
            }
            _ => panic!("unexpected network probe broker command: {command:?}"),
        };
        pipeline.begin_udp_preflight(
            super::super::actions::Incoming {
                request: 15,
                owner: "amaterasu.rtsp".into(),
                approved_api: false,
                now: 0,
                remaining: 4_000,
            },
            endpoints.to_vec(),
            &mut send,
        );
        for now in 1..=4 {
            pipeline.progress_udp_preflight(now, &mut send);
        }
        assert!(pipeline.udp_preflight.is_none());
        assert_eq!(reply, Some(result_payload(Err("udp_bind_failed"))));
        assert_eq!(
            requests,
            [
                (
                    "example.media/1".into(),
                    json!({"action":"probe_udp_binds",
                    "addresses":["127.0.0.1:35500","127.0.0.1:35502","[::1]:35500","[::1]:35502"]})
                ),
                (
                    "example.control/1".into(),
                    json!({"action":"probe_udp_binds",
                    "addresses":["127.0.0.1:35501","[::1]:35501"]})
                ),
            ],
        );
    }
}
