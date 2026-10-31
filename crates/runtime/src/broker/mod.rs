mod api;
mod configuration;
mod errors;
mod events;
mod hooks;
mod requests;

use amaterasu_contracts::{
    Command, DIAGNOSTICS_COMPONENT, Grants, HOST_LIFECYCLE_CONTROL_PRINCIPAL,
    HOST_LIFECYCLE_SERVICE, HOST_LOG_SERVICE, HostLifecycleAction, HostLogEvent, Response,
    RuntimeConfig,
};
use std::collections::VecDeque;
use std::time::Instant;

use api::{ApiInvocation, ApiQuery, ApiRegistry};
use events::EventBus;
use hooks::HookRegistry;
use requests::{RequestOptions, RequestRouter};

pub struct Broker {
    configuration: RuntimeConfig,
    requests: RequestRouter,
    events: EventBus,
    hooks: HookRegistry,
    host_lifecycle: VecDeque<HostLifecycleAction>,
    log_events: VecDeque<HostLogEvent>,
    dropped_log_records: u64,
    logging_revision: u64,
    logging_policy_received: bool,
    api: ApiRegistry,
}

const HOST_LIFECYCLE_QUEUE_CAPACITY: usize = 8;
const LOG_EVENT_QUEUE_CAPACITY: usize = 512;
const LOG_DRAIN_BATCH_CAPACITY: usize = 16;
const MAX_LOG_TARGET_BYTES: usize = 128;
const MAX_LOG_MESSAGE_BYTES: usize = 7_500;
const MAX_LOG_PATH_BYTES: usize = 4096;

impl Broker {
    pub fn new(configuration: RuntimeConfig) -> Result<Self, String> {
        configuration::validate(&configuration)?;

        let mut requests = RequestRouter::default();
        if configuration.host_lifecycle_control {
            let grants = Grants {
                provide: [HOST_LOG_SERVICE.to_owned()].into(),
                ..Default::default()
            };
            let result = requests.register_service(
                HOST_LIFECYCLE_CONTROL_PRINCIPAL,
                &grants,
                HOST_LOG_SERVICE.into(),
            );
            if result.is_error() {
                return Err("could not register the reserved host log service".into());
            }
        }
        Ok(Self {
            hooks: HookRegistry::new(configuration.hooks.clone()),
            api: ApiRegistry::new(&configuration),
            configuration,
            requests,
            events: EventBus::default(),
            host_lifecycle: VecDeque::with_capacity(HOST_LIFECYCLE_QUEUE_CAPACITY),
            log_events: VecDeque::with_capacity(LOG_EVENT_QUEUE_CAPACITY),
            dropped_log_records: 0,
            logging_revision: 0,
            logging_policy_received: false,
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

        if matches!(command, Command::DrainLogEvents {}) {
            if caller != HOST_LIFECYCLE_CONTROL_PRINCIPAL {
                return errors::permission_denied();
            }
            let events = (0..LOG_DRAIN_BATCH_CAPACITY)
                .filter_map(|_| self.log_events.pop_front())
                .collect();
            return Response::LogEvents {
                events,
                dropped_records: std::mem::take(&mut self.dropped_log_records),
            };
        }

        if caller == HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            if !self.configuration.host_lifecycle_control {
                return errors::permission_denied();
            }
            return match command {
                Command::Poll {} => self.requests.poll(caller, now),
                Command::CheckRequest { request } => self.requests.check_active(caller, request),
                Command::Complete { request, payload } => {
                    self.requests.complete(caller, request, payload, now)
                }
                _ => errors::permission_denied(),
            };
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
            Command::DrainLogEvents {} => unreachable!("log drain handled before module dispatch"),
            Command::Log {
                level,
                target,
                message,
            } => self.enqueue_log(caller, level, target, message),
            Command::LoggingPolicyChanged {
                revision,
                minimum_level,
                log_path,
            } => self.update_logging_policy(caller, revision, minimum_level, log_path),
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
                if service == HOST_LOG_SERVICE || self.api.owns_service(&service) {
                    errors::permission_denied()
                } else {
                    self.requests.register_service(caller, grants, service)
                }
            }
            Command::RegisterApi { method } => {
                if method.service == HOST_LOG_SERVICE {
                    return errors::permission_denied();
                }
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
                ApiQuery {
                    service: &service,
                    method: &method,
                    major: interface_major,
                    min_minor,
                },
                false,
            ),
            Command::QueryApiApproval {
                service,
                method,
                interface_major,
                min_minor,
            } => self.api.query(
                caller,
                grants,
                ApiQuery {
                    service: &service,
                    method: &method,
                    major: interface_major,
                    min_minor,
                },
                true,
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
                if service == HOST_LOG_SERVICE {
                    return errors::permission_denied();
                }
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
                if service == HOST_LOG_SERVICE && caller != "amaterasu.management" {
                    return errors::permission_denied();
                }
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
            Command::RunHookOperation {
                hook,
                payload,
                operation_id,
                cleanup,
            } => self
                .hooks
                .begin_operation(caller, hook, payload, operation_id, cleanup, now),
            Command::ListApiApprovals {
                parent,
                after,
                limit,
            } => {
                let authority = match parent {
                    Some(id) => match self.requests.discovery_authority(caller, id) {
                        Ok(authority) => Some(authority),
                        Err(error) => return error,
                    },
                    None => None,
                };
                self.api
                    .list_approvals(caller, grants, authority.as_ref(), after, limit)
            }
            Command::LookupHook { operation_id } => self.hooks.lookup(caller, &operation_id),
            Command::CancelHook { invocation } => {
                self.hooks
                    .cancel(caller, invocation, now, &mut self.requests)
            }
            Command::ReleaseHook { invocation } => self.hooks.release(caller, invocation),
            Command::HookResult { invocation } => {
                self.hooks
                    .advance(caller, grants, invocation, now, &mut self.requests)
            }
        }
    }

    fn enqueue_log(
        &mut self,
        caller: &str,
        level: u8,
        target: String,
        message: String,
    ) -> Response {
        if level > 5
            || target.is_empty()
            || target.len() > MAX_LOG_TARGET_BYTES
            || target.chars().any(char::is_control)
            || message.len() > MAX_LOG_MESSAGE_BYTES
            || message.chars().any(|character| character == '\0')
        {
            return Response::error("invalid_log_record", "log record is invalid");
        }
        let target = format!("{caller}::{target}");
        if target.len() > MAX_LOG_TARGET_BYTES {
            return Response::error("invalid_log_record", "log record is invalid");
        }
        if self.log_events.len() >= LOG_EVENT_QUEUE_CAPACITY {
            self.dropped_log_records = self.dropped_log_records.saturating_add(1);
            return Response::Ok;
        }
        self.log_events.push_back(HostLogEvent::Record {
            level,
            target,
            message,
        });
        Response::Ok
    }

    fn update_logging_policy(
        &mut self,
        caller: &str,
        revision: u64,
        minimum_level: u8,
        log_path: String,
    ) -> Response {
        if caller != DIAGNOSTICS_COMPONENT {
            return errors::permission_denied();
        }
        if minimum_level > 6
            || log_path.is_empty()
            || log_path.len() > MAX_LOG_PATH_BYTES
            || log_path.chars().any(char::is_control)
        {
            return Response::error("invalid_logging_policy", "logging policy is invalid");
        }
        if revision < self.logging_revision {
            return Response::error("stale_revision", "logging policy revision is stale");
        }
        if self.logging_policy_received && revision == self.logging_revision {
            return Response::Ok;
        }

        if self.log_events.len() >= LOG_EVENT_QUEUE_CAPACITY {
            if let Some(record_index) = self
                .log_events
                .iter()
                .position(|event| matches!(event, HostLogEvent::Record { .. }))
            {
                self.log_events.remove(record_index);
                self.dropped_log_records = self.dropped_log_records.saturating_add(1);
            } else {
                return Response::error("capacity_exhausted", "host log event queue is full");
            }
        }
        self.log_events.push_back(HostLogEvent::Policy {
            revision,
            minimum_level,
            log_path,
        });
        self.logging_revision = revision;
        self.logging_policy_received = true;
        Response::Ok
    }
}
