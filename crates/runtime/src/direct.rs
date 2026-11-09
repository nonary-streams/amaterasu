//! Direct call routing. The host registers each module's verified handler;
//! the runtime decides who may reach it and on which thread.
use amaterasu_abi::DirectCallHandler;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::thread::ThreadId;

const MAX_HANDLERS: usize = 256;

#[derive(Clone, Copy)]
pub(crate) struct Handler {
    pub instance: *mut c_void,
    pub call: DirectCallHandler,
    thread: ThreadId,
}

// SAFETY: the instance pointer is only passed back to its own module's
// handler, and `DirectRegistry::for_current_thread` releases it solely on the
// thread that registered it. The registry itself never dereferences it.
unsafe impl Send for Handler {}

#[derive(Default)]
pub(crate) struct DirectRegistry {
    handlers: Mutex<BTreeMap<String, Handler>>,
}

impl DirectRegistry {
    /// Binds the registering thread: a module's instance is not required to be
    /// usable from any other thread, so no other thread may enter it.
    pub fn register(
        &self,
        principal: &str,
        instance: *mut c_void,
        call: DirectCallHandler,
    ) -> bool {
        let Ok(mut handlers) = self.handlers.lock() else {
            return false;
        };
        if instance.is_null() || handlers.contains_key(principal) || handlers.len() >= MAX_HANDLERS
        {
            return false;
        }
        handlers.insert(
            principal.to_owned(),
            Handler {
                instance,
                call,
                thread: std::thread::current().id(),
            },
        );
        true
    }

    pub fn unregister(&self, principal: &str) {
        if let Ok(mut handlers) = self.handlers.lock() {
            handlers.remove(principal);
        }
    }

    /// The provider's handler, if the caller is on the provider's own thread.
    /// Because registration, calls and removal all happen on that thread, a
    /// handler returned here cannot be unregistered while it is being invoked.
    pub fn for_current_thread(&self, principal: &str) -> Option<Handler> {
        let handler = *self.handlers.lock().ok()?.get(principal)?;
        (handler.thread == std::thread::current().id()).then_some(handler)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_abi::{BorrowedBytes, ReplySink, STATUS_OK, Status};

    unsafe extern "C" fn handler(
        _: *mut c_void,
        _: BorrowedBytes,
        _: BorrowedBytes,
        _: BorrowedBytes,
        _: ReplySink,
    ) -> Status {
        STATUS_OK
    }

    #[test]
    fn a_handler_is_reachable_only_from_its_registering_thread_until_removed() {
        let registry = std::sync::Arc::new(DirectRegistry::default());
        let mut instance = 0_u8;
        let pointer = std::ptr::from_mut(&mut instance).cast::<c_void>();
        assert!(registry.register("provider", pointer, handler));
        assert!(!registry.register("provider", pointer, handler));
        assert!(!registry.register("null", std::ptr::null_mut(), handler));
        assert!(registry.for_current_thread("provider").is_some());
        assert!(registry.for_current_thread("other").is_none());

        let elsewhere = registry.clone();
        let reachable =
            std::thread::spawn(move || elsewhere.for_current_thread("provider").is_some())
                .join()
                .unwrap();
        assert!(!reachable);

        registry.unregister("provider");
        assert!(registry.for_current_thread("provider").is_none());
    }
}
