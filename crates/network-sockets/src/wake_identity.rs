//! Provider-owned Wake-on-LAN identity snapshot. No discovery runs inside a
//! native callback, and refreshing the snapshot never holds the reader lock.
use amaterasu_network_api::Address;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) type Cache = Mutex<Option<CachedSnapshot>>;
const MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(60);

pub(crate) struct CachedSnapshot {
    snapshot: Snapshot,
    observed_at: Instant,
}

impl CachedSnapshot {
    pub(crate) fn mac_for(&self, local: Address, now: Instant) -> Option<[u8; 6]> {
        if now.saturating_duration_since(self.observed_at) >= MAX_SNAPSHOT_AGE {
            return None;
        }
        self.snapshot.mac_for(local)
    }
}

#[derive(Default)]
pub(crate) struct Snapshot {
    interfaces: Vec<(u32, [u8; 6])>,
    addresses: Vec<amaterasu_network_api::InterfaceAddress>,
}

impl Snapshot {
    pub(crate) fn mac_for(&self, local: Address) -> Option<[u8; 6]> {
        let serving = self.addresses.iter().find_map(|entry| {
            (entry.address.family == local.family
                && entry.address.bytes == local.bytes
                && (local.scope_id == 0 || entry.interface_index == local.scope_id))
                .then(|| {
                    self.interfaces
                        .iter()
                        .find(|(index, _)| *index == entry.interface_index)
                        .map(|(_, mac)| *mac)
                })
                .flatten()
        });
        serving.or_else(|| self.interfaces.first().map(|(_, mac)| *mac))
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::Refresh;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use amaterasu_network_api::MAX_ADDRESSES;
    use std::{
        ffi::OsString,
        fs::{self, File},
        io::{self, Read},
        path::Path,
        sync::{
            Arc,
            mpsc::{self, RecvTimeoutError, SyncSender, TryRecvError},
        },
        thread::{self, JoinHandle},
    };

    const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

    pub(crate) struct Refresh {
        stop: SyncSender<()>,
        worker: Option<JoinHandle<()>>,
    }

    impl Refresh {
        pub(crate) fn start(cache: Arc<Cache>) -> io::Result<Self> {
            let (stop, stopping) = mpsc::sync_channel(1);
            let worker = thread::Builder::new()
                .name("network-wake-identity".into())
                .spawn(move || {
                    while stopping.try_recv() == Err(TryRecvError::Empty) {
                        if let Ok(snapshot) = discover(Path::new("/sys/class/net"))
                            && let Ok(mut cache) = cache.lock()
                        {
                            *cache = Some(CachedSnapshot {
                                snapshot,
                                observed_at: Instant::now(),
                            });
                        }
                        if !matches!(
                            stopping.recv_timeout(REFRESH_INTERVAL),
                            Err(RecvTimeoutError::Timeout)
                        ) {
                            break;
                        }
                    }
                })?;
            Ok(Self {
                stop,
                worker: Some(worker),
            })
        }

        pub(crate) fn quiesce(&self) -> bool {
            let _ = self.stop.try_send(());
            self.worker.as_ref().is_none_or(JoinHandle::is_finished)
        }
    }

    impl Drop for Refresh {
        fn drop(&mut self) {
            let _ = self.stop.try_send(());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn discover(root: &Path) -> io::Result<Snapshot> {
        let entries = fs::read_dir(root)?
            .take(MAX_ADDRESSES + 1)
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<OsString>>>()?;
        if entries.len() > MAX_ADDRESSES {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut entries = entries;
        entries.sort();
        let interfaces = entries
            .iter()
            .filter_map(|name| physical_interface(&root.join(name)))
            .collect();
        let addresses = crate::interfaces::enumerate()?;
        Ok(Snapshot {
            interfaces,
            addresses,
        })
    }

    fn physical_interface(path: &Path) -> Option<(u32, [u8; 6])> {
        // Bridges, veths, loopback and tunnels lack a backing hardware device.
        path.join("device").exists().then_some(())?;
        let index = read_sysfs(path, "ifindex", 12)?.trim().parse().ok()?;
        let address = read_sysfs(path, "address", 19)?;
        Some((index, parse_mac(address.trim())?))
    }

    fn read_sysfs(path: &Path, name: &str, limit: u64) -> Option<String> {
        let mut value = String::new();
        File::open(path.join(name))
            .ok()?
            .take(limit)
            .read_to_string(&mut value)
            .ok()?;
        (value.len() < limit as usize).then_some(value)
    }

    fn parse_mac(text: &str) -> Option<[u8; 6]> {
        let mut octets = text.split(':');
        let mut mac = [0; 6];
        for byte in &mut mac {
            let octet = octets.next()?;
            if octet.len() != 2 || !octet.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            *byte = u8::from_str_radix(octet, 16).ok()?;
        }
        (octets.next().is_none() && mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rejects_zero_nonunicast_and_malformed_addresses() {
            assert_eq!(
                parse_mac("02:BB:CC:DD:EE:FF"),
                Some([2, 0xbb, 0xcc, 0xdd, 0xee, 0xff])
            );
            for invalid in [
                "00:00:00:00:00:00",
                "01:bb:cc:dd:ee:ff",
                "00:bb:cc:dd:ee:fg",
                "00:bb:cc:dd:ee:f",
                "00:bb:cc:dd:ee:ff:00",
                "+0:bb:cc:dd:ee:ff",
            ] {
                assert_eq!(parse_mac(invalid), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_network_api::InterfaceAddress;

    #[test]
    fn serving_interface_and_ipv6_scope_take_precedence_over_first_hardware_address() {
        let first = [2, 1, 1, 1, 1, 1];
        let second = [2, 2, 2, 2, 2, 2];
        let endpoint = Address {
            family: 6,
            bytes: "fe80::1".parse::<std::net::Ipv6Addr>().unwrap().octets(),
            ..Address::default()
        };
        let snapshot = Snapshot {
            interfaces: vec![(5, first), (6, second)],
            addresses: [5, 6]
                .into_iter()
                .map(|interface_index| InterfaceAddress {
                    address: endpoint,
                    interface_index,
                    reserved: 0,
                })
                .collect(),
        };
        assert_eq!(
            snapshot.mac_for(Address {
                scope_id: 6,
                port: 47989,
                ..endpoint
            }),
            Some(second)
        );
        assert_eq!(snapshot.mac_for(Address::default()), Some(first));
        assert_eq!(Snapshot::default().mac_for(endpoint), None);
    }

    #[test]
    fn failed_refresh_may_use_recent_identity_but_stale_identity_expires() {
        let mac = [2, 1, 1, 1, 1, 1];
        let observed_at = Instant::now();
        let cached = CachedSnapshot {
            snapshot: Snapshot {
                interfaces: vec![(5, mac)],
                ..Snapshot::default()
            },
            observed_at,
        };
        assert_eq!(
            cached.mac_for(Address::default(), observed_at + Duration::from_secs(31)),
            Some(mac)
        );
        assert_eq!(
            cached.mac_for(Address::default(), observed_at + MAX_SNAPSHOT_AGE),
            None
        );
    }
}
