//! Thread-safe scheduling notification. It conveys readiness only; consumers
//! still acquire data through their selected, versioned provider contracts.
use crate::{Command, RuntimeClient};

#[derive(Clone)]
pub struct WorkNotifier {
    endpoint: crate::abi::HostApi,
}
// SAFETY: HostApi explicitly permits concurrent invokes from consumer workers.
// The managed-module contract joins those workers before releasing the runtime.
unsafe impl Send for WorkNotifier {}
// SAFETY: notify uses only the thread-safe borrowed endpoint and local replies.
unsafe impl Sync for WorkNotifier {}
impl WorkNotifier {
    /// Notify readiness through the borrowed runtime endpoint. All copies must
    /// stop being used before the owning module's runtime is destroyed.
    pub fn notify(&self) {
        // SAFETY: obtained from a live RuntimeClient, whose module must stop its
        // workers and unsubscribe callbacks before the endpoint is destroyed.
        let runtime = unsafe { RuntimeClient::from_endpoint(self.endpoint) };
        let _ = runtime.send(Command::NotifyWorkReady {});
    }
    pub(crate) fn new(endpoint: crate::abi::HostApi) -> Self {
        Self { endpoint }
    }
}
