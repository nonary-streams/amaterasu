//! Read library artwork in bounded, revision-pinned chunks.
use super::{Job, Permission, Purpose, Reply, RequestContext, Router, query::Query, xml};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const ARTWORK_TIMEOUT: Duration = Duration::from_secs(120);
// Four images can progress concurrently, with at most 64 MiB of image bodies.
pub(super) const MAX_ACTIVE_ARTWORK: usize = 4;
const SERVICE: &str = "amaterasu.library/1";

pub(super) struct Artwork;
impl Artwork {
    pub fn new(_id: String, _transfer_id: String) -> Self {
        Self
    }
    pub fn service(&self) -> &'static str {
        SERVICE
    }
}

impl Router {
    pub(super) fn appasset(
        &mut self,
        connection: u64,
        peer: &RequestContext,
        query: &Query,
        now: Instant,
    ) -> Option<Reply> {
        if !peer.trusted() {
            return Some(xml::error(401, "authenticated paired TLS is required"));
        }
        if !peer.list_allowed {
            return Some(xml::error(403, "application list permission denied"));
        }
        let app_id = match query
            .required("appid")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|id| *id > 0 && *id <= i32::MAX as u32)
        {
            Some(id) => id,
            None => return Some(xml::error(400, "invalid application ID")),
        };
        let id = format!("app-{app_id:016x}");
        let mut random = [0u8; 16];
        if getrandom::fill(&mut random).is_err() {
            return Some(xml::error(503, "artwork transfer unavailable"));
        }
        let transfer_id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let queued = self.queue(
            Some(connection),
            Purpose::Artwork {
                id: id.clone(),
                transfer_id: transfer_id.clone(),
                certificate: peer.peer_certificate.clone().expect("trusted certificate"),
            },
            json!({"action":"artwork_open", "id":id}),
            now + ARTWORK_TIMEOUT,
            now,
        );
        match queued {
            Ok(()) => None,
            Err(message) => Some(xml::error(503, message)),
        }
    }
    pub(super) fn resolve_artwork(
        &mut self,
        job: Job,
        result: Result<Value, String>,
        _now: Instant,
        replies: &mut Vec<(u64, Reply)>,
        receive: &mut impl FnMut(Value) -> Result<amaterasu_module_sdk::artwork_transfer::Image, String>,
    ) -> Option<Job> {
        let image = result.and_then(receive);
        if let Some(connection) = job.connection {
            match image {
                Ok(image) => {
                    let mime = match image.mime_type.as_str() {
                        "image/png" => "image/png",
                        "image/jpeg" => "image/jpeg",
                        "image/webp" => "image/webp",
                        _ => {
                            replies.push((connection, xml::error(404, "artwork unavailable")));
                            self.artwork_reservations.remove(&connection);
                            return None;
                        }
                    };
                    replies.push((
                        connection,
                        Reply {
                            status: 200,
                            content_type: mime,
                            body: image.bytes,
                            requires_paired_tls: true,
                            permission: Some(Permission::List),
                        },
                    ));
                }
                Err(_) => {
                    self.artwork_reservations.remove(&connection);
                    replies.push((connection, xml::error(404, "artwork unavailable")));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Controller;
    use crate::wire::{Request, RouterConfig};
    use amaterasu_module_sdk::{Command, Response};
    fn router() -> Router {
        Router::new(
            RouterConfig {
                hostname: "test".into(),
                http_port: 1,
                https_port: 2,
            },
            Controller::new(None).unwrap(),
        )
    }
    fn peer() -> RequestContext {
        RequestContext {
            secure: true,
            remote: "127.0.0.1".parse().unwrap(),
            peer_certificate: Some(vec![1]),
            peer_trusted: true,
            list_allowed: true,
            launch_allowed: false,
            view_allowed: true,
            terminate_allowed: false,
        }
    }
    #[test]
    fn moonlight_reads_one_native_record_and_retains_delivery_authorization() {
        let mut router = router();
        let now = Instant::now();
        assert!(
            router
                .dispatch_with(
                    1,
                    Request {
                        method: "GET".into(),
                        target: "/appasset?appid=1".into(),
                        body: vec![]
                    },
                    peer(),
                    now,
                    &mut |_| panic!()
                )
                .is_none()
        );
        let mut calls = 0;
        let mut send = |command| match command {
            Command::Request {
                service, payload, ..
            } => {
                calls += 1;
                assert_eq!(service, SERVICE);
                assert_eq!(
                    payload,
                    json!({"action":"artwork_open","id":"app-0000000000000001"})
                );
                Ok(Response::Accepted { id: 9 })
            }
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok":true,"offer":{}}),
            }),
            Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => panic!(),
        };
        let mut receive = |_| {
            Ok(amaterasu_module_sdk::artwork_transfer::Image {
                revision: "r1".into(),
                mime_type: "image/png".into(),
                bytes: vec![0xa5; 1024 * 1024 + 3],
            })
        };
        assert!(
            router
                .poll_artwork_with_identity(now, &mut send, |_, _| true, &mut receive)
                .is_empty()
        );
        let replies = router.poll_artwork_with_identity(now, &mut send, |_, _| true, &mut receive);
        assert_eq!(calls, 1);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1.status, 200);
        assert_eq!(replies[0].1.body.len(), 1024 * 1024 + 3);
        assert!(replies[0].1.requires_paired_tls);
        assert!(matches!(replies[0].1.permission, Some(Permission::List)));
        assert!(router.artwork_reservations.contains(&1));
        assert!(router.jobs.is_empty());
    }
    #[test]
    fn unpaired_or_list_denied_clients_never_open_artwork() {
        for denied in [
            RequestContext {
                peer_trusted: false,
                ..peer()
            },
            RequestContext {
                list_allowed: false,
                ..peer()
            },
        ] {
            let mut router = router();
            let response = router
                .dispatch_with(
                    1,
                    Request {
                        method: "GET".into(),
                        target: "/appasset?appid=1".into(),
                        body: vec![],
                    },
                    denied,
                    Instant::now(),
                    &mut |_| panic!(),
                )
                .unwrap();
            assert_ne!(response.status, 200);
            assert!(router.jobs.is_empty());
        }
    }
}
