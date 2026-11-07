//! Durable intent for cleanup of Linux PulseAudio resources.
//!
//! The journal is kept below the host-selected state root. Records are written
//! before mutating the Pulse server so a restart can reconcile an operation
//! whose callback was lost.

use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const JOURNAL_VERSION: u64 = 1;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;
pub(super) const MAX_SINK_INTENTS: usize = 64;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CleanupState {
    pub(super) sinks: BTreeMap<String, SinkIntent>,
    pub(super) route: Option<RouteIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ServerIdentity {
    pub(super) address: String,
    pub(super) instance_cookie: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SinkIntent {
    pub(super) owner_token: String,
    pub(super) server_identity: Option<ServerIdentity>,
    pub(super) module_index: Option<u32>,
    pub(super) lease_active: bool,
    pub(super) cleanup_requested: bool,
    /// True only after an ordered same-connection round trip fences the load.
    pub(super) load_quiesced: bool,
    pub(super) unload_submitted: bool,
    pub(super) unload_confirmed: bool,
    pub(super) unload_quiesced: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RouteIntent {
    pub(super) target_sink: String,
    pub(super) original_sink: String,
    /// False means the assignment callback was indeterminate. Keep this
    /// tombstone while the current route is not our target because the server
    /// may still apply the request after the client callback has failed.
    pub(super) assignment_confirmed: bool,
    /// True only after an ordered same-connection round trip fences assignment.
    pub(super) assignment_quiesced: bool,
    pub(super) restoration_submitted: bool,
    pub(super) restoration_confirmed: bool,
    pub(super) restoration_quiesced: bool,
}

#[derive(Clone, Debug)]
pub(super) struct CleanupJournal {
    directory: PathBuf,
    path: PathBuf,
    owner_uid: u32,
}

impl CleanupJournal {
    pub(super) fn open(state_root: &Path) -> Result<Self, &'static str> {
        if !state_root.is_absolute() {
            return Err("audio_cleanup_state_root_invalid");
        }
        let root_metadata =
            fs::symlink_metadata(state_root).map_err(|_| "audio_cleanup_state_root_unavailable")?;
        // SAFETY: geteuid has no pointer arguments or mutable process state.
        let current_uid = unsafe { geteuid() };
        if !root_metadata.is_dir()
            || root_metadata.file_type().is_symlink()
            || root_metadata.uid() != current_uid
            || root_metadata.mode() & 0o077 != 0
        {
            return Err("audio_cleanup_state_root_invalid");
        }

        let directory = state_root.join("audio");
        match fs::create_dir(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .map_err(|_| "audio_cleanup_journal_unavailable")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("audio_cleanup_journal_unavailable"),
        }

        let directory_metadata =
            fs::symlink_metadata(&directory).map_err(|_| "audio_cleanup_journal_unavailable")?;
        if !directory_metadata.is_dir()
            || directory_metadata.file_type().is_symlink()
            || directory_metadata.uid() != root_metadata.uid()
            || directory_metadata.mode() & 0o077 != 0
        {
            return Err("audio_cleanup_journal_invalid_path");
        }

        let journal = Self {
            path: directory.join("cleanup-v1.json"),
            directory,
            owner_uid: root_metadata.uid(),
        };
        journal.read()?;
        Ok(journal)
    }

    pub(super) fn read(&self) -> Result<CleanupState, &'static str> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CleanupState::default());
            }
            Err(_) => return Err("audio_cleanup_journal_unavailable"),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != self.owner_uid
            || metadata.mode() & 0o077 != 0
        {
            return Err("audio_cleanup_journal_invalid_path");
        }
        if metadata.len() > MAX_JOURNAL_BYTES {
            return Err("audio_cleanup_journal_too_large");
        }

        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|_| "audio_cleanup_journal_unavailable")?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "audio_cleanup_journal_unavailable")?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err("audio_cleanup_journal_too_large");
        }
        decode_state(&bytes)
    }

    pub(super) fn write(&self, state: &CleanupState) -> Result<(), &'static str> {
        let bytes = encode_state(state)?;
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let temp_path = self.directory.join(format!(
            ".cleanup-v1.{}.{}.{timestamp:x}.tmp",
            std::process::id(),
            sequence
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|_| "audio_cleanup_journal_unavailable")?;
        let write_result = (|| {
            file.write_all(&bytes)
                .map_err(|_| "audio_cleanup_journal_unavailable")?;
            file.sync_all()
                .map_err(|_| "audio_cleanup_journal_unavailable")?;
            fs::rename(&temp_path, &self.path).map_err(|_| "audio_cleanup_journal_unavailable")?;
            File::open(&self.directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| "audio_cleanup_journal_unavailable")?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

fn encode_state(state: &CleanupState) -> Result<Vec<u8>, &'static str> {
    let sinks = state
        .sinks
        .iter()
        .map(|(name, intent)| {
            (
                name.clone(),
                json!({
                    "owner_token": intent.owner_token,
                    "module_index": intent.module_index,
                    "server_identity": intent.server_identity.as_ref().map(|identity| json!({
                        "address": identity.address,
                        "instance_cookie": identity.instance_cookie,
                    })),
                    "lease_active": intent.lease_active,
                    "cleanup_requested": intent.cleanup_requested,
                    "load_quiesced": intent.load_quiesced,
                    "unload_submitted": intent.unload_submitted,
                    "unload_confirmed": intent.unload_confirmed,
                    "unload_quiesced": intent.unload_quiesced,
                }),
            )
        })
        .collect::<Map<String, Value>>();
    let route = state.route.as_ref().map(|intent| {
        json!({
            "target_sink": intent.target_sink,
            "original_sink": intent.original_sink,
            "assignment_confirmed": intent.assignment_confirmed,
            "assignment_quiesced": intent.assignment_quiesced,
            "restoration_submitted": intent.restoration_submitted,
            "restoration_confirmed": intent.restoration_confirmed,
            "restoration_quiesced": intent.restoration_quiesced,
        })
    });
    let value = json!({
        "version": JOURNAL_VERSION,
        "sinks": sinks,
        "route": route,
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| "audio_cleanup_journal_corrupt")?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err("audio_cleanup_journal_too_large");
    }
    Ok(bytes)
}

fn decode_state(bytes: &[u8]) -> Result<CleanupState, &'static str> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "audio_cleanup_journal_corrupt")?;
    let object = value
        .as_object()
        .filter(|object| object.len() == 3)
        .ok_or("audio_cleanup_journal_corrupt")?;
    if object.get("version").and_then(Value::as_u64) != Some(JOURNAL_VERSION) {
        return Err("audio_cleanup_journal_version_unsupported");
    }
    let sinks_value = object
        .get("sinks")
        .and_then(Value::as_object)
        .ok_or("audio_cleanup_journal_corrupt")?;
    let mut sinks = BTreeMap::new();
    for (name, value) in sinks_value {
        if name.is_empty() || name.len() > 256 {
            return Err("audio_cleanup_journal_corrupt");
        }
        let item = value
            .as_object()
            .filter(|object| match object.len() {
                6 | 8 => !object.contains_key("server_identity"),
                9 => object.contains_key("server_identity"),
                _ => false,
            })
            .ok_or("audio_cleanup_journal_corrupt")?;
        let owner_token = item
            .get("owner_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty() && token.len() <= 256)
            .ok_or("audio_cleanup_journal_corrupt")?
            .to_owned();
        let module_index = match item.get("module_index") {
            Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|index| u32::try_from(index).ok())
                    .ok_or("audio_cleanup_journal_corrupt")?,
            ),
            None => return Err("audio_cleanup_journal_corrupt"),
        };
        let server_identity = match item.get("server_identity") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let identity = value
                    .as_object()
                    .filter(|identity| identity.len() == 2)
                    .ok_or("audio_cleanup_journal_corrupt")?;
                let address = identity
                    .get("address")
                    .and_then(Value::as_str)
                    .filter(|address| !address.is_empty() && address.len() <= 1024)
                    .ok_or("audio_cleanup_journal_corrupt")?
                    .to_owned();
                let instance_cookie = identity
                    .get("instance_cookie")
                    .and_then(Value::as_u64)
                    .and_then(|cookie| u32::try_from(cookie).ok())
                    .ok_or("audio_cleanup_journal_corrupt")?;
                Some(ServerIdentity {
                    address,
                    instance_cookie,
                })
            }
        };
        let unload_submitted = item
            .get("unload_submitted")
            .and_then(Value::as_bool)
            .ok_or("audio_cleanup_journal_corrupt")?;
        let lease_active = item
            .get("lease_active")
            .and_then(Value::as_bool)
            .ok_or("audio_cleanup_journal_corrupt")?;
        let cleanup_requested = item
            .get("cleanup_requested")
            .and_then(Value::as_bool)
            .ok_or("audio_cleanup_journal_corrupt")?;
        let load_quiesced = optional_bool(item, "load_quiesced")?;
        let unload_confirmed = item
            .get("unload_confirmed")
            .and_then(Value::as_bool)
            .ok_or("audio_cleanup_journal_corrupt")?;
        let unload_quiesced = optional_bool(item, "unload_quiesced")?;
        sinks.insert(
            name.clone(),
            SinkIntent {
                owner_token,
                server_identity,
                module_index,
                lease_active,
                cleanup_requested,
                load_quiesced,
                unload_submitted,
                unload_confirmed,
                unload_quiesced,
            },
        );
    }
    let route = match object.get("route") {
        Some(Value::Null) => None,
        Some(value) => {
            let item = value
                .as_object()
                .filter(|object| object.len() == 5 || object.len() == 7)
                .ok_or("audio_cleanup_journal_corrupt")?;
            let target_sink = required_string(item, "target_sink")?;
            let original_sink = required_string(item, "original_sink")?;
            let assignment_confirmed = item
                .get("assignment_confirmed")
                .and_then(Value::as_bool)
                .ok_or("audio_cleanup_journal_corrupt")?;
            let restoration_submitted = item
                .get("restoration_submitted")
                .and_then(Value::as_bool)
                .ok_or("audio_cleanup_journal_corrupt")?;
            let assignment_quiesced = optional_bool(item, "assignment_quiesced")?;
            let restoration_confirmed = item
                .get("restoration_confirmed")
                .and_then(Value::as_bool)
                .ok_or("audio_cleanup_journal_corrupt")?;
            let restoration_quiesced = optional_bool(item, "restoration_quiesced")?;
            Some(RouteIntent {
                target_sink,
                original_sink,
                assignment_confirmed,
                assignment_quiesced,
                restoration_submitted,
                restoration_confirmed,
                restoration_quiesced,
            })
        }
        None => return Err("audio_cleanup_journal_corrupt"),
    };
    Ok(CleanupState { sinks, route })
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, &'static str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .map(str::to_owned)
        .ok_or("audio_cleanup_journal_corrupt")
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<bool, &'static str> {
    match object.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        None => Ok(false),
        _ => Err("audio_cleanup_journal_corrupt"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-audio-{label}-{}-{}-{timestamp:x}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed),
            timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn journal_refuses_a_host_root_visible_to_other_users() {
        let root = temporary_root("public-root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            CleanupJournal::open(&root),
            Err("audio_cleanup_state_root_invalid")
        ));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indeterminate_load_tombstone_survives_absent_query_and_restart() {
        let root = temporary_root("restart");
        let journal = CleanupJournal::open(&root).unwrap();
        let mut state = CleanupState::default();
        // The load callback timed out. The sink list can briefly report absent
        // while the server still processes the request, so absence is not a
        // cleanup acknowledgment.
        state.sinks.insert(
            "amaterasu_stream_1_2ch_1".into(),
            SinkIntent {
                owner_token: "owner-1".into(),
                module_index: None,
                lease_active: false,
                cleanup_requested: true,
                ..SinkIntent::default()
            },
        );
        journal.write(&state).unwrap();
        assert!(state.sinks.contains_key("amaterasu_stream_1_2ch_1"));

        let restarted = CleanupJournal::open(&root).unwrap();
        assert_eq!(restarted.read().unwrap(), state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sink_tombstone_retires_only_after_same_connection_fence_and_capacity_is_bounded() {
        let mut intent = SinkIntent {
            owner_token: "delayed-load".into(),
            module_index: None,
            cleanup_requested: true,
            ..SinkIntent::default()
        };
        // An immediate absent query may precede server-side execution.
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Absent),
            SinkCleanupAction::KeepTombstone
        );
        intent.load_quiesced = true;
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Absent),
            SinkCleanupAction::ClearConfirmedAbsent
        );

        let mut unresolved = CleanupState::default();
        unresolved.sinks.insert(
            "indeterminate-attempt".into(),
            SinkIntent {
                module_index: None,
                cleanup_requested: true,
                load_quiesced: false,
                ..intent.clone()
            },
        );
        assert!(has_unquiesced_sink_load(&unresolved));
        assert!(!can_start_sink_load(&unresolved, "another-load"));

        let mut full = CleanupState::default();
        for index in 0..MAX_SINK_INTENTS {
            full.sinks.insert(
                format!("amaterasu_stream_{index}"),
                SinkIntent {
                    owner_token: format!("owner-{index}"),
                    ..SinkIntent::default()
                },
            );
        }
        assert!(!can_add_sink_intent(&full, "one-more"));
        assert!(can_add_sink_intent(&full, "amaterasu_stream_0"));
        full.sinks.remove("amaterasu_stream_0");
        assert!(can_add_sink_intent(&full, "one-more"));
    }

    #[test]
    fn disconnect_during_cleanup_and_shutdown_preserve_route_and_sink_for_restart() {
        let root = temporary_root("disconnect-shutdown");
        let journal = CleanupJournal::open(&root).unwrap();
        let mut state = CleanupState::default();
        state.sinks.insert(
            "amaterasu_stream_7_2ch_3".into(),
            SinkIntent {
                owner_token: "7-3".into(),
                module_index: Some(91),
                lease_active: false,
                cleanup_requested: true,
                unload_submitted: true,
                unload_confirmed: false,
                ..SinkIntent::default()
            },
        );
        state.route = Some(RouteIntent {
            target_sink: "amaterasu_stream_7_2ch_3".into(),
            original_sink: "user-default".into(),
            assignment_confirmed: false,
            restoration_submitted: false,
            restoration_confirmed: false,
            ..RouteIntent::default()
        });
        // A disconnected server makes the bounded shutdown pass a no-op; the
        // on-disk intent is the handoff to the next module start.
        journal.write(&state).unwrap();
        let restarted = CleanupJournal::open(&root).unwrap();
        let recovered = restarted.read().unwrap();
        assert_eq!(recovered, state);
        assert_eq!(
            sink_cleanup_action(
                recovered.sinks.get("amaterasu_stream_7_2ch_3").unwrap(),
                SinkObservation::Absent
            ),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            route_restore_action(recovered.route.as_ref().unwrap(), Some("user-default")),
            RouteRestoreAction::KeepTombstone
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_owner_token_is_required_to_reconcile_same_named_sink() {
        let intent = SinkIntent {
            owner_token: "our-operation".into(),
            module_index: None,
            lease_active: false,
            cleanup_requested: true,
            ..SinkIntent::default()
        };
        assert!(sink_matches_intent("our-operation", &intent, 17));
        assert!(!sink_matches_intent(
            "somebody-elses-operation",
            &intent,
            18
        ));
    }

    #[test]
    fn active_stream_sink_is_not_reaped_by_the_periodic_cleanup_worker() {
        let active = SinkIntent {
            owner_token: "active-stream".into(),
            module_index: Some(31),
            lease_active: true,
            cleanup_requested: false,
            ..SinkIntent::default()
        };
        assert!(!sink_cleanup_requested(&active));

        let dropped = SinkIntent {
            lease_active: false,
            cleanup_requested: true,
            ..active
        };
        assert!(sink_cleanup_requested(&dropped));
    }

    #[test]
    fn restart_turns_crashed_process_leases_into_cleanup_work() {
        let mut state = CleanupState::default();
        state.sinks.insert(
            "active-before-crash".into(),
            SinkIntent {
                owner_token: "token".into(),
                module_index: Some(3),
                lease_active: true,
                cleanup_requested: false,
                ..SinkIntent::default()
            },
        );
        deactivate_prior_process_sinks(&mut state);
        assert!(sink_cleanup_requested(
            state.sinks.get("active-before-crash").unwrap()
        ));
    }

    #[test]
    fn timed_out_load_survives_absent_query_then_reaps_only_delayed_owned_sink() {
        let intent = SinkIntent {
            owner_token: "owner-1".into(),
            module_index: None,
            lease_active: false,
            cleanup_requested: true,
            ..SinkIntent::default()
        };
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Absent),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            sink_cleanup_action(
                &intent,
                SinkObservation::Owned {
                    owner_token: "different-load",
                    module_index: 71,
                }
            ),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            sink_cleanup_action(
                &intent,
                SinkObservation::Owned {
                    owner_token: "owner-1",
                    module_index: 72,
                }
            ),
            SinkCleanupAction::Unload(72)
        );
    }

    #[test]
    fn known_module_absence_is_releasable_but_foreign_sink_is_not() {
        let intent = SinkIntent {
            owner_token: "owner-1".into(),
            module_index: Some(71),
            lease_active: false,
            cleanup_requested: true,
            load_quiesced: true,
            ..SinkIntent::default()
        };
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Absent),
            SinkCleanupAction::ClearConfirmedAbsent
        );
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Foreign),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            sink_cleanup_action(
                &intent,
                SinkObservation::Owned {
                    owner_token: "owner-1",
                    module_index: 72,
                }
            ),
            SinkCleanupAction::KeepTombstone
        );
        let uncertain_unload = SinkIntent {
            unload_submitted: true,
            ..intent.clone()
        };
        assert_eq!(
            sink_cleanup_action(&uncertain_unload, SinkObservation::Absent),
            SinkCleanupAction::KeepTombstone
        );
        let confirmed_unload = SinkIntent {
            unload_confirmed: true,
            ..uncertain_unload
        };
        assert_eq!(
            sink_cleanup_action(&confirmed_unload, SinkObservation::Absent),
            SinkCleanupAction::ClearConfirmedAbsent
        );
        let fenced_unload = SinkIntent {
            unload_confirmed: false,
            unload_quiesced: true,
            ..confirmed_unload
        };
        assert_eq!(
            sink_cleanup_action(&fenced_unload, SinkObservation::Absent),
            SinkCleanupAction::ClearConfirmedAbsent
        );
    }

    #[test]
    fn crashed_unload_retires_only_after_same_server_native_module_absence() {
        let server_identity = ServerIdentity {
            address: "unix:/private/pulse/native".into(),
            instance_cookie: 19,
        };
        let intent = SinkIntent {
            owner_token: "owner-1".into(),
            server_identity: Some(server_identity.clone()),
            module_index: Some(71),
            lease_active: false,
            cleanup_requested: true,
            load_quiesced: true,
            unload_submitted: true,
            ..SinkIntent::default()
        };
        // A crash loses the successful callback before the journal records it.
        assert_eq!(
            sink_cleanup_action(&intent, SinkObservation::Absent),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            sink_cleanup_action(
                &intent,
                SinkObservation::SinkAndModuleAbsent {
                    server_identity: &server_identity,
                    module_index: 71
                }
            ),
            SinkCleanupAction::ClearConfirmedAbsent
        );
    }

    #[test]
    fn native_absence_proof_refuses_wrong_server_index_or_unfinished_load() {
        let server = ServerIdentity {
            address: "unix:/private/pulse/native".into(),
            instance_cookie: 19,
        };
        let known = SinkIntent {
            owner_token: "owner-1".into(),
            server_identity: Some(server.clone()),
            module_index: Some(71),
            load_quiesced: true,
            cleanup_requested: true,
            unload_submitted: true,
            ..SinkIntent::default()
        };
        for changed_server in [
            ServerIdentity {
                instance_cookie: 20,
                ..server.clone()
            },
            ServerIdentity {
                address: "unix:/another/pulse/native".into(),
                ..server.clone()
            },
        ] {
            assert_eq!(
                sink_cleanup_action(
                    &known,
                    SinkObservation::SinkAndModuleAbsent {
                        server_identity: &changed_server,
                        module_index: 71
                    }
                ),
                SinkCleanupAction::KeepTombstone
            );
        }
        assert_eq!(
            sink_cleanup_action(
                &known,
                SinkObservation::SinkAndModuleAbsent {
                    server_identity: &server,
                    module_index: 72
                }
            ),
            SinkCleanupAction::KeepTombstone
        );
        for unsafe_intent in [
            SinkIntent {
                server_identity: None,
                ..known.clone()
            },
            SinkIntent {
                module_index: None,
                ..known.clone()
            },
            SinkIntent {
                load_quiesced: false,
                ..known.clone()
            },
            SinkIntent {
                lease_active: true,
                ..known.clone()
            },
            SinkIntent {
                cleanup_requested: false,
                ..known.clone()
            },
        ] {
            assert_eq!(
                sink_cleanup_action(
                    &unsafe_intent,
                    SinkObservation::SinkAndModuleAbsent {
                        server_identity: &server,
                        module_index: 71
                    }
                ),
                SinkCleanupAction::KeepTombstone
            );
        }
        assert_eq!(
            sink_cleanup_action(&known, SinkObservation::Foreign),
            SinkCleanupAction::KeepTombstone
        );
        assert_eq!(
            sink_cleanup_action(
                &known,
                SinkObservation::Owned {
                    owner_token: "foreign-owner",
                    module_index: 71
                }
            ),
            SinkCleanupAction::KeepTombstone
        );
    }

    #[test]
    fn server_identity_roundtrips_and_legacy_journals_remain_without_identity() {
        let server = ServerIdentity {
            address: "unix:/private/pulse/native".into(),
            instance_cookie: 19,
        };
        let state = CleanupState {
            sinks: BTreeMap::from([(
                "owned-sink".into(),
                SinkIntent {
                    owner_token: "owner-1".into(),
                    server_identity: Some(server),
                    module_index: Some(71),
                    load_quiesced: true,
                    cleanup_requested: true,
                    unload_submitted: true,
                    ..SinkIntent::default()
                },
            )]),
            route: None,
        };
        let encoded = encode_state(&state).unwrap();
        assert_eq!(decode_state(&encoded).unwrap(), state);
        let mut legacy: Value = serde_json::from_slice(&encoded).unwrap();
        legacy["sinks"]["owned-sink"]
            .as_object_mut()
            .unwrap()
            .remove("server_identity");
        let decoded = decode_state(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(decoded.sinks["owned-sink"].server_identity.is_none());
        assert_eq!(
            sink_cleanup_action(&decoded.sinks["owned-sink"], SinkObservation::Absent),
            SinkCleanupAction::KeepTombstone
        );
        for invalid_identity in [
            json!({"address":"", "instance_cookie":19}),
            json!({"address":"x".repeat(1025), "instance_cookie":19}),
            json!({"address":"unix:/private/pulse/native", "instance_cookie":u64::from(u32::MAX)+1}),
            json!({"address":"unix:/private/pulse/native"}),
        ] {
            legacy["sinks"]["owned-sink"]["server_identity"] = invalid_identity;
            assert!(decode_state(&serde_json::to_vec(&legacy).unwrap()).is_err());
        }
    }

    #[test]
    fn every_identity_bearing_absence_path_requires_the_original_server() {
        let server = ServerIdentity {
            address: "unix:/private/pulse/native".into(),
            instance_cookie: 19,
        };
        let known = SinkIntent {
            owner_token: "owner-1".into(),
            server_identity: Some(server.clone()),
            module_index: Some(71),
            load_quiesced: true,
            cleanup_requested: true,
            ..SinkIntent::default()
        };
        for intent in [
            known.clone(),
            SinkIntent {
                unload_submitted: true,
                unload_confirmed: true,
                ..known.clone()
            },
        ] {
            assert_eq!(
                sink_cleanup_action(&intent, SinkObservation::Absent),
                SinkCleanupAction::ClearConfirmedAbsent
            );
            for other_server in [
                ServerIdentity {
                    instance_cookie: 20,
                    ..server.clone()
                },
                ServerIdentity {
                    address: "unix:/another/pulse/native".into(),
                    ..server.clone()
                },
            ] {
                assert_eq!(
                    sink_cleanup_action_on_server(
                        &intent,
                        Some(&other_server),
                        SinkObservation::Absent
                    ),
                    SinkCleanupAction::KeepTombstone
                );
                assert_eq!(
                    sink_cleanup_action_on_server(
                        &intent,
                        Some(&other_server),
                        SinkObservation::Owned {
                            owner_token: "owner-1",
                            module_index: 71
                        }
                    ),
                    SinkCleanupAction::KeepTombstone
                );
            }
            assert_eq!(
                sink_cleanup_action_on_server(&intent, None, SinkObservation::Absent),
                SinkCleanupAction::KeepTombstone
            );
            assert_eq!(
                sink_cleanup_action_on_server(&intent, Some(&server), SinkObservation::Absent),
                SinkCleanupAction::ClearConfirmedAbsent
            );
        }
    }

    #[test]
    fn reconnect_cannot_replace_outstanding_sink_identity_on_another_server() {
        let server = ServerIdentity {
            address: "unix:/private/pulse/native".into(),
            instance_cookie: 19,
        };
        let existing = SinkIntent {
            owner_token: "owner-1".into(),
            server_identity: Some(server.clone()),
            module_index: Some(71),
            load_quiesced: true,
            lease_active: true,
            ..SinkIntent::default()
        };
        for reconnect_server in [
            ServerIdentity {
                instance_cookie: 20,
                ..server.clone()
            },
            ServerIdentity {
                address: "unix:/another/pulse/native".into(),
                ..server.clone()
            },
        ] {
            assert_eq!(
                validate_sink_intent_replacement(
                    Some(&existing),
                    "owner-1",
                    Some(&reconnect_server)
                ),
                Err("audio_cleanup_journal_conflict")
            );
        }
        assert!(validate_sink_intent_replacement(Some(&existing), "owner-1", None).is_err());
        assert!(
            validate_sink_intent_replacement(Some(&existing), "foreign-owner", Some(&server))
                .is_err()
        );
        assert!(
            validate_sink_intent_replacement(Some(&existing), "owner-1", Some(&server)).is_ok()
        );
        assert!(validate_sink_intent_replacement(None, "owner-1", Some(&server)).is_ok());
        assert_eq!(existing.server_identity, Some(server));
        assert_eq!(existing.module_index, Some(71));
    }

    #[test]
    fn route_restore_is_only_authorized_while_current_default_is_our_target() {
        let route = RouteIntent {
            target_sink: "amaterasu-stream".into(),
            original_sink: "user-default".into(),
            assignment_confirmed: true,
            restoration_submitted: false,
            restoration_confirmed: false,
            ..RouteIntent::default()
        };
        assert_eq!(
            route_restore_action(&route, Some("amaterasu-stream")),
            RouteRestoreAction::RestoreOriginal
        );
        let indeterminate_on_target = RouteIntent {
            assignment_confirmed: false,
            ..route.clone()
        };
        assert_eq!(
            route_restore_action(&indeterminate_on_target, Some("amaterasu-stream")),
            RouteRestoreAction::KeepTombstone
        );
        let assignment_fenced_on_target = RouteIntent {
            assignment_quiesced: true,
            ..indeterminate_on_target.clone()
        };
        assert_eq!(
            route_restore_action(&assignment_fenced_on_target, Some("amaterasu-stream")),
            RouteRestoreAction::KeepTombstone
        );
        assert_eq!(
            route_restore_action(&route, Some("new-user-choice")),
            RouteRestoreAction::Release
        );
        assert_eq!(route_restore_action(&route, None), RouteRestoreAction::Wait);

        let indeterminate = RouteIntent {
            assignment_confirmed: false,
            ..route.clone()
        };
        assert_eq!(
            route_restore_action(&indeterminate, Some("new-user-choice")),
            RouteRestoreAction::KeepTombstone
        );
        let assignment_fenced_without_apply = RouteIntent {
            assignment_quiesced: true,
            ..indeterminate
        };
        assert_eq!(
            route_restore_action(&assignment_fenced_without_apply, Some("user-default")),
            RouteRestoreAction::KeepTombstone
        );
        assert_eq!(
            route_restore_action(&assignment_fenced_without_apply, Some("new-user-choice")),
            RouteRestoreAction::KeepTombstone
        );
        let uncertain_restore = RouteIntent {
            restoration_submitted: true,
            restoration_confirmed: false,
            ..route
        };
        assert_eq!(
            route_restore_action(&uncertain_restore, Some("new-user-choice")),
            RouteRestoreAction::KeepTombstone
        );
        assert_eq!(
            route_restore_action(&uncertain_restore, Some("amaterasu-stream")),
            RouteRestoreAction::KeepTombstone
        );
        assert_eq!(
            route_restore_action(&uncertain_restore, Some("user-default")),
            RouteRestoreAction::KeepTombstone
        );
        let restoration_fenced = RouteIntent {
            restoration_quiesced: true,
            ..uncertain_restore.clone()
        };
        assert_eq!(
            route_restore_action(&restoration_fenced, Some("new-user-choice")),
            RouteRestoreAction::KeepTombstone
        );
    }

    #[test]
    fn delayed_route_commands_keep_their_persisted_owner_after_an_unproven_fence() {
        struct Server {
            default: &'static str,
            pending: Option<&'static str>,
        }

        impl Server {
            fn submit_without_reply(&mut self, target: &'static str) {
                assert!(self.pending.replace(target).is_none());
            }

            fn finish_prior_command(&mut self) -> &'static str {
                self.default = self.pending.take().expect("a command is still pending");
                self.default
            }
        }

        let root = temporary_root("late-route-commands");
        let journal = CleanupJournal::open(&root).unwrap();
        let mut state = CleanupState {
            route: Some(RouteIntent {
                target_sink: "stream".into(),
                original_sink: "headset".into(),
                ..RouteIntent::default()
            }),
            ..CleanupState::default()
        };
        let mut server = Server {
            default: "headset",
            pending: None,
        };
        journal.write(&state).unwrap();
        server.submit_without_reply("stream");
        server.default = "user-choice";
        // PipeWire can answer an ordered server query before publishing the
        // preceding route metadata. A fence alone is therefore insufficient.
        state.route.as_mut().unwrap().assignment_quiesced = true;
        journal.write(&state).unwrap();

        // Neither a different current default nor a restarted journal proves
        // that the earlier connection's queued assignment cannot still run.
        state = journal.read().unwrap();
        assert_eq!(
            route_restore_action(state.route.as_ref().unwrap(), Some(server.default)),
            RouteRestoreAction::KeepTombstone
        );
        assert!(validate_route_intent_replacement(state.route.as_ref()).is_err());
        let assignment_reply = server.finish_prior_command();
        assert_eq!(server.default, "stream");
        assert_eq!(
            route_restore_action(state.route.as_ref().unwrap(), Some(server.default)),
            RouteRestoreAction::KeepTombstone
        );

        // A successful assignment followed by an unacknowledged restoration
        // has the same hazard. A fresh read of the user's choice is no fence.
        assert_eq!(assignment_reply, "stream");
        let route = state.route.as_mut().unwrap();
        route.assignment_confirmed = true;
        route.restoration_submitted = true;
        journal.write(&state).unwrap();
        server.submit_without_reply("headset");
        server.default = "user-choice";
        state = CleanupJournal::open(&root).unwrap().read().unwrap();
        assert_eq!(
            route_restore_action(state.route.as_ref().unwrap(), Some(server.default)),
            RouteRestoreAction::KeepTombstone
        );
        assert!(validate_route_intent_replacement(state.route.as_ref()).is_err());
        state.route.as_mut().unwrap().restoration_quiesced = true;
        assert_eq!(
            route_restore_action(state.route.as_ref().unwrap(), Some(server.default)),
            RouteRestoreAction::KeepTombstone
        );
        assert_eq!(server.finish_prior_command(), "headset");
        let route = state.route.as_mut().unwrap();
        route.restoration_confirmed = true;
        assert_eq!(server.default, "headset");
        server.default = "user-choice";
        assert_eq!(
            route_restore_action(state.route.as_ref().unwrap(), Some(server.default)),
            RouteRestoreAction::Release
        );
        state.route = None;
        journal.write(&state).unwrap();
        assert!(validate_route_intent_replacement(journal.read().unwrap().route.as_ref()).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RouteRestoreAction {
    Wait,
    KeepTombstone,
    Release,
    RestoreOriginal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SinkObservation<'a> {
    Absent,
    /// A complete native inventory on the identified server found neither the
    /// owned sink nor its known module. This is resource evidence, not an
    /// acknowledgment of any command sent on a previous connection.
    SinkAndModuleAbsent {
        server_identity: &'a ServerIdentity,
        module_index: u32,
    },
    Foreign,
    Owned {
        owner_token: &'a str,
        module_index: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SinkCleanupAction {
    KeepTombstone,
    ClearConfirmedAbsent,
    Unload(u32),
}

pub(super) fn validate_sink_intent_replacement(
    existing: Option<&SinkIntent>,
    owner_token: &str,
    server_identity: Option<&ServerIdentity>,
) -> Result<(), &'static str> {
    if let Some(existing) = existing
        && (existing.owner_token != owner_token
            || existing
                .server_identity
                .as_ref()
                .is_some_and(|expected| server_identity != Some(expected)))
    {
        // Reconnecting to another server cannot replace an outstanding native
        // allocation's ownership merely because that server lacks its sink.
        return Err("audio_cleanup_journal_conflict");
    }
    Ok(())
}

pub(super) fn can_add_sink_intent(state: &CleanupState, name: &str) -> bool {
    state.sinks.contains_key(name) || state.sinks.len() < MAX_SINK_INTENTS
}

pub(super) fn has_unquiesced_sink_load(state: &CleanupState) -> bool {
    state
        .sinks
        .values()
        .any(|intent| intent.module_index.is_none() && !intent.load_quiesced)
}

pub(super) fn can_start_sink_load(state: &CleanupState, name: &str) -> bool {
    can_add_sink_intent(state, name) && !has_unquiesced_sink_load(state)
}

/// A route is global to the Pulse server. Even an apparently superseded
/// intent must be reconciled before another assignment can own its journal
/// slot: an older, unfenced command may still change the default later.
pub(super) fn validate_route_intent_replacement(
    existing: Option<&RouteIntent>,
) -> Result<(), &'static str> {
    if existing.is_some() {
        Err("audio_route_restore_pending")
    } else {
        Ok(())
    }
}

pub(super) fn route_restore_action(
    intent: &RouteIntent,
    current_default: Option<&str>,
) -> RouteRestoreAction {
    // PipeWire can fence a request before its route metadata is published.
    // A fence or a fresh read is not a successful callback.
    if !intent.assignment_confirmed {
        return RouteRestoreAction::KeepTombstone;
    }
    if intent.restoration_submitted && !intent.restoration_confirmed {
        return RouteRestoreAction::KeepTombstone;
    }
    if intent.target_sink == intent.original_sink {
        return RouteRestoreAction::Release;
    }
    match current_default {
        None => RouteRestoreAction::Wait,
        Some(current)
            if current == intent.original_sink
                && intent.restoration_submitted
                && intent.restoration_confirmed =>
        {
            RouteRestoreAction::Release
        }
        Some(current) if current == intent.target_sink => RouteRestoreAction::RestoreOriginal,
        // A later user choice supersedes our settled cleanup. Never retry over
        // that choice.
        Some(_) => RouteRestoreAction::Release,
    }
}

#[cfg(test)]
pub(super) fn sink_matches_intent(token: &str, intent: &SinkIntent, _module_index: u32) -> bool {
    token == intent.owner_token
}

pub(super) fn sink_cleanup_action_on_server(
    intent: &SinkIntent,
    server_identity: Option<&ServerIdentity>,
    observation: SinkObservation<'_>,
) -> SinkCleanupAction {
    if intent
        .server_identity
        .as_ref()
        .is_some_and(|expected| server_identity != Some(expected))
    {
        return SinkCleanupAction::KeepTombstone;
    }
    sink_cleanup_action(intent, observation)
}

pub(super) fn sink_cleanup_action(
    intent: &SinkIntent,
    observation: SinkObservation<'_>,
) -> SinkCleanupAction {
    match observation {
        SinkObservation::SinkAndModuleAbsent {
            server_identity,
            module_index,
        } if intent.server_identity.as_ref() == Some(server_identity)
            && intent.module_index == Some(module_index)
            && intent.load_quiesced
            && sink_cleanup_requested(intent) =>
        {
            // The original load is complete, so it cannot create a resource
            // after this observation. The same server instance has neither the
            // sink nor its module: no unload callback is needed to prove that
            // this exact native allocation has retired.
            SinkCleanupAction::ClearConfirmedAbsent
        }
        SinkObservation::Absent
            if intent.module_index.is_some()
                && (!intent.unload_submitted
                    || intent.unload_confirmed
                    || intent.unload_quiesced) =>
        {
            SinkCleanupAction::ClearConfirmedAbsent
        }
        SinkObservation::Absent if intent.module_index.is_none() && intent.load_quiesced => {
            SinkCleanupAction::ClearConfirmedAbsent
        }
        SinkObservation::Absent
        | SinkObservation::Foreign
        | SinkObservation::SinkAndModuleAbsent { .. } => SinkCleanupAction::KeepTombstone,
        SinkObservation::Owned {
            owner_token,
            module_index,
        } if owner_token == intent.owner_token
            && intent
                .module_index
                .is_none_or(|expected| expected == module_index) =>
        {
            SinkCleanupAction::Unload(module_index)
        }
        SinkObservation::Owned { .. } => SinkCleanupAction::KeepTombstone,
    }
}

pub(super) fn sink_cleanup_requested(intent: &SinkIntent) -> bool {
    intent.cleanup_requested && !intent.lease_active
}

pub(super) fn deactivate_prior_process_sinks(state: &mut CleanupState) {
    for intent in state.sinks.values_mut() {
        intent.lease_active = false;
        intent.cleanup_requested = true;
    }
}
