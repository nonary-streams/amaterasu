//! Bounded session log output for accepted diagnostics records.
use crate::protocol::Record;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_LIMIT: usize = 30;
const ROLLOVER_SIZE: u64 = 2 * 1024 * 1024;
const SESSION_SIZE: u64 = 10 * 1024 * 1024;
const ROLLOVER_COUNT: usize = (SESSION_SIZE / ROLLOVER_SIZE) as usize - 1;
const LOG_SUFFIX: &str = ".log";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    #[serde(default)]
    min_log_level: Option<u8>,
    #[serde(default)]
    log_path: Option<String>,
    #[serde(default)]
    state_root: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct LoggingSettings {
    pub(crate) minimum_level: u8,
    pub(crate) log_path: Option<PathBuf>,
    pub(crate) state_root: Option<PathBuf>,
}

impl LoggingSettings {
    pub(crate) fn from_value(value: serde_json::Value) -> Result<Self, String> {
        let raw: RawSettings = serde_json::from_value(value)
            .map_err(|error| format!("invalid host logging settings: {error}"))?;
        let minimum_level = raw
            .min_log_level
            .unwrap_or_else(|| default_minimum_level(env!("CARGO_PKG_VERSION_PRE")));
        if minimum_level > 6 {
            return Err("host minimum log level must be between 0 and 6".into());
        }
        let log_path = raw
            .log_path
            .map(|path| validate_configured_path(&path).map(|()| PathBuf::from(path)))
            .transpose()?;
        let state_root = raw
            .state_root
            .map(|path| validate_state_root(Path::new(&path)).map_err(|error| error.to_string()))
            .transpose()?;
        if log_path.is_some() && state_root.is_none() {
            return Err("host logging settings are missing the trusted state root".into());
        }
        Ok(Self {
            minimum_level,
            log_path,
            state_root,
        })
    }

    pub(crate) fn from_host_configuration(
        payload: &Value,
        state_root: Option<&Path>,
    ) -> Result<Self, String> {
        if payload.get("ok").and_then(Value::as_bool) != Some(true)
            || payload.get("ready").and_then(Value::as_bool) != Some(true)
            || payload.get("version").and_then(Value::as_u64) != Some(1)
            || payload.get("revision").and_then(Value::as_u64).unwrap_or(0) == 0
        {
            return Err("host configuration did not return a persisted v1 document".into());
        }

        let values = payload
            .get("values")
            .and_then(Value::as_object)
            .ok_or("host configuration values are not an object")?;
        let log_path = values
            .get("log_path")
            .and_then(Value::as_str)
            .ok_or("host configuration omitted log_path")?;
        let effective = payload
            .get("effective")
            .and_then(Value::as_object)
            .ok_or("host configuration effective values are not an object")?;
        let minimum_level = match effective
            .get("min_log_level")
            .and_then(Value::as_str)
            .ok_or("host configuration omitted effective min_log_level")?
        {
            "verbose" => 0,
            "debug" => 1,
            "info" => 2,
            "warning" => 3,
            "error" => 4,
            "fatal" => 5,
            "none" => 6,
            _ => return Err("host configuration returned an unknown minimum log level".into()),
        };
        let state_root = state_root
            .ok_or("diagnostics settings are missing the trusted state root")?
            .to_string_lossy();

        Self::from_value(json!({
            "min_log_level":minimum_level,
            "log_path":log_path,
            "state_root":state_root,
        }))
    }
}

fn default_minimum_level(prerelease: &str) -> u8 {
    if is_prerelease_channel(prerelease) {
        1
    } else {
        2
    }
}

fn is_prerelease_channel(prerelease: &str) -> bool {
    let first_identifier = prerelease.split(['.', '+']).next().unwrap_or_default();
    !first_identifier.is_empty() && !first_identifier.eq_ignore_ascii_case("stable")
}

#[derive(Default)]
pub(crate) struct SessionLogger {
    minimum_level: u8,
    settings: Option<LoggingSettings>,
    session: Option<SessionLog>,
    failure: Option<SinkFailure>,
}

#[derive(Clone, Copy)]
enum SinkFailure {
    Open,
    Write,
}

impl SessionLogger {
    pub(crate) fn open(settings: LoggingSettings) -> (Self, Option<String>) {
        let minimum_level = settings.minimum_level;
        let logging_enabled = settings.log_path.is_some();
        let mut logger = Self {
            minimum_level,
            settings: logging_enabled.then_some(settings),
            session: None,
            failure: None,
        };
        if let Some(session) = logger.open_configured_session() {
            match session {
                Ok(session) => logger.session = Some(session),
                Err(error) => {
                    logger.failure = Some(SinkFailure::Open);
                    return (logger, Some(error.to_string()));
                }
            }
        }
        (logger, None)
    }

    fn open_configured_session(&self) -> Option<io::Result<SessionLog>> {
        let settings = self.settings.as_ref()?;
        let log_path = settings.log_path.as_deref()?;
        let state_root = settings.state_root.as_deref()?;
        Some(SessionLog::new(log_path, state_root))
    }

    pub(crate) fn health(&self) -> Value {
        let state = if self.settings.is_none() {
            "disabled"
        } else if self.failure.is_some() {
            "failed"
        } else {
            "healthy"
        };
        let failure = match self.failure {
            Some(SinkFailure::Open) => Some("open"),
            Some(SinkFailure::Write) => Some("write"),
            None => None,
        };
        json!({"state":state,"failure":failure})
    }

    pub(crate) fn write_record(&mut self, caller: &str, record: &Record) {
        if record.level.numeric() < self.minimum_level {
            return;
        }
        if self.session.is_none()
            && let Some(session) = self.open_configured_session()
        {
            match session {
                Ok(session) => self.session = Some(session),
                Err(_) => {
                    self.failure = Some(SinkFailure::Open);
                    return;
                }
            }
        }
        if let Some(session) = &mut self.session {
            match session.write_record(caller, record) {
                Ok(()) => self.failure = None,
                Err(_) => {
                    // Preserve the in-memory caller journal if a disk sink stops working.
                    self.session = None;
                    self.failure = Some(SinkFailure::Write);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn close_sink_for_test(&mut self) {
        self.session.as_mut().unwrap().file.take();
    }

    #[cfg(test)]
    pub(crate) fn active_path_for_test(&self) -> &Path {
        &self.session.as_ref().unwrap().active_path
    }
}

struct SessionLog {
    root: PathBuf,
    label: String,
    active_path: PathBuf,
    file: Option<File>,
    bytes_written: u64,
    rollover_size: u64,
    rollover_count: usize,
}

impl SessionLog {
    fn new(configured_path: &Path, state_root: &Path) -> io::Result<Self> {
        Self::with_limits(configured_path, state_root, ROLLOVER_SIZE, ROLLOVER_COUNT)
    }

    fn with_limits(
        configured_path: &Path,
        state_root: &Path,
        rollover_size: u64,
        rollover_count: usize,
    ) -> io::Result<Self> {
        let state_root = validate_state_root(state_root)?;
        let is_relative = !configured_path.is_absolute();
        let configured_path = if is_relative {
            validate_relative_path(configured_path)?;
            check_existing_path_has_no_symlinks(&state_root, configured_path)?;
            state_root.join(configured_path)
        } else {
            configured_path.to_path_buf()
        };
        let (root, base_name) = resolve_log_root(&configured_path)?;
        fs::create_dir_all(&root)?;
        let canonical_root = root.canonicalize()?;
        if is_relative && !canonical_root.starts_with(&state_root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "relative log path escapes the trusted state root",
            ));
        }
        if !fs::symlink_metadata(&canonical_root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log root is not a directory",
            ));
        }

        purge_old_sessions(&canonical_root, &base_name)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let process_id = std::process::id();
        let (label, file) = (0..100)
            .find_map(|attempt| {
                let label = format!("{base_name}-{timestamp:020}-{process_id:010}-{attempt:02}");
                let active_path = canonical_root.join(format!("{label}{LOG_SUFFIX}"));
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&active_path)
                {
                    Ok(file) => Some(Ok((label, file))),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::AlreadyExists, "session log name collision")
            })?;
        let active_path = canonical_root.join(format!("{label}{LOG_SUFFIX}"));

        Ok(Self {
            root: canonical_root,
            label,
            active_path,
            file: Some(file),
            bytes_written: 0,
            rollover_size: rollover_size.max(1),
            rollover_count,
        })
    }

    fn write_record(&mut self, caller: &str, record: &Record) -> io::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut line = serde_json::to_vec(&json!({
            "timestamp_unix_ms": timestamp,
            "level": record.level.as_str(),
            "caller": caller,
            "target": record.target,
            "message": record.message,
            "fields": record.fields,
        }))
        .map_err(io::Error::other)?;
        line.push(b'\n');
        self.write_line(&line)
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.bytes_written > 0
            && self.bytes_written.saturating_add(line.len() as u64) > self.rollover_size
        {
            self.rotate()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("session log is unavailable"))?;
        file.write_all(line)?;
        file.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(line.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        if self.rollover_count == 0 {
            fs::remove_file(&self.active_path)?;
        } else {
            let oldest = rollover_path(&self.root, &self.label, self.rollover_count);
            remove_if_present(&oldest)?;
            for index in (1..self.rollover_count).rev() {
                let previous = rollover_path(&self.root, &self.label, index);
                let next = rollover_path(&self.root, &self.label, index + 1);
                if previous.exists() {
                    fs::rename(previous, next)?;
                }
            }
            fs::rename(&self.active_path, rollover_path(&self.root, &self.label, 1))?;
        }
        self.file = Some(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.active_path)?,
        );
        self.bytes_written = 0;
        Ok(())
    }
}

fn resolve_log_root(configured_path: &Path) -> io::Result<(PathBuf, String)> {
    let base_name = configured_path
        .file_stem()
        .filter(|_| configured_path.extension().is_some())
        .or_else(|| configured_path.file_name())
        .and_then(|name| name.to_str())
        .map(safe_base_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "amaterasu".into());
    let metadata = fs::metadata(configured_path);
    if matches!(metadata, Ok(ref metadata) if metadata.is_dir()) {
        return Ok((configured_path.to_path_buf(), base_name));
    }
    if configured_path.extension().is_some() {
        let parent = configured_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty());
        return Ok((
            parent.unwrap_or_else(|| Path::new(".")).join("logs"),
            base_name,
        ));
    }
    Ok((configured_path.to_path_buf(), base_name))
}

fn safe_base_name(name: &str) -> String {
    let normalized: String = name
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if normalized == name {
        return normalized;
    }

    // An escaped prefix cannot overlap a plain filename's prefix. Include the
    // original bytes so sanitization and truncation do not merge log policies.
    let hash = name.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("~{normalized}-{hash:016x}")
}

fn purge_old_sessions(root: &Path, base_name: &str) -> io::Result<()> {
    let mut sessions = BTreeMap::<String, Vec<PathBuf>>::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(label) = extract_session_label(&filename, base_name) {
            sessions.entry(label).or_default().push(entry.path());
        }
    }
    while sessions.len() >= SESSION_LIMIT {
        let Some(oldest) = sessions.keys().next().cloned() else {
            break;
        };
        if let Some(paths) = sessions.remove(&oldest) {
            for path in paths {
                remove_if_present(&path)?;
            }
        }
    }
    Ok(())
}

fn extract_session_label(filename: &str, base_name: &str) -> Option<String> {
    let prefix = format!("{base_name}-");
    let suffix = filename.strip_prefix(&prefix)?;
    let log_suffix = suffix.find(LOG_SUFFIX)?;
    let session_fields = &suffix[..log_suffix];
    let rollover = &suffix[log_suffix + LOG_SUFFIX.len()..];
    let mut fields = session_fields.split('-');
    let timestamp = fields.next()?;
    let process_id = fields.next()?;
    let attempt = fields.next()?;
    if fields.next().is_some()
        || timestamp.len() != 20
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || process_id.len() != 10
        || !process_id.bytes().all(|byte| byte.is_ascii_digit())
        || attempt.len() != 2
        || !attempt.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    if !rollover.is_empty() {
        let rollover_index = rollover.strip_prefix('.')?.parse::<usize>().ok()?;
        if !(1..=ROLLOVER_COUNT).contains(&rollover_index)
            || rollover_index.to_string() != rollover[1..]
        {
            return None;
        }
    }
    Some(format!("{prefix}{session_fields}"))
}

fn rollover_path(root: &Path, label: &str, index: usize) -> PathBuf {
    root.join(format!("{label}{LOG_SUFFIX}.{index}"))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_configured_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || (!Path::new(path).is_absolute() && validate_relative_path(Path::new(path)).is_err())
    {
        return Err("host log path is invalid".into());
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "relative log path must remain under the trusted state root",
        ));
    }
    Ok(())
}

fn check_existing_path_has_no_symlinks(state_root: &Path, relative: &Path) -> io::Result<()> {
    let mut current = state_root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "relative log path contains a symbolic link",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn validate_state_root(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "trusted state root must be absolute",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "trusted state root is not a real directory",
        ));
    }
    let canonical = path.canonicalize()?;
    if !same_path(&canonical, path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "trusted state root is not canonical",
        ));
    }
    Ok(canonical)
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let path = path.to_string_lossy();
            path.strip_prefix(r"\\?\")
                .unwrap_or(path.as_ref())
                .replace('/', "\\")
        };
        normalize(left).eq_ignore_ascii_case(&normalize(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn default_minimum_level_matches_stable_and_prerelease_channels() {
        assert_eq!(default_minimum_level(""), 2);
        assert_eq!(default_minimum_level("stable.4"), 2);
        assert_eq!(default_minimum_level("StAbLe.rc1"), 2);
        assert_eq!(default_minimum_level("alpha.1"), 1);
        assert_eq!(default_minimum_level("1.0"), 1);
    }

    #[test]
    fn from_value_uses_the_package_version_channel_by_default() {
        let settings = LoggingSettings::from_value(json!({})).unwrap();

        assert_eq!(
            settings.minimum_level,
            default_minimum_level(env!("CARGO_PKG_VERSION_PRE"))
        );
    }

    #[test]
    fn from_value_preserves_an_explicit_minimum_level() {
        let settings = LoggingSettings::from_value(json!({"min_log_level":4})).unwrap();

        assert_eq!(settings.minimum_level, 4);
    }

    #[test]
    fn persisted_host_configuration_maps_effective_levels_and_preserves_stored_paths() {
        let state_root = temp_root();
        let configuration = json!({
            "ok":true,
            "ready":true,
            "version":1,
            "revision":3,
            "values":{"log_path":"custom/logs/daemon.log"},
            "effective":{
                "log_path":state_root.join("custom/logs/daemon.log").to_string_lossy(),
                "min_log_level":"warning",
            },
        });

        let settings =
            LoggingSettings::from_host_configuration(&configuration, Some(&state_root)).unwrap();

        assert_eq!(settings.minimum_level, 3);
        assert_eq!(
            settings.log_path,
            Some(PathBuf::from("custom/logs/daemon.log"))
        );
        assert_eq!(settings.state_root.as_deref(), Some(state_root.as_path()));
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn persisted_host_configuration_rejects_unready_or_unsupported_levels() {
        let state_root = temp_root();
        let mut configuration = json!({
            "ok":true,
            "ready":true,
            "version":1,
            "revision":1,
            "values":{"log_path":"daemon.log"},
            "effective":{
                "log_path":state_root.join("daemon.log").to_string_lossy(),
                "min_log_level":"info",
            },
        });

        assert!(
            LoggingSettings::from_host_configuration(
                &json!({"ok":false,"ready":false,"version":1,"revision":0}),
                Some(&state_root)
            )
            .is_err()
        );
        configuration["effective"]["min_log_level"] = json!("trace");
        assert!(
            LoggingSettings::from_host_configuration(&configuration, Some(&state_root)).is_err()
        );
        fs::remove_dir_all(state_root).unwrap();
    }

    fn temp_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn test_record(message: &str) -> Record {
        Record {
            sequence: 1,
            level: crate::protocol::Level::Info,
            target: "diagnostics::test".into(),
            message: message.into(),
            fields: BTreeMap::new(),
        }
    }

    #[test]
    fn failed_sink_reopens_for_the_next_qualifying_record() {
        let state_root = temp_root();
        let settings = LoggingSettings::from_value(json!({
            "log_path":"capture.log",
            "state_root":state_root,
        }))
        .unwrap();
        let (mut logger, warning) = SessionLogger::open(settings);
        assert!(warning.is_none());
        assert_eq!(logger.health(), json!({"state":"healthy","failure":null}));
        let original_path = logger.session.as_ref().unwrap().active_path.clone();

        drop(logger.session.as_mut().unwrap().file.take());
        logger.write_record("capture", &test_record("lost to closed sink"));
        assert!(logger.session.is_none());
        assert_eq!(logger.health(), json!({"state":"failed","failure":"write"}));

        logger.write_record("capture", &test_record("written after reopening"));
        let reopened = logger.session.as_ref().unwrap();
        assert_ne!(reopened.active_path, original_path);
        let contents = fs::read_to_string(&reopened.active_path).unwrap();
        assert!(contents.contains("written after reopening"));
        assert!(!contents.contains("lost to closed sink"));
        assert_eq!(logger.health(), json!({"state":"healthy","failure":null}));

        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn initially_unavailable_sink_reopens_when_a_later_record_arrives() {
        let state_root = temp_root();
        let blocking_path = state_root.join("delayed");
        fs::write(&blocking_path, b"block directory creation").unwrap();
        let settings = LoggingSettings::from_value(json!({
            "log_path":"delayed/capture.log",
            "state_root":state_root,
        }))
        .unwrap();
        let (mut logger, warning) = SessionLogger::open(settings);
        assert!(warning.is_some());
        assert!(logger.session.is_none());
        assert_eq!(logger.health(), json!({"state":"failed","failure":"open"}));

        logger.write_record("capture", &test_record("still blocked"));
        assert_eq!(logger.health(), json!({"state":"failed","failure":"open"}));

        fs::remove_file(&blocking_path).unwrap();
        logger.write_record(
            "capture",
            &test_record("recovered after initial open failure"),
        );

        let session = logger.session.as_ref().unwrap();
        let contents = fs::read_to_string(&session.active_path).unwrap();
        assert!(contents.contains("recovered after initial open failure"));
        assert_eq!(logger.health(), json!({"state":"healthy","failure":null}));

        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn file_and_directory_paths_resolve_to_session_logs() {
        let state_root = temp_root();
        let file_path = PathBuf::from("capture.log");
        let mut file_logger = SessionLog::new(&file_path, &state_root).unwrap();
        assert_eq!(file_logger.root, state_root.join("logs"));
        file_logger.write_line(b"record\n").unwrap();
        assert!(file_logger.active_path.exists());

        let directory_path = PathBuf::from("custom-dir");
        let directory_logger = SessionLog::new(&directory_path, &state_root).unwrap();
        assert_eq!(directory_logger.root, state_root.join("custom-dir"));
        assert!(directory_logger.active_path.exists());
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn relative_traversal_and_symlink_escape_are_rejected() {
        let state_root = temp_root();
        let outside = temp_root();
        assert!(SessionLog::new(Path::new("../outside.log"), &state_root).is_err());

        let link = state_root.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            // Windows may not grant unprivileged test processes symlink creation.
            fs::remove_dir_all(state_root).unwrap();
            fs::remove_dir_all(outside).unwrap();
            return;
        }
        assert!(SessionLog::new(Path::new("link/logs"), &state_root).is_err());
        fs::remove_dir_all(state_root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn rotation_keeps_only_the_active_file_and_bounded_rollovers() {
        let state_root = temp_root();
        let mut session =
            SessionLog::with_limits(Path::new("capture.log"), &state_root, 8, 2).unwrap();
        for _ in 0..6 {
            session.write_line(b"123456\n").unwrap();
        }
        assert!(rollover_path(&session.root, &session.label, 1).exists());
        assert!(rollover_path(&session.root, &session.label, 2).exists());
        assert!(!rollover_path(&session.root, &session.label, 3).exists());
        assert!(session.active_path.exists());
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn startup_retention_caps_existing_session_groups_at_twenty_nine() {
        let state_root = temp_root();
        let root = state_root.join("capture");
        fs::create_dir_all(&root).unwrap();
        for index in 0..30 {
            fs::write(
                root.join(format!("capture-{index:020}-0000000000-00.log")),
                b"old",
            )
            .unwrap();
        }
        let unrelated_files = [
            "capture-notes.log",
            "capture-00000000000000000000-0000000000-0.log",
            "capture-00000000000000000000-0000000000-00.log.backup",
            "capture-00000000000000000000-0000000000-00.log.",
        ];
        for filename in unrelated_files {
            fs::write(root.join(filename), b"keep").unwrap();
        }

        let _session = SessionLog::new(Path::new("capture"), &state_root).unwrap();
        let session_count = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                extract_session_label(&entry.file_name().to_string_lossy(), "capture").is_some()
            })
            .map(|entry| {
                extract_session_label(&entry.file_name().to_string_lossy(), "capture").unwrap()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(session_count, 30);
        for filename in unrelated_files {
            assert_eq!(fs::read(root.join(filename)).unwrap(), b"keep");
        }
        fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn custom_file_retention_does_not_prune_another_policy_after_restart() {
        let state_root = temp_root();
        let open = |path: &str| {
            let settings = LoggingSettings::from_value(json!({
                "log_path":path,
                "state_root":state_root,
            }))
            .unwrap();
            let (logger, warning) = SessionLogger::open(settings);
            assert!(warning.is_none());
            logger
        };
        let mut first = open("custom/app+one.log");
        first.write_record("capture", &test_record("first policy"));
        let first_session_path = first.active_path_for_test().to_path_buf();
        drop(first);

        // These configured names previously collapsed to the same prefix in
        // custom/logs, allowing this policy to prune the first policy's file.
        for _ in 0..SESSION_LIMIT {
            let mut session = open("custom/app_one.log");
            session.write_record("capture", &test_record("second policy"));
        }
        let restarted = open("custom/app+one.log");

        assert_eq!(
            restarted.health(),
            json!({"state":"healthy","failure":null})
        );
        assert_eq!(
            first_session_path.parent(),
            Some(state_root.join("custom/logs").as_path())
        );
        assert_ne!(safe_base_name("app+one"), safe_base_name("app_one"));
        assert!(
            fs::read_to_string(&first_session_path)
                .unwrap()
                .contains("first policy")
        );
        fs::remove_dir_all(state_root).unwrap();
    }
}
