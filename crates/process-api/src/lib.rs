//! Experimental version 1 process-provider contract.
//!
//! Trusted startup composition selects an independently implemented provider.
//! Native grants authorize access; they do not sandbox native code. Bindings
//! authenticate principals, and all operation IDs and resource tokens are scoped
//! to the principal and exact provider instance. Tokens are opaque values: no
//! PID, OS handle, pointer, file descriptor or allocator crosses this interface.
//!
//! Submit only admits bounded queued work. Before publishing a Spawn receipt,
//! the provider owns the spawned child and its native wait obligation. Its worker
//! must retain that ownership and reap exactly once after a binding is released;
//! neither closing observation nor releasing an operation kills a detached
//! child. Managed roots remain exclusively wait-owned and PID/PGID-pinned until
//! their group has been signaled and verified retired. A replacement or package
//! activation must not unload the provider while it owns any live child.
//!
//! Every submit carries a caller-unique operation ID and immutable serialized
//! request. Retrying identical input is idempotent; different input conflicts.
//! Synchronous non-OK admission must not have accepted work. Once accepted,
//! poll is read-only. Release is allowed only for terminal operations and frees
//! the response, not a live effect. Operation IDs consist of a provider-issued
//! binding epoch and a strictly increasing, gapless sequence. The provider must
//! enforce finite limits and reject admission before effects when it cannot
//! retain the operation identity or resource ownership. Binding release closes
//! admission for its epoch; it cannot discard
//! its pending effect, output-sink lease, native wait ownership or fingerprint.
//! The exact immutable request, including its sink tokens, is fingerprinted.
//! Accepted sequences are gapless and strictly ordered within each epoch;
//! providers may accept a duplicate only when the retained fingerprint matches.
//! Released contiguous sequences compact to a high-watermark. Submissions at or
//! below that watermark are permanently rejected, never re-executed; poll reports
//! Released for them. Out-of-order releases remain bounded entries until the
//! contiguous prefix advances. A closed epoch cannot admit new operations, but
//! retained results may be polled and released by the same principal. Still-live
//! children and their sinks survive epoch closure. The provider bounds active
//! and retired epochs and backpressures instead of forgetting unresolved work.
//! Epochs are provider-issued, strictly increasing per principal for the provider
//! instance, never wrap or repeat, and are bound to the authenticated principal.
//! The provider can compact a fully settled closed epoch into a closed-epoch
//! watermark; IDs in compacted epochs must be rejected rather than replayed. One
//! binding submits one sequence at a time; a non-OK submit admits no work, and
//! an OK submit consumes exactly the next sequence. A retry of an unretired ID
//! must use its exact request. An ambiguous call must be reconciled before the
//! next sequence is submitted.
//! Provider work queues and all retained resources are bounded; queue exhaustion
//! rejects admission before spawning. Terminal receipts remain available to
//! observation until explicitly released, even after native reaping.
//!
//! One opened sink is provider-owned and retains the original append-file
//! identity across managed, preparation and detached children. Reopening a path
//! later is not equivalent. Separate per-command overrides open separate sinks;
//! closing an observation cannot invalidate sinks used by running children.
//! This interface is a foundation only; it is not a deployed reaper or proof of
//! lifecycle qualification. The distinct trusted host/provider lifetime ABI is
//! in `lifetime`; composition must use its retirement barrier and must not
//! unload a provider based on this consumer interface alone.
pub mod lifetime;

use amaterasu_abi::{BorrowedBytes, NativeInterface, ReplySink, Status};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::c_void};

pub const SERVICE: &str = "amaterasu.process/1";
pub const CONTRACT: &str = "amaterasu.process-interface/1";
pub const MAJOR: u32 = 1;
pub const MINOR: u32 = 0;
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
pub const MAX_ARGUMENTS: usize = 256;
pub const MAX_ENVIRONMENT: usize = 256;
pub const MAX_FIELD_BYTES: usize = 8192;
pub const MAX_TOKEN_BYTES: usize = 128;
/// No work was admitted because bounded capacity is exhausted.
pub const RESOURCE_EXHAUSTED: Status = 100;
/// The operation ID is already bound to a different immutable request.
pub const CONFLICT: Status = 101;
/// The requested transition is not valid for this operation or resource.
pub const INVALID_STATE: Status = 102;

/// All callbacks are thread-safe and cannot unwind. Both borrowed byte inputs
/// and reply sinks are valid only synchronously; the provider copies input
/// before submit returns. Calls do not wait for process creation or retirement.
/// Poll and inspect each write one bounded JSON response when returning OK.
/// Inspect reads a cached process snapshot without waiting for native work and
/// consumes no operation identity. The caller must end
/// all concurrent calls before releasing `base`; binding release does not stop
/// provider workers or abandon their children.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessApi {
    pub base: NativeInterface,
    /// Copies the provider's JSON capabilities before any process work.
    pub capabilities: unsafe extern "C" fn(*mut c_void, ReplySink) -> Status,
    /// Copies the provider-issued positive decimal epoch as a JSON string.
    pub epoch: unsafe extern "C" fn(*mut c_void, ReplySink) -> Status,
    pub submit: unsafe extern "C" fn(*mut c_void, BorrowedBytes, BorrowedBytes) -> Status,
    pub poll: unsafe extern "C" fn(*mut c_void, BorrowedBytes, ReplySink) -> Status,
    pub inspect: unsafe extern "C" fn(*mut c_void, BorrowedBytes, ReplySink) -> Status,
    pub release_operation: unsafe extern "C" fn(*mut c_void, BorrowedBytes) -> Status,
}

/// Declared features and provider limits, separate from ABI version and size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub managed_groups: bool,
    pub detached: bool,
    pub shared_output: bool,
    pub elevated: bool,
    pub elevated_output: bool,
    pub max_payload_bytes: u32,
    pub max_queued_operations: u32,
    pub max_operation_receipts: u32,
    pub max_processes: u32,
    pub max_reservations: u32,
    pub max_sinks: u32,
    pub max_epochs: u32,
}
impl Capabilities {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_payload_bytes == 0
            || self.max_payload_bytes as usize > MAX_PAYLOAD_BYTES
            || self.max_queued_operations == 0
            || self.max_operation_receipts == 0
            || self.max_processes == 0
            || self.max_reservations == 0
            || self.max_epochs == 0
            || (self.shared_output && self.max_sinks == 0)
            || (self.elevated_output && !(self.elevated && self.shared_output))
        {
            Err("invalid_process_capabilities")
        } else {
            Ok(())
        }
    }
}

macro_rules! token {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            fn valid(&self) -> bool {
                valid_identity(&self.0)
            }
        }
    };
}
token!(ReservationToken);
token!(OutputSinkToken);
token!(ProcessToken);

pub fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Canonical operation ID: provider-issued positive decimal epoch, a colon,
/// then a positive decimal sequence; neither part has leading zeros or wraps.
pub fn valid_operation_id(value: &str) -> bool {
    let Some((epoch, sequence)) = value.split_once(':') else {
        return false;
    };
    !epoch.starts_with('0')
        && epoch.bytes().all(|byte| byte.is_ascii_digit())
        && epoch.parse::<u64>().is_ok_and(|number| number > 0)
        && !sequence.starts_with('0')
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.parse::<u64>().is_ok_and(|number| number > 0)
}
fn valid_text(value: &str) -> bool {
    value.len() <= MAX_FIELD_BYTES && !value.contains('\0')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    Managed,
    Detached,
}

/// Fully resolved command. Environment replaces, rather than extends, the
/// provider environment. Separate stdout and stderr sinks permit explicit
/// command-level redirection without substituting the shared sink.
/// Callers resolve executable and working-directory behavior before submission;
/// the provider independently applies its trusted platform-specific path and
/// authorization policy. String validation here intentionally does not use the
/// build host's path syntax to decide whether a foreign-platform path is valid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub executable: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<String>,
    pub elevated: bool,
    pub stdout: OutputTarget,
    pub stderr: OutputTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputTarget {
    Null,
    Sink { sink: OutputSinkToken },
}

impl CommandSpec {
    fn valid(&self) -> bool {
        !self.executable.is_empty()
            && valid_text(&self.executable)
            && self.arguments.len() <= MAX_ARGUMENTS
            && self.arguments.iter().all(|value| valid_text(value))
            && self.environment.len() <= MAX_ENVIRONMENT
            && self.environment.iter().all(|(key, value)| {
                !key.is_empty() && !key.contains('=') && valid_text(key) && valid_text(value)
            })
            && self
                .working_directory
                .as_deref()
                .is_none_or(|path| !path.is_empty() && valid_text(path))
            && [&self.stdout, &self.stderr]
                .iter()
                .all(|target| match target {
                    OutputTarget::Null => true,
                    OutputTarget::Sink { sink } => sink.valid(),
                })
    }
}

/// The provider issues opaque reservations before any child is spawned.
/// Cancellation only applies to an unconsumed reservation; it never signals a
/// spawned child and is rejected while its spawn is in flight. An admitted spawn
/// atomically retains its reservation and all referenced sink leases at submit
/// admission until its outcome is known, despite concurrent sink closure,
/// operation release or binding release. A no-child Spawn error guarantees no
/// child was created or that a created child was conclusively retired. If spawn
/// or containment fails with an unretired child, SpawnFailedOwned returns its
/// opaque identity and failure; the provider retains exact cleanup ownership.
/// Closing a sink prevents new use, while pending and running children retain
/// their existing leases. Output open and reservation cancellation each return
/// an explicit terminal outcome. Binding release retires unused reservations but
/// does not drop a pending spawn, its sink leases or any child.
/// Releasing observation of a live or unretired managed tree or failed spawn
/// is forbidden: the exact child and group ownership remain pinned until
/// retirement. Releasing observation of a running detached child is allowed,
/// but never releases the provider's wait ownership or output leases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Reserve {
        kind: ProcessKind,
    },
    CancelReservation {
        reservation: ReservationToken,
    },
    OpenSink {
        path: String,
    },
    CloseSink {
        sink: OutputSinkToken,
    },
    Spawn {
        reservation: ReservationToken,
        kind: ProcessKind,
        command: CommandSpec,
    },
    StopManaged {
        process: ProcessToken,
    },
    ForceManaged {
        process: ProcessToken,
    },
    /// Retry cleanup of a spawn/containment failure against the retained exact
    /// child identity. It does not assume a managed group was ever established.
    RetireFailedSpawn {
        process: ProcessToken,
    },
    ReleaseObservation {
        process: ProcessToken,
    },
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::Reserve { .. } => true,
            Self::CancelReservation { reservation } => reservation.valid(),
            Self::OpenSink { path } => !path.is_empty() && valid_text(path),
            Self::CloseSink { sink } => sink.valid(),
            Self::Spawn {
                reservation,
                command,
                ..
            } => reservation.valid() && command.valid(),
            Self::StopManaged { process }
            | Self::ForceManaged { process }
            | Self::RetireFailedSpawn { process }
            | Self::ReleaseObservation { process } => process.valid(),
        };
        if valid && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_PAYLOAD_BYTES) {
            Ok(())
        } else {
            Err("invalid_process_request")
        }
    }
}

/// RootExited is nonterminal for a managed tree. Retired is returned only after
/// native wait and any managed group retirement are confirmed. WaitFailed
/// retains ownership and must never be treated as retirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcessState {
    Running,
    RootExited {
        success: bool,
        exit_code: Option<i32>,
        group: GroupState,
    },
    Retired {
        success: bool,
        exit_code: Option<i32>,
    },
    WaitFailed {
        code: String,
    },
}

/// Unknown is not evidence of group retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupState {
    Live,
    Empty,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Pending,
    /// No operation has been admitted under this caller-scoped ID.
    NotFound,
    /// Its terminal response was explicitly released; its fingerprint remains.
    Released,
    Reservation {
        reservation: ReservationToken,
    },
    Sink {
        sink: OutputSinkToken,
    },
    Process {
        process: ProcessToken,
    },
    SpawnFailedOwned {
        process: ProcessToken,
        code: String,
    },
    ProcessStatus {
        state: ProcessState,
    },
    Signaled {
        delivered: bool,
    },
    Ok,
    Error {
        code: String,
    },
}
impl Response {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::Reservation { reservation } => reservation.valid(),
            Self::Sink { sink } => sink.valid(),
            Self::Process { process } => process.valid(),
            Self::SpawnFailedOwned { process, code } => process.valid() && valid_identity(code),
            Self::ProcessStatus {
                state: ProcessState::WaitFailed { code },
            }
            | Self::Error { code } => valid_identity(code),
            _ => true,
        };
        if valid && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_PAYLOAD_BYTES) {
            Ok(())
        } else {
            Err("invalid_process_response")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_must_describe_bounded_consistent_resources() {
        let mut caps = Capabilities {
            managed_groups: true,
            detached: true,
            shared_output: true,
            elevated: false,
            elevated_output: false,
            max_payload_bytes: MAX_PAYLOAD_BYTES as u32,
            max_queued_operations: 4,
            max_operation_receipts: 8,
            max_processes: 4,
            max_reservations: 4,
            max_sinks: 4,
            max_epochs: 4,
        };
        assert!(caps.validate().is_ok());
        caps.elevated_output = true;
        assert!(caps.validate().is_err());
        caps.elevated = true;
        assert!(caps.validate().is_ok());
        caps.max_queued_operations = 0;
        assert!(caps.validate().is_err());
    }

    #[test]
    fn operation_ids_are_canonical_and_bounded() {
        for valid in ["1:1", "9:18446744073709551615"] {
            assert!(valid_operation_id(valid));
        }
        for invalid in [
            "1:0",
            "1:01",
            "1:",
            ":1",
            "1:1:2",
            "1:18446744073709551616",
            "epoch:2",
            "01:2",
        ] {
            assert!(!valid_operation_id(invalid), "{invalid}");
        }
    }

    #[test]
    fn request_validation_bounds_inputs_without_exposing_native_handles() {
        let mut request = Request::Spawn {
            reservation: ReservationToken("provider-reservation".into()),
            kind: ProcessKind::Managed,
            command: CommandSpec {
                executable: "/bin/example".into(),
                arguments: vec!["arg".into()],
                environment: BTreeMap::from([("PATH".into(), "/bin".into())]),
                working_directory: None,
                elevated: false,
                stdout: OutputTarget::Sink {
                    sink: OutputSinkToken("shared-sink".into()),
                },
                stderr: OutputTarget::Sink {
                    sink: OutputSinkToken("shared-sink".into()),
                },
            },
        };
        assert!(request.validate().is_ok());
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);
        if let Request::Spawn { command, .. } = &mut request {
            command
                .environment
                .insert("INVALID=KEY".into(), "value".into());
        }
        assert!(request.validate().is_err());
    }

    #[test]
    fn process_status_retains_unknown_group_and_rejects_invalid_codes() {
        let response = Response::ProcessStatus {
            state: ProcessState::RootExited {
                success: true,
                exit_code: Some(0),
                group: GroupState::Unknown,
            },
        };
        let json = serde_json::to_vec(&response).unwrap();
        assert_eq!(serde_json::from_slice::<Response>(&json).unwrap(), response);
        assert!(
            Response::ProcessStatus {
                state: ProcessState::WaitFailed {
                    code: String::new()
                }
            }
            .validate()
            .is_err()
        );
    }
}
