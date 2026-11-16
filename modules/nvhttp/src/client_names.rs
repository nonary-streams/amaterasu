//! Bounded hostname discovery for the legacy placeholder sent by Moonlight.
use amaterasu_module_sdk::network::Network;
use serde_json::Value;
use std::{
    net::IpAddr,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const LOOKUP_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_LOOKUPS: usize = 8;

pub struct ClientNames {
    network: Option<Network>,
    lookups: Vec<Lookup>,
}
pub struct ResolvedName {
    pub client: String,
    pub connection: Option<u64>,
    pub deadline: Instant,
    pub payload: Value,
}
struct Lookup {
    request: ResolvedName,
    worker: Option<JoinHandle<Option<String>>>,
}
impl Drop for Lookup {
    fn drop(&mut self) {
        // The provider bounds each lookup. Retain its binding until the worker
        // joins so no consumer/provider code can outlive native-module unload.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl ClientNames {
    pub fn new(network: Option<Network>) -> Self {
        Self {
            network,
            lookups: Vec::new(),
        }
    }
    pub fn start(
        &mut self,
        client: &str,
        connection: u64,
        remote: IpAddr,
        payload: &Value,
        deadline: Instant,
    ) -> bool {
        let Some(network) = self.network.clone() else {
            return false;
        };
        if self.lookups.len() >= MAX_LOOKUPS {
            return false;
        }
        let Ok(worker) = thread::Builder::new()
            .name("pairing-hostname".into())
            .spawn(move || {
                network
                    .reverse_resolve_timeout(remote, LOOKUP_TIMEOUT)
                    .ok()
                    .filter(|name| valid_name(name))
            })
        else {
            return false;
        };
        self.lookups.push(Lookup {
            request: ResolvedName {
                client: client.into(),
                connection: Some(connection),
                deadline,
                payload: payload.clone(),
            },
            worker: Some(worker),
        });
        true
    }
    pub fn completed(&mut self) -> Vec<ResolvedName> {
        let mut ready = Vec::new();
        let mut index = 0;
        while index < self.lookups.len() {
            if !self.lookups[index]
                .worker
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                index += 1;
                continue;
            }
            let mut lookup = self.lookups.swap_remove(index);
            if let Some(name) = lookup
                .worker
                .take()
                .and_then(|worker| worker.join().ok())
                .flatten()
            {
                lookup.request.payload["name"] = name.into();
            }
            // Lookup owns a join guard; moving the request out requires replacing it.
            ready.push(ResolvedName {
                client: std::mem::take(&mut lookup.request.client),
                connection: lookup.request.connection,
                deadline: lookup.request.deadline,
                payload: std::mem::take(&mut lookup.request.payload),
            });
        }
        ready
    }
    pub fn cancel(&mut self, connection: u64) -> Vec<String> {
        self.lookups
            .iter_mut()
            .filter_map(|lookup| {
                if lookup.request.connection == Some(connection) {
                    lookup.request.connection = None;
                    Some(lookup.request.client.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn quiesce(&mut self) -> bool {
        self.completed();
        self.lookups.is_empty()
    }
}
pub fn legacy_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("roth") || name.eq_ignore_ascii_case("Moonlight")
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;

    fn pending(worker: JoinHandle<Option<String>>) -> Lookup {
        Lookup {
            request: ResolvedName {
                client: "client".into(),
                connection: Some(7),
                deadline: Instant::now() + Duration::from_secs(1),
                payload: json!({"name":"127.0.0.1"}),
            },
            worker: Some(worker),
        }
    }
    fn wait(names: &mut ClientNames) -> ResolvedName {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = names.completed().pop() {
                return result;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn successful_lookup_replaces_the_address_and_failure_keeps_it() {
        for (result, expected) in [(Some("steamdeck".into()), "steamdeck"), (None, "127.0.0.1")] {
            let mut names = ClientNames::new(None);
            names.lookups.push(pending(thread::spawn(move || result)));
            let result = wait(&mut names);
            assert_eq!(result.payload["name"], expected);
            assert_eq!(result.connection, Some(7));
            assert!(names.quiesce());
        }
    }
    #[test]
    fn cancellation_and_quiescence_retain_workers_until_they_retire() {
        let (release, held) = mpsc::channel();
        let mut names = ClientNames::new(None);
        names.lookups.push(pending(thread::spawn(move || {
            held.recv().unwrap();
            Some("steamdeck".into())
        })));
        assert_eq!(names.cancel(7), ["client"]);
        assert!(!names.quiesce());
        release.send(()).unwrap();
        assert_eq!(wait(&mut names).connection, None);
        assert!(names.quiesce());
    }
    #[test]
    fn discovered_names_are_bounded_and_legacy_names_are_identified() {
        assert!(legacy_name("roth"));
        assert!(legacy_name("ROTH"));
        assert!(legacy_name("Moonlight"));
        assert!(!legacy_name("steamdeck"));
        assert!(valid_name("steamdeck.home.local"));
        assert!(valid_name(&"é".repeat(64)));
        for invalid in ["", "  ", "host\nname", &"é".repeat(65)] {
            assert!(!valid_name(invalid));
        }
    }
}
