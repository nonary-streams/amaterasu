//! A runtime endpoint answered by the test itself.
//!
//! [`TestHost`](crate::TestHost) is the real runtime and is the right default.
//! Use a [`ScriptedHost`] for what a healthy runtime will not do on request: a
//! reply lost in transport, an error at one exact step, or a peer whose every
//! answer the test decides.
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK, Status,
};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

type Respond<S> = dyn Fn(&mut S, Command) -> Response + Send + Sync;

struct Script<S> {
    state: Mutex<S>,
    respond: Box<Respond<S>>,
    replies_to_lose: Mutex<usize>,
}

impl<S> Script<S> {
    /// Consumes one pending reply loss, if any remain.
    fn loses_this_reply(&self) -> bool {
        let mut remaining = self
            .replies_to_lose
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        true
    }
}

/// Test-owned state and the function that answers every command a module
/// sends, in place of a runtime.
///
/// ```ignore
/// #[derive(Default)]
/// struct Seen { replies: Vec<Value> }
///
/// let host = ScriptedHost::new(Seen::default(), |seen, command| match command {
///     Command::Register { .. } => Response::Ok,
///     Command::Poll {} => Response::Pending,
///     Command::Complete { payload, .. } => {
///         seen.replies.push(payload);
///         Response::Ok
///     }
///     _ => Response::error("unexpected", "not scripted"),
/// });
/// let mut module = MyModule::start(host.client()).unwrap();
/// module.tick().unwrap();
/// assert!(host.state().replies.is_empty());
/// ```
pub struct ScriptedHost<S> {
    script: Arc<Script<S>>,
}

impl<S: Send + 'static> ScriptedHost<S> {
    /// `respond` receives the state and each command, and returns the reply.
    /// It must not send commands itself.
    pub fn new(
        state: S,
        respond: impl Fn(&mut S, Command) -> Response + Send + Sync + 'static,
    ) -> Self {
        Self {
            script: Arc::new(Script {
                state: Mutex::new(state),
                respond: Box::new(respond),
                replies_to_lose: Mutex::new(0),
            }),
        }
    }

    /// A client whose commands are answered by the script. Each client keeps
    /// the script alive for the rest of the process, so it stays valid
    /// wherever the module under test moves it.
    pub fn client(&self) -> RuntimeClient {
        let context = Arc::into_raw(self.script.clone());
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: context.cast_mut().cast(),
            invoke: invoke::<S>,
        };
        // SAFETY: the reference taken by `into_raw` is never returned, so the
        // script outlives the client. The script is `Sync`: its state is
        // behind a mutex and its responder is `Send + Sync`.
        unsafe { RuntimeClient::from_endpoint(endpoint) }
    }

    /// Inspect or change the state. Release the guard before the module under
    /// test sends another command.
    pub fn state(&self) -> MutexGuard<'_, S> {
        self.script
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Lose the replies to the next `count` commands. The script still sees
    /// and answers each one, as a runtime that acted would have; only the
    /// module is left not knowing.
    pub fn lose_replies(&self, count: usize) {
        *self
            .script
            .replies_to_lose
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = count;
    }
}

unsafe extern "C" fn invoke<S>(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: the context is the script kept alive by `ScriptedHost::client`.
    let script = unsafe { &*context.cast::<Script<S>>() };
    // SAFETY: the SDK lends the command bytes for this synchronous call.
    let Ok(bytes) = (unsafe { input.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let response = {
        let mut state = script.state.lock().unwrap_or_else(PoisonError::into_inner);
        (script.respond)(&mut state, command)
    };
    if script.loses_this_reply() {
        return STATUS_INTERNAL_ERROR;
    }
    let Ok(encoded) = serde_json::to_vec(&response) else {
        return STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the sink is valid until this call returns and copies the bytes.
    unsafe { reply.send(&encoded) };
    STATUS_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::ControlError;
    use amaterasu_module_sdk::requests::ReplyDelivery;
    use serde_json::{Value, json};

    #[derive(Default)]
    struct Seen {
        replies: Vec<Value>,
    }

    fn host() -> ScriptedHost<Seen> {
        ScriptedHost::new(Seen::default(), |seen, command| match command {
            Command::Complete { payload, .. } => {
                seen.replies.push(payload);
                Response::Ok
            }
            _ => Response::error("unexpected", "not scripted"),
        })
    }

    #[test]
    fn the_script_answers_commands_and_keeps_what_it_saw() {
        let host = host();
        let client = host.client();
        assert_eq!(
            client.reply(1, &json!("first")),
            Ok(ReplyDelivery::Recorded)
        );
        assert!(client.provide("example/1").is_err());
        assert_eq!(host.state().replies, [json!("first")]);
    }

    #[test]
    fn a_lost_reply_still_reaches_the_script() {
        let host = host();
        let client = host.client();
        host.lose_replies(1);
        assert!(matches!(
            client.reply(1, &json!("lost")),
            Err(ControlError::Transport(_))
        ));
        assert_eq!(client.reply(2, &json!("kept")), Ok(ReplyDelivery::Recorded));
        assert_eq!(host.state().replies, [json!("lost"), json!("kept")]);
    }

    #[test]
    fn a_client_outlives_the_host_that_made_it() {
        let client = host().client();
        assert_eq!(client.reply(1, &json!(1)), Ok(ReplyDelivery::Recorded));
    }
}
