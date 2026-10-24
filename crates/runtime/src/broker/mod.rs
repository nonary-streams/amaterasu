mod api;
mod configuration;
mod errors;
mod events;
mod hooks;
mod requests;

use amaterasu_contracts::{Command, Response, RuntimeConfig};
use std::time::Instant;

use api::{ApiInvocation, ApiRegistry};
use events::EventBus;
use hooks::HookRegistry;
use requests::{RequestOptions, RequestRouter};

pub struct Broker {
    configuration: RuntimeConfig,
    requests: RequestRouter,
    events: EventBus,
    hooks: HookRegistry,
    api: ApiRegistry,
}

impl Broker {
    pub fn new(configuration: RuntimeConfig) -> Result<Self, String> {
        configuration::validate(&configuration)?;

        Ok(Self {
            hooks: HookRegistry::new(configuration.hooks.clone()),
            api: ApiRegistry::new(&configuration),
            configuration,
            requests: RequestRouter::default(),
            events: EventBus::default(),
        })
    }

    pub fn dispatch(&mut self, caller: &str, command: Command) -> Response {
        self.dispatch_at(caller, command, Instant::now())
    }

    pub(crate) fn dispatch_at(&mut self, caller: &str, command: Command, now: Instant) -> Response {
        self.requests.expire(now);
        self.hooks.expire(now);

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
            Command::GetSettings {} => Response::Result {
                payload: self
                    .configuration
                    .settings
                    .get(caller)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            },
            Command::GetHostContext {} => {
                // Audio needs the host-owned state root on Linux to reconcile
                // its own Pulse cleanup journal after a worker restart.
                if caller != "amaterasu.state"
                    && !(cfg!(target_os = "linux") && caller == "amaterasu.audio")
                {
                    return errors::permission_denied();
                }
                Response::Result {
                    payload: serde_json::json!({
                        "schema": self.configuration.host_context.schema,
                        "state_root": self.configuration.host_context.state_root,
                    }),
                }
            }
            Command::GetPackageRoot {} => {
                if caller != "amaterasu.management" {
                    return errors::permission_denied();
                }
                match &self.configuration.host_context.package_root {
                    Some(package_root) => Response::Result {
                        payload: serde_json::json!({"package_root":package_root}),
                    },
                    None => Response::error(
                        "package_root_unavailable",
                        "host did not supply a package root",
                    ),
                }
            }
            Command::GetRunningPackageDigest {} => {
                if caller != "amaterasu.management" {
                    return errors::permission_denied();
                }
                match &self.configuration.host_context.running_package_digest {
                    Some(digest) => Response::Result {
                        payload: serde_json::json!({"digest": digest}),
                    },
                    None => Response::error(
                        "running_package_unavailable",
                        "host did not supply a loaded package digest",
                    ),
                }
            }
            Command::Register { service } => {
                if self.api.owns_service(&service) {
                    errors::permission_denied()
                } else {
                    self.requests.register_service(caller, grants, service)
                }
            }
            Command::RegisterApi { method } => {
                let registered = self.api.register(caller, grants, method.clone());
                if registered.is_error() {
                    registered
                } else {
                    self.requests
                        .register_service(caller, grants, method.service)
                }
            }
            Command::QueryApi {
                service,
                method,
                interface_major,
                min_minor,
            } => self.api.query(
                caller,
                grants,
                &service,
                &method,
                interface_major,
                min_minor,
            ),
            Command::CallApi {
                service,
                method,
                interface_major,
                interface_minor,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } => {
                let initiator = match parent {
                    Some(parent_id) => {
                        match self.requests.initiator_for_parent(caller, parent_id) {
                            Ok(initiator) => initiator.to_owned(),
                            Err(error) => return error,
                        }
                    }
                    None => caller.to_owned(),
                };
                match self.api.callable(
                    caller,
                    &initiator,
                    grants,
                    ApiInvocation {
                        service: &service,
                        method: &method,
                        major: interface_major,
                        minor: interface_minor,
                        payload: &payload,
                    },
                ) {
                    Ok((payload, response_schema)) => self.requests.submit(
                        caller,
                        grants,
                        RequestOptions {
                            service,
                            payload,
                            timeout_ms,
                            operation_id,
                            parent,
                            response_schema: Some(response_schema),
                        },
                        now,
                    ),
                    Err(error) => error,
                }
            }
            Command::Request {
                service,
                payload,
                timeout_ms,
                operation_id,
                parent,
            } => {
                if self.api.owns_service(&service) {
                    return errors::permission_denied();
                }
                let options = RequestOptions {
                    service,
                    payload,
                    timeout_ms,
                    operation_id,
                    parent,
                    response_schema: None,
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
