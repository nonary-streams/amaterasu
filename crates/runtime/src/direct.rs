//! Direct call routing. The host registers each module's verified handler;
//! the runtime decides who may reach it and on which thread.
use amaterasu_abi::DirectCallHandler;
use std::cell::RefCell;
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

/// One direct call a principal is serving on the current thread.
struct Frame {
    /// Address of the owning registry, so two runtimes never share a frame.
    registry: usize,
    provider: String,
    service: String,
    caller: String,
}

thread_local! {
    /// Direct calls this thread is serving, innermost last.
    static SERVING: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Keeps a direct call on the current thread's record until dropped.
pub(crate) struct Serving(());

impl Drop for Serving {
    fn drop(&mut self) {
        // The record is already gone when the thread is being torn down.
        let _ = SERVING.try_with(|frames| frames.borrow_mut().pop());
    }
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

    /// Records that `provider` is serving `caller`'s direct call to `service`
    /// on this thread for as long as the returned guard lives.
    pub fn serve(&self, provider: &str, service: &str, caller: &str) -> Serving {
        let frame = Frame {
            registry: self.identity(),
            provider: provider.to_owned(),
            service: service.to_owned(),
            caller: caller.to_owned(),
        };
        SERVING.with_borrow_mut(|frames| frames.push(frame));
        Serving(())
    }

    /// The caller of the direct call that `layer` is serving for `service`
    /// right now. A layer that passes that call to the next hop keeps this
    /// identity, as a forwarded request keeps its parent's caller.
    pub fn caller_being_served(&self, layer: &str, service: &str) -> Option<String> {
        SERVING.with_borrow(|frames| {
            frames
                .last()
                .filter(|frame| {
                    frame.registry == self.identity()
                        && frame.provider == layer
                        && frame.service == service
                })
                .map(|frame| frame.caller.clone())
        })
    }

    fn identity(&self) -> usize {
        std::ptr::from_ref(self) as usize
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

    #[test]
    fn only_the_layer_serving_a_call_for_that_service_sees_its_caller() {
        let registry = DirectRegistry::default();
        let other_runtime = DirectRegistry::default();
        assert_eq!(registry.caller_being_served("layer", "echo/1"), None);
        {
            let _outer = registry.serve("layer", "echo/1", "consumer");
            assert_eq!(
                registry.caller_being_served("layer", "echo/1").as_deref(),
                Some("consumer")
            );
            assert_eq!(registry.caller_being_served("layer", "other/1"), None);
            assert_eq!(registry.caller_being_served("provider", "echo/1"), None);
            assert_eq!(other_runtime.caller_being_served("layer", "echo/1"), None);
            {
                // The next hop is now the innermost call on this thread.
                let _inner = registry.serve("provider", "echo/1", "consumer");
                assert_eq!(registry.caller_being_served("layer", "echo/1"), None);
            }
            assert_eq!(
                registry.caller_being_served("layer", "echo/1").as_deref(),
                Some("consumer")
            );
        }
        assert_eq!(registry.caller_being_served("layer", "echo/1"), None);
    }
}
