use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};
const MAX_WORKERS: usize = 8;
/// System DNS has no portable cancellation operation. Deadline expiry returns to
/// the caller, but the bounded native worker remains tracked. Destruction joins
/// every worker before the provider DLL may unload; teardown can therefore wait
/// for the OS resolver's own timeout. No worker holds caller buffers or pointers.
pub(super) struct Resolver {
    workers: Mutex<Vec<JoinHandle<()>>>,
}
impl Resolver {
    pub fn new() -> Self {
        Self {
            workers: Mutex::new(Vec::new()),
        }
    }
    pub fn resolve(&self, host: &str, port: u16, timeout_ms: u32) -> io::Result<Vec<SocketAddr>> {
        if timeout_ms == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| io::Error::other("resolver unavailable"))?;
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                let worker = workers.swap_remove(index);
                if worker.join().is_err() {
                    return Err(io::Error::other("resolver worker failed"));
                }
            } else {
                index += 1
            }
        }
        if workers.len() >= MAX_WORKERS {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let host = host.to_owned();
        let (send, receive) = mpsc::sync_channel(1);
        workers.push(
            thread::Builder::new()
                .name("network-dns".into())
                .spawn(move || {
                    let result = (host.as_str(), port)
                        .to_socket_addrs()
                        .and_then(|addresses| {
                            let mut result = Vec::new();
                            for address in addresses {
                                if !result.contains(&address) {
                                    if result.len() == amaterasu_network_api::MAX_ADDRESSES {
                                        return Err(io::Error::other("too many DNS addresses"));
                                    }
                                    result.push(address)
                                }
                            }
                            if result.is_empty() {
                                Err(io::Error::from(io::ErrorKind::NotFound))
                            } else {
                                Ok(result)
                            }
                        });
                    let _ = send.send(result);
                })?,
        );
        drop(workers);
        receive
            .recv_timeout(Duration::from_millis(u64::from(timeout_ms)))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => io::Error::from(io::ErrorKind::TimedOut),
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("resolver worker unavailable")
                }
            })?
    }
}
impl Drop for Resolver {
    fn drop(&mut self) {
        let workers = self.workers.get_mut().unwrap_or_else(|e| e.into_inner());
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}
