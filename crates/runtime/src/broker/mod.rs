mod configuration;
mod errors;
mod events;
mod hooks;
mod requests;

use amaterasu_contracts::{
    Command, HOST_LIFECYCLE_CONTROL_PRINCIPAL, HOST_LIFECYCLE_SERVICE, HostLifecycleAction,
    Response, RuntimeConfig,
};
use std::collections::VecDeque;
use std::time::Instant;

use events::EventBus;
use hooks::HookRegistry;
use requests::{RequestOptions, RequestRouter};

pub struct Broker {
    configuration: RuntimeConfig,
    requests: RequestRouter,
    events: EventBus,
    hooks: HookRegistry,
    host_lifecycle: VecDeque<HostLifecycleAction>,
}

const HOST_LIFECYCLE_QUEUE_CAPACITY: usize = 8;

impl Broker {
    pub fn new(configuration: RuntimeConfig) -> Result<Self, String> {
        configuration::validate(&configuration)?;

        Ok(Self {
            hooks: HookRegistry::new(configuration.hooks.clone()),
            configuration,
            requests: RequestRouter::default(),
            events: EventBus::default(),
            host_lifecycle: VecDeque::with_capacity(HOST_LIFECYCLE_QUEUE_CAPACITY),
        })
    }

    pub fn dispatch(&mut self, caller: &str, command: Command) -> Response {
        self.dispatch_at(caller, command, Instant::now())
    }

    pub(crate) fn dispatch_at(&mut self, caller: &str, command: Command, now: Instant) -> Response {
        self.requests.expire(now);
        self.hooks.expire(now);

        if matches!(command, Command::HostLifecycleDrain {}) {
            if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                return errors::permission_denied();
            }
            return Response::HostLifecycle {
                action: self.host_lifecycle.pop_front(),
            };
        }

        if caller == HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            return errors::permission_denied();
        }

        let Some(grants) = self.configuration.principals.get(caller) else {
            return errors::permission_denied();
        };

        match command {
            Command::NativeRegister { .. }
            | Command::NativeBind { .. }
            | Command::NativeRelease { .. } => Response::error(
                "native_transport_required",
                "native interfaces require the authenticated ABI endpoint",
            ),
            Command::HostLifecycle { action } => {
                if !grants.call.contains(HOST_LIFECYCLE_SERVICE) {
                    return errors::permission_denied();
                }
                if self.host_lifecycle.len() >= HOST_LIFECYCLE_QUEUE_CAPACITY {
                    return errors::host_lifecycle_capacity_exhausted();
                }
                self.host_lifecycle.push_back(action);
                Response::Ok
            }
            Command::HostLifecycleDrain {} => unreachable!("drain handled before module dispatch"),
            Command::GetSettings {} => Response::Result {
                payload: self
                    .configuration
                    .settings
                    .get(caller)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            },
            Command::GetHostContext {} => {
                if caller != "amaterasu.state" {
                    return errors::permission_denied();
                }
                Response::Result {
                    payload: serde_json::to_value(&self.configuration.host_context)
                        .expect("host context is serializable"),
                }
            }
            Command::Register { service } => {
                self.requests.register_service(caller, grants, service)
            }
            Command::Request {
                service,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } => {
                let options = RequestOptions {
                    service,
                    payload,
                    timeout_ms,
                    operation_id,
                    parent,
                };
                self.requests.submit(caller, grants, options, now)
            }
            Command::Poll {} => self.requests.poll(caller, now),
            Command::CheckRequest { request } => self.requests.check_active(caller, request),
            Command::Complete { request, payload } => {
                self.requests.complete(caller, request, payload, now)
            }
            Command::TakeResult { request } => self.requests.result(caller, request),
            Command::ReleaseResult { request } => self.requests.release(caller, request),
            Command::Cancel { request } => self.requests.cancel(caller, request, now),
            Command::Subscribe {
                topic,
                capacity,
                coalesce,
            } => self
                .events
                .subscribe(caller, grants, topic, capacity, coalesce),
            Command::Publish { topic, payload } => {
                self.events.publish(caller, grants, topic, payload)
            }
            Command::NextEvent { subscription } => self.events.next_event(caller, subscription),
            Command::Unsubscribe { subscription } => self.events.unsubscribe(caller, subscription),
            Command::RegisterHook { hook } => self.hooks.register(caller, grants, hook),
            Command::RunHook { hook, payload } => self.hooks.begin(caller, hook, payload, now),
            Command::HookResult { invocation } => {
                self.hooks
                    .advance(caller, grants, invocation, now, &mut self.requests)
            }
        }
    }
}
