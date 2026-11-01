//! Read library artwork in bounded, revision-pinned chunks.
use super::{Call, Job, Permission, Purpose, Reply, RequestContext, Router, query::Query, xml};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const CHUNK_BYTES: usize = 8 * 1024;
const ARTWORK_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ACTIVE_ARTWORK: usize = 2;
const SERVICE: &str = "amaterasu.library/1";

pub(super) struct Artwork {
    id: String,
    transfer_id: String,
    revision: Option<String>,
    total: Option<usize>,
    mime: Option<&'static str>,
    bytes: Vec<u8>,
}

pub(super) enum Progress {
    Read { payload: Value },
    Complete { mime: &'static str, bytes: Vec<u8> },
}

impl Artwork {
    pub fn new(id: String, transfer_id: String) -> Self {
        Self {
            id,
            transfer_id,
            revision: None,
            total: None,
            mime: None,
            bytes: Vec::new(),
        }
    }
    pub fn service(&self) -> &'static str {
        SERVICE
    }
    fn accept(&mut self, payload: Value) -> Result<Progress, ()> {
        if payload["ok"] != true {
            return Err(());
        }
        let offset = self.bytes.len();
        let revision = payload["revision"]
            .as_str()
            .filter(|revision| {
                !revision.is_empty()
                    && revision.len() <= 64
                    && revision
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or(())?;
        if self
            .revision
            .as_deref()
            .is_some_and(|previous| previous != revision)
            || payload["offset"].as_u64() != Some(offset as u64)
        {
            return Err(());
        }
        let total = payload["total_bytes"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0 && *n <= MAX_BYTES)
            .ok_or(())?;
        let mime = match payload["mime_type"].as_str() {
            Some("image/png") => "image/png",
            Some("image/jpeg") => "image/jpeg",
            Some("image/webp") => "image/webp",
            _ => return Err(()),
        };
        if self.total.is_some_and(|previous| previous != total)
            || self.mime.is_some_and(|previous| previous != mime)
            || offset >= total
        {
            return Err(());
        }
        let expected = (total - offset).min(CHUNK_BYTES);
        let encoded = payload["data"].as_str().ok_or(())?;
        if encoded.len() != expected.div_ceil(3) * 4 {
            return Err(());
        }
        let chunk = STANDARD.decode(encoded).map_err(|_| ())?;
        if chunk.len() != expected {
            return Err(());
        }
        let next = offset + expected;
        if (next < total && payload["next_offset"].as_u64() != Some(next as u64))
            || (next == total && !payload.get("next_offset").is_some_and(Value::is_null))
        {
            return Err(());
        }
        self.revision = Some(revision.to_owned());
        self.total = Some(total);
        self.mime = Some(mime);
        self.bytes.extend_from_slice(&chunk);
        if next < total {
            return Ok(Progress::Read {
                payload: json!({"action":"artwork_read","id":self.id,"transfer_id":self.transfer_id,"revision":revision,"offset":next}),
            });
        }
        Ok(Progress::Complete {
            mime,
            bytes: std::mem::take(&mut self.bytes),
        })
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
        if self
            .launches
            .selected_applications_for(
                peer.peer_certificate
                    .as_deref()
                    .expect("trusted certificate"),
            )
            .is_some()
        {
            return Some(xml::error(404, "application artwork is unavailable"));
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
        if self.artwork_reservations.len() >= MAX_ACTIVE_ARTWORK {
            return Some(xml::error(503, "artwork request capacity exhausted"));
        }
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
            json!({"action":"artwork_read", "id":id,"transfer_id":transfer_id,"offset":0}),
            now + ARTWORK_TIMEOUT,
            now,
        );
        match queued {
            Ok(()) => {
                self.artwork_reservations.insert(connection);
                None
            }
            Err(message) => Some(xml::error(503, message)),
        }
    }
    pub(super) fn resolve_artwork(
        &mut self,
        mut job: Job,
        result: Result<Value, String>,
        now: Instant,
        replies: &mut Vec<(u64, Reply)>,
    ) -> Option<Job> {
        let progress = result
            .ok()
            .and_then(|payload| job.artwork.as_mut()?.accept(payload).ok());
        match progress {
            Some(Progress::Read { payload }) => {
                job.call = Call::Ready {
                    payload,
                    not_before: now,
                };
                Some(job)
            }
            Some(Progress::Complete { mime, bytes }) => {
                if let Some(connection) = job.connection {
                    replies.push((
                        connection,
                        Reply {
                            status: 200,
                            content_type: mime,
                            body: bytes,
                            requires_paired_tls: true,
                            permission: Some(Permission::List),
                        },
                    ));
                }
                Some(release_job(job, now))
            }
            None => {
                if let Some(connection) = job.connection {
                    self.artwork_reservations.remove(&connection);
                    replies.push((connection, xml::error(404, "artwork unavailable")));
                }
                Some(release_job(job, now))
            }
        }
    }

    pub(super) fn resolve_artwork_release(
        &mut self,
        mut job: Job,
        result: Result<Value, String>,
        now: Instant,
    ) -> Option<Job> {
        if result.is_ok_and(|payload| payload["ok"] == true) {
            return None;
        }
        if let Purpose::ArtworkRelease { id, transfer_id } = &job.purpose {
            job.call = Call::Ready {
                payload: json!({"action":"artwork_release", "id":id, "transfer_id":transfer_id}),
                not_before: now + Duration::from_millis(250),
            };
            Some(job)
        } else {
            None
        }
    }
}

pub(super) fn release_job(mut job: Job, now: Instant) -> Job {
    if let Purpose::Artwork {
        id, transfer_id, ..
    } = &job.purpose
    {
        let payload = json!({"action":"artwork_release", "id":id, "transfer_id":transfer_id});
        job.purpose = Purpose::ArtworkRelease {
            id: id.clone(),
            transfer_id: transfer_id.clone(),
        };
        job.call = Call::Ready {
            payload,
            not_before: now,
        };
        job.connection = None;
        job.artwork = None;
        job.deadline = now + Duration::from_secs(30);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chunk(bytes: &[u8], offset: usize, total: usize, revision: &str) -> Value {
        let next = offset + bytes.len();
        json!({"ok":true,"revision":revision,"offset":offset,"total_bytes":total,"mime_type":"image/png","next_offset":if next < total { Some(next) } else { None },"data":STANDARD.encode(bytes)})
    }
    #[test]
    fn assembles_complete_image_and_pins_each_chunk() {
        let mut artwork = Artwork::new(
            "app-0000000000000001".into(),
            "0123456789abcdef0123456789abcdef".into(),
        );
        let first = vec![3; CHUNK_BYTES];
        let Progress::Read { payload } = artwork
            .accept(chunk(&first, 0, CHUNK_BYTES + 1, "7"))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(payload["offset"], CHUNK_BYTES);
        assert_eq!(payload["revision"], "7");
        assert_eq!(payload["transfer_id"], "0123456789abcdef0123456789abcdef");
        assert!(
            artwork
                .accept(chunk(&[4], CHUNK_BYTES, CHUNK_BYTES + 1, "8"))
                .is_err()
        );
        let Progress::Complete { mime, bytes } = artwork
            .accept(chunk(&[4], CHUNK_BYTES, CHUNK_BYTES + 1, "7"))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(mime, "image/png");
        assert_eq!(bytes.len(), CHUNK_BYTES + 1);
        assert_eq!(bytes[CHUNK_BYTES], 4);
    }
    #[test]
    fn rejects_malformed_chunks() {
        let original = chunk(&[1, 2, 3], 0, 3, "7");
        for (field, value) in [
            ("revision", json!("")),
            ("offset", json!(1)),
            ("total_bytes", json!(MAX_BYTES + 1)),
            ("mime_type", json!("text/html")),
            ("next_offset", json!(3)),
            ("data", json!("!!!!")),
        ] {
            let mut artwork = Artwork::new(
                "app-0000000000000001".into(),
                "0123456789abcdef0123456789abcdef".into(),
            );
            let mut changed = original.clone();
            changed[field] = value;
            assert!(artwork.accept(changed).is_err(), "accepted {field}");
        }
    }
    #[test]
    fn complete_artwork_is_returned_as_a_list_protected_image() {
        use crate::launch::Controller;
        use crate::wire::{Request, RouterConfig};
        use amaterasu_module_sdk::{Command, Response};
        let mut router = Router::new(
            RouterConfig {
                hostname: "test".into(),
                http_port: 1,
                https_port: 2,
            },
            Controller::new(None).unwrap(),
        );
        let now = Instant::now();
        let peer = RequestContext {
            secure: true,
            remote: "127.0.0.1".parse().unwrap(),
            peer_certificate: Some(vec![1]),
            peer_trusted: true,
            list_allowed: true,
            launch_allowed: false,
        };
        let request = Request {
            method: "GET".into(),
            target: "/appasset?appid=1".into(),
            body: vec![],
        };
        assert!(
            router
                .dispatch_with(1, request, peer, now, &mut |_| panic!())
                .is_none()
        );
        let mut send = |command| match command {
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, SERVICE);
                assert_eq!(payload["offset"], 0);
                Ok(Response::Accepted { id: 1 })
            }
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: chunk(b"abc", 0, 3, "7"),
            }),
            _ => panic!(),
        };
        assert!(
            router
                .poll_with_identity(now, &mut send, |_, _| true)
                .is_empty()
        );
        let replies = router.poll_with_identity(now, &mut send, |_, _| true);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1.body, b"abc");
        assert_eq!(replies[0].1.permission, Some(Permission::List));
        assert!(replies[0].1.requires_paired_tls);
        let release = router.jobs.values().next().expect("release retained");
        let Purpose::ArtworkRelease { transfer_id, .. } = &release.purpose else {
            panic!()
        };
        let token = transfer_id.clone();
        assert_eq!(token.len(), 32);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let mut release_send = |command| match command {
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, SERVICE);
                assert_eq!(payload["action"], "artwork_release");
                assert_eq!(payload["transfer_id"], token);
                Ok(Response::Accepted { id: 2 })
            }
            Command::TakeResult { .. } => Ok(Response::Result {
                payload: json!({"ok": true}),
            }),
            _ => panic!(),
        };
        router.poll_with_identity(now, &mut release_send, |_, _| false);
        router.poll_with_identity(now, &mut release_send, |_, _| false);
        assert!(router.jobs.is_empty());
        // A completed reply still occupies its reservation until the slow reader
        // finishes, even though the asynchronous fetch job has been released.
        let context = || RequestContext {
            secure: true,
            remote: "127.0.0.1".parse().unwrap(),
            peer_certificate: Some(vec![1]),
            peer_trusted: true,
            list_allowed: true,
            launch_allowed: false,
        };
        let request = || Request {
            method: "GET".into(),
            target: "/appasset?appid=1".into(),
            body: vec![],
        };
        assert!(
            router
                .dispatch_with(2, request(), context(), now, &mut |_| panic!())
                .is_none()
        );
        assert_eq!(
            router
                .dispatch_with(3, request(), context(), now, &mut |_| panic!())
                .unwrap()
                .status,
            503
        );
        router.reply_delivered(1);
        assert!(
            router
                .dispatch_with(3, request(), context(), now, &mut |_| panic!())
                .is_none()
        );
        assert_eq!(
            router
                .dispatch_with(4, request(), context(), now, &mut |_| panic!())
                .unwrap()
                .status,
            503
        );
        router.cancel_with(2, now, &mut |_| panic!("undispatched work has no child"));
        assert!(
            router
                .dispatch_with(4, request(), context(), now, &mut |_| panic!())
                .is_none()
        );
    }
    #[test]
    fn route_requires_list_permission_and_rechecks_before_delivery() {
        use crate::launch::Controller;
        use crate::wire::{Request, RouterConfig};
        use amaterasu_module_sdk::{Command, Response};
        let mut router = Router::new(
            RouterConfig {
                hostname: "test".into(),
                http_port: 1,
                https_port: 2,
            },
            Controller::new(None).unwrap(),
        );
        let now = Instant::now();
        let context = |allowed| RequestContext {
            secure: true,
            remote: "127.0.0.1".parse().unwrap(),
            peer_certificate: Some(vec![1]),
            peer_trusted: true,
            list_allowed: allowed,
            launch_allowed: false,
        };
        let request = || Request {
            method: "GET".into(),
            target: "/appasset?appid=1".into(),
            body: vec![],
        };
        assert_eq!(
            router
                .dispatch_with(1, request(), context(false), now, &mut |_| panic!())
                .unwrap()
                .status,
            403
        );
        assert!(
            router
                .dispatch_with(1, request(), context(true), now, &mut |_| panic!())
                .is_none()
        );
        let mut sends = 0;
        let mut send = |command| {
            sends += 1;
            match command {
                Command::Request {
                    service, payload, ..
                } => {
                    assert_eq!(service, SERVICE);
                    assert_eq!(payload["id"], "app-0000000000000001");
                    assert!(payload.get("revision").is_none());
                    Ok(Response::Accepted { id: 1 })
                }
                Command::Cancel { .. } => Ok(Response::Ok),
                Command::TakeResult { .. } => Ok(Response::Error {
                    code: "not_found".into(),
                    message: String::new(),
                }),
                _ => panic!(),
            }
        };
        assert!(
            router
                .poll_with_identity(now, &mut send, |_, permission| permission
                    == Permission::List)
                .is_empty()
        );
        let replies = router.poll_with_identity(now, &mut send, |_, _| false);
        assert_eq!(replies.len(), 1);
        assert_ne!(replies[0].1.status, 200);
        assert!(sends >= 2);
        assert!(
            router
                .jobs
                .values()
                .any(|job| matches!(job.purpose, Purpose::ArtworkRelease { .. }))
        );
    }
}
