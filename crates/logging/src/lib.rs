//! Product-owned filtering and bounded session file policy for host records.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SESSION_LIMIT: usize = 30;
const ROLLOVER_SIZE: u64 = 2 * 1024 * 1024;
const MAX_SESSION_SIZE: u64 = 10 * 1024 * 1024;
const MAX_ROLLOVERS: usize = (MAX_SESSION_SIZE / ROLLOVER_SIZE) as usize - 1;
const LOG_SUFFIX: &str = ".log";
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const LOGGER_QUEUE_CAPACITY: usize = 512;
const MAX_TARGET_BYTES: usize = 128;
const MAX_MESSAGE_BYTES: usize = 7_500;
const MAX_LOG_LINE_BYTES: usize = 8 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Level {
    Verbose = 0,
    Debug = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
    Fatal = 5,
}

impl Level {
    pub fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Verbose),
            1 => Some(Self::Debug),
            2 => Some(Self::Info),
            3 => Some(Self::Warning),
            4 => Some(Self::Error),
            5 => Some(Self::Fatal),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Verbose => "Verbose",
            Self::Debug => "Debug",
            Self::Info => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::Fatal => "Fatal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinimumLevel(u8);

impl MinimumLevel {
    pub fn new(value: u8) -> Result<Self, String> {
        if value <= 6 {
            Ok(Self(value))
        } else {
            Err("minimum log level must be between 0 and 6".into())
        }
    }

    pub fn parse(value: &str, fallback: Self) -> Self {
        Self::parse_checked(value, fallback).unwrap_or(fallback)
    }

    pub fn parse_checked(value: &str, fallback: Self) -> Result<Self, String> {
        match value {
            "product_default" => Ok(fallback),
            "verbose" => Ok(Self(0)),
            "debug" => Ok(Self(1)),
            "info" => Ok(Self(2)),
            "warning" => Ok(Self(3)),
            "error" => Ok(Self(4)),
            "fatal" => Ok(Self(5)),
            "none" => Ok(Self(6)),
            _ => match value.as_bytes().first() {
                Some(b'0'..=b'6') => Ok(Self(value.as_bytes()[0] - b'0')),
                _ => Err("minimum log level is invalid".into()),
            },
        }
    }

    pub fn default_for_version(version: &str) -> Self {
        let prerelease = version
            .split_once('-')
            .map(|(_, prerelease)| prerelease)
            .unwrap_or_default();
        Self::default_for_prerelease(prerelease)
    }

    pub fn default_for_prerelease(prerelease: &str) -> Self {
        if is_prerelease_channel(prerelease) {
            Self(1)
        } else {
            Self(2)
        }
    }

    pub fn value(self) -> u8 {
        self.0
    }

    pub fn allows(self, level: Level) -> bool {
        (level as u8) >= self.0
    }
}

fn is_prerelease_channel(version: &str) -> bool {
    let first_identifier = version.split(['.', '+']).next().unwrap_or_default();
    !first_identifier.is_empty() && !first_identifier.eq_ignore_ascii_case("stable")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoggingPolicy {
    pub revision: u64,
    pub minimum_level: MinimumLevel,
    pub log_path: PathBuf,
    pub state_root: PathBuf,
}

impl LoggingPolicy {
    pub fn new(
        revision: u64,
        minimum_level: u8,
        log_path: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let minimum_level = MinimumLevel::new(minimum_level)?;
        let log_path = log_path.into();
        validate_configured_path(&log_path).map_err(|error| error.to_string())?;
        let state_root = state_root.into();
        if !state_root.is_absolute() {
            return Err("logging state root must be absolute".into());
        }
        Ok(Self {
            revision,
            minimum_level,
            log_path,
            state_root,
        })
    }

    /// Resolves the product-owned subset of the verified global settings.
    /// Invalid individual values fall back to the built-in product defaults.
    pub fn from_global(
        revision: u64,
        global: Option<&serde_json::Value>,
        state_root: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let default_level = MinimumLevel::default_for_version(env!("CARGO_PKG_VERSION"));
        let minimum_level = global
            .and_then(|settings| settings.get("min_log_level"))
            .and_then(serde_json::Value::as_str)
            .map_or(default_level, |value| {
                MinimumLevel::parse(value, default_level)
            });
        let default_path = PathBuf::from("amaterasu.log");
        let log_path = global
            .and_then(|settings| settings.get("log_path"))
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .filter(|path| validate_configured_path(path).is_ok())
            .unwrap_or(default_path);
        Self::new(revision, minimum_level.value(), log_path, state_root)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl LogRecord {
    pub fn new(level: u8, target: &str, message: &str) -> Result<Self, String> {
        let level = Level::from_wire(level).ok_or("log record level must be between 0 and 5")?;
        if target.is_empty()
            || target.len() > MAX_TARGET_BYTES
            || target.chars().any(char::is_control)
        {
            return Err("log record target is invalid".into());
        }
        if message.len() > MAX_MESSAGE_BYTES || message.chars().any(|character| character == '\0') {
            return Err("log record message is invalid".into());
        }
        let message = message
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        Ok(Self {
            level,
            target: target.to_owned(),
            message,
        })
    }

    fn line(&self, timestamp_unix_ms: u128) -> Vec<u8> {
        let mut line = format!(
            "[{}]: {}: {}: {}\n",
            format_local_timestamp(timestamp_unix_ms),
            self.level.name(),
            self.target,
            self.message
        )
        .into_bytes();
        if line.len() > MAX_LOG_LINE_BYTES {
            line.truncate(MAX_LOG_LINE_BYTES);
            if let Some(last) = line.last_mut() {
                *last = b'\n';
            }
        }
        line
    }
}

fn format_local_timestamp(unix_ms: u128) -> String {
    let milliseconds = (unix_ms % 1000) as u32;
    let Some(local) = local_time(unix_ms) else {
        return "1970-01-01 00:00:00.000".into();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{milliseconds:03}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
    )
}

fn format_session_stamp(unix_ms: u128) -> String {
    let milliseconds = (unix_ms % 1000) as u32;
    let Some(local) = local_time(unix_ms) else {
        return format!("19700101-000000-{milliseconds:03}");
    };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{milliseconds:03}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
    )
}

fn local_time(unix_ms: u128) -> Option<libc::tm> {
    let seconds = libc::time_t::try_from(unix_ms / 1000).ok()?;
    let mut local = MaybeUninit::<libc::tm>::zeroed();
    #[cfg(windows)]
    // SAFETY: the CRT writes one `tm` using the live `time_t` input pointer.
    let status = unsafe { libc::localtime_s(local.as_mut_ptr(), &seconds) };
    #[cfg(unix)]
    // SAFETY: `localtime_r` writes to the supplied output and does not retain either pointer.
    let status = unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) };
    #[cfg(not(any(unix, windows)))]
    return None;
    #[cfg(windows)]
    if status != 0 {
        return None;
    }
    #[cfg(unix)]
    if status.is_null() {
        return None;
    }
    // SAFETY: a successful platform conversion initialized the complete `tm` value.
    Some(unsafe { local.assume_init() })
}

pub struct SessionWriter {
    root: PathBuf,
    label: String,
    active_path: PathBuf,
    file: Option<File>,
    bytes_written: u64,
    rollover_size: u64,
    rollover_count: usize,
}

impl SessionWriter {
    pub fn open(configured_path: &Path, state_root: &Path) -> io::Result<Self> {
        Self::with_limits(configured_path, state_root, ROLLOVER_SIZE, MAX_ROLLOVERS)
    }

    fn with_limits(
        configured_path: &Path,
        state_root: &Path,
        rollover_size: u64,
        rollover_count: usize,
    ) -> io::Result<Self> {
        let state_root = validate_state_root(state_root)?;
        let relative_path = !configured_path.is_absolute();
        let configured_path = if relative_path {
            validate_relative_path(configured_path)?;
            ensure_existing_path_has_no_symlinks(&state_root, configured_path)?;
            state_root.join(configured_path)
        } else {
            configured_path.to_path_buf()
        };
        let (root, base_name) = resolve_log_root(&configured_path)?;
        fs::create_dir_all(&root)?;
        let canonical_root = root.canonicalize()?;
        if relative_path && !canonical_root.starts_with(&state_root) {
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
        let mut session_file = None;
        for _ in 0..100 {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let label = format!("{base_name}-{}", format_session_stamp(timestamp));
            let active_path = canonical_root.join(format!("{label}{LOG_SUFFIX}"));
            match open_new_file(&active_path) {
                Ok(file) => {
                    session_file = Some((label, file));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error),
            }
        }
        let (label, file) = session_file.ok_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "session log name collision")
        })?;
        let active_path = canonical_root.join(format!("{label}{LOG_SUFFIX}"));
        let mut writer = Self {
            root: canonical_root,
            label,
            active_path,
            file: Some(file),
            bytes_written: 0,
            rollover_size: rollover_size.max(1),
            rollover_count,
        };
        writer.write_bom()?;
        Ok(writer)
    }

    pub fn active_path(&self) -> &Path {
        &self.active_path
    }

    pub fn write_record(&mut self, record: &LogRecord) -> io::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.write_line(&record.line(timestamp))
    }

    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < line.len() {
            if self.bytes_written >= self.rollover_size {
                self.rotate()?;
            }
            let remaining = self.rollover_size - self.bytes_written;
            let chunk_len = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(line.len() - written);
            self.write_chunk(&line[written..written + chunk_len])?;
            written += chunk_len;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }

    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("session log is unavailable"))?;
        file.write_all(bytes)?;
        file.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn write_bom(&mut self) -> io::Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("session log is unavailable"))?;
        file.write_all(BOM)?;
        file.flush()
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }

        if self.rollover_count == 0 {
            remove_if_present(&self.active_path)?;
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
            let first_rollover = rollover_path(&self.root, &self.label, 1);
            if let Err(rename_error) = fs::rename(&self.active_path, &first_rollover) {
                fs::copy(&self.active_path, &first_rollover).map_err(|_| rename_error)?;
                fs::remove_file(&self.active_path)?;
            }
        }

        let active_file = open_new_file(&self.active_path)?;
        self.file = Some(active_file);
        self.bytes_written = 0;
        self.write_bom()
    }
}

fn open_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
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
    if !filename.starts_with(&prefix) {
        return None;
    }
    let log_suffix = filename.find(LOG_SUFFIX)?;
    (log_suffix > prefix.len()).then(|| filename[..log_suffix].to_owned())
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

fn validate_configured_path(path: &Path) -> io::Result<()> {
    let path_string = path.to_string_lossy();
    if path.as_os_str().is_empty()
        || path_string.len() > 4096
        || path_string.chars().any(char::is_control)
        || (!path.is_absolute() && validate_relative_path(path).is_err())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configured log path is invalid",
        ));
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
            "relative log path escapes the trusted state root",
        ));
    }
    Ok(())
}

fn ensure_existing_path_has_no_symlinks(root: &Path, relative: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "relative log path traverses a symbolic link",
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
            io::ErrorKind::PermissionDenied,
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
    if canonical != path {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "trusted state root contains a path redirect",
        ));
    }
    Ok(canonical)
}

pub struct ProductLogger {
    events: SyncSender<LoggerEvent>,
    state_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoggerError {
    InvalidRecord,
    InvalidPolicy,
    QueueFull,
    WorkerUnavailable,
}

enum LoggerEvent {
    Record(LogRecord),
    Policy(LoggingPolicy),
}

impl ProductLogger {
    pub fn start_default(state_root: impl Into<PathBuf>) -> io::Result<Self> {
        Self::start_global(None, state_root)
    }

    pub fn start_global(
        global: Option<&serde_json::Value>,
        state_root: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let policy = LoggingPolicy::from_global(0, global, state_root)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Self::start(policy)
    }

    pub fn start(policy: LoggingPolicy) -> io::Result<Self> {
        validate_configured_path(&policy.log_path)?;
        let state_root = policy.state_root.clone();
        let (event_sender, event_receiver) = mpsc::sync_channel(LOGGER_QUEUE_CAPACITY);
        thread::Builder::new()
            .name("amaterasu-product-log".into())
            .spawn(move || run_logger(event_receiver, policy))?;
        Ok(Self {
            events: event_sender,
            state_root,
        })
    }

    pub fn emit(&self, level: u8, target: &str, message: &str) -> Result<(), LoggerError> {
        let record =
            LogRecord::new(level, target, message).map_err(|_| LoggerError::InvalidRecord)?;
        self.events
            .try_send(LoggerEvent::Record(record))
            .map_err(map_send_error)
    }

    pub fn update_policy(
        &self,
        revision: u64,
        minimum_level: u8,
        log_path: &str,
    ) -> Result<(), LoggerError> {
        let policy = LoggingPolicy::new(
            revision,
            minimum_level,
            PathBuf::from(log_path),
            self.state_root.clone(),
        )
        .map_err(|_| LoggerError::InvalidPolicy)?;
        self.events
            .try_send(LoggerEvent::Policy(policy))
            .map_err(map_send_error)
    }
}

fn map_send_error<T>(error: TrySendError<T>) -> LoggerError {
    match error {
        TrySendError::Full(_) => LoggerError::QueueFull,
        TrySendError::Disconnected(_) => LoggerError::WorkerUnavailable,
    }
}

fn run_logger(events: Receiver<LoggerEvent>, initial_policy: LoggingPolicy) {
    let mut policy = initial_policy;
    let mut writer = SessionWriter::open(&policy.log_path, &policy.state_root).ok();
    let mut retry_at = Instant::now() + RETRY_INTERVAL;
    let mut pending_policy: Option<LoggingPolicy> = None;
    let mut policy_retry_at = None;
    let mut policy_received = false;
    let mut events_open = true;
    while events_open {
        let next_event = if pending_policy.is_some()
            && policy_retry_at.is_none_or(|deadline| Instant::now() >= deadline)
        {
            let next = pending_policy.take().expect("policy presence was checked");
            if next.log_path == policy.log_path && next.state_root == policy.state_root {
                policy = next;
                policy_retry_at = None;
            } else if let Ok(next_writer) = SessionWriter::open(&next.log_path, &next.state_root) {
                policy = next;
                writer = Some(next_writer);
                retry_at = Instant::now() + RETRY_INTERVAL;
                policy_retry_at = None;
            } else {
                pending_policy = Some(next);
                policy_retry_at = Some(Instant::now() + RETRY_INTERVAL);
            }
            None
        } else {
            match events.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    events_open = false;
                    None
                }
            }
        };

        if let Some(event) = next_event {
            match event {
                LoggerEvent::Policy(next) => {
                    if ((!policy_received && next.revision == policy.revision)
                        || next.revision > policy.revision)
                        && pending_policy
                            .as_ref()
                            .is_none_or(|current: &LoggingPolicy| next.revision > current.revision)
                    {
                        pending_policy = Some(next);
                        policy_retry_at = None;
                        policy_received = true;
                    }
                }
                LoggerEvent::Record(record) => {
                    if policy.minimum_level.allows(record.level) {
                        let line = record.line(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                        );
                        if let Some(session_writer) = &mut writer
                            && session_writer.write_line(&line).is_err()
                        {
                            writer = None;
                            retry_at = Instant::now() + RETRY_INTERVAL;
                        }
                        let mut stderr = io::stderr().lock();
                        let _ = stderr.write_all(&line);
                        let _ = stderr.flush();
                    }
                }
            }
        }

        if writer.is_none() && pending_policy.is_none() && Instant::now() >= retry_at {
            writer = SessionWriter::open(&policy.log_path, &policy.state_root).ok();
            retry_at = Instant::now() + RETRY_INTERVAL;
        }
    }
    if let Some(mut session_writer) = writer {
        let _ = session_writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("amaterasu-logging-{}-{unique}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn source_levels_and_channel_defaults_match_pinned_policy() {
        assert_eq!(MinimumLevel::default_for_version("1.2.3").value(), 2);
        assert_eq!(MinimumLevel::default_for_version("1.2.3-rc.2").value(), 1);
        assert_eq!(
            MinimumLevel::default_for_version("1.2.3-stable.4").value(),
            2
        );
        assert_eq!(MinimumLevel::default_for_version("1.2.3-StAbLe").value(), 2);
        for (setting, expected) in [
            ("verbose", 0),
            ("debug", 1),
            ("info", 2),
            ("warning", 3),
            ("error", 4),
            ("fatal", 5),
            ("none", 6),
            ("3suffix", 3),
            ("6", 6),
            ("INFO", 2),
            ("7", 2),
            ("unknown", 2),
        ] {
            assert_eq!(
                MinimumLevel::parse(setting, MinimumLevel(2)).value(),
                expected,
                "setting {setting}"
            );
        }
    }

    #[test]
    fn threshold_filters_every_level_and_none_suppresses_all_records() {
        for minimum in 0..=6 {
            let minimum = MinimumLevel::new(minimum).unwrap();
            for level in 0..=5 {
                assert_eq!(
                    minimum.allows(Level::from_wire(level).unwrap()),
                    level >= minimum.value(),
                    "minimum {minimum:?}, level {level}"
                );
            }
        }
    }

    #[test]
    fn global_values_resolve_in_the_product_library_with_safe_fallbacks() {
        let directory = TestDirectory::new();
        let state_root = directory.0.join("state");
        fs::create_dir(&state_root).unwrap();
        let configured = serde_json::json!({
            "min_log_level": "debug",
            "log_path": "custom/amaterasu.log",
            "unrelated_setting": "ignored"
        });
        let policy = LoggingPolicy::from_global(7, Some(&configured), &state_root).unwrap();
        assert_eq!(policy.revision, 7);
        assert_eq!(policy.minimum_level.value(), 1);
        assert_eq!(policy.log_path, PathBuf::from("custom/amaterasu.log"));

        let invalid = serde_json::json!({
            "min_log_level": "unknown",
            "log_path": "../outside.log"
        });
        let fallback = LoggingPolicy::from_global(8, Some(&invalid), &state_root).unwrap();
        assert_eq!(
            fallback.minimum_level,
            MinimumLevel::default_for_version(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(fallback.log_path, PathBuf::from("amaterasu.log"));
    }

    #[test]
    fn records_are_bounded_and_rendered_with_source_severity_names() {
        let record = LogRecord::new(3, "amaterasu.nvhttp", "Listener ready\nnext line").unwrap();
        let timestamp = format_local_timestamp(1_783_000_000_123);
        assert_eq!(
            record.line(1_783_000_000_123),
            format!("[{timestamp}]: Warning: amaterasu.nvhttp: Listener ready next line\n")
                .as_bytes()
        );
        assert!(LogRecord::new(6, "safe", "text").is_err());
        assert!(LogRecord::new(2, "bad\ntarget", "text").is_err());
        assert!(LogRecord::new(2, "safe", &"x".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
        assert!(LogRecord::new(2, "safe", "private\0credential").is_err());
    }

    #[test]
    fn configured_file_path_uses_directory_or_extension_semantics_and_rotation() {
        let directory = TestDirectory::new();
        let root = directory.0.join("state");
        fs::create_dir(&root).unwrap();
        let output_directory = root.join("logs");
        let mut writer = SessionWriter::with_limits(&output_directory, &root, 32, 2).unwrap();
        let active_path = writer.active_path().to_path_buf();
        writer.write_line(&[b'x'; 40]).unwrap();
        writer.write_line(b"record\n").unwrap();

        let first_rollover = PathBuf::from(format!("{}.1", active_path.display()));
        assert!(first_rollover.exists());
        assert!(active_path.exists());
        assert_eq!(fs::metadata(&first_rollover).unwrap().len(), 35);
        assert!(fs::read(&active_path).unwrap().starts_with(BOM));

        let configured_file = root.join("nested").join("amaterasu.log");
        let _file_writer = SessionWriter::open(&configured_file, &root).unwrap();
        assert!(root.join("nested").join("logs").is_dir());
    }

    #[test]
    fn relative_paths_cannot_escape_or_traverse_symlinks() {
        let directory = TestDirectory::new();
        let root = directory.0.join("state");
        fs::create_dir(&root).unwrap();
        assert!(SessionWriter::open(Path::new("../outside.log"), &root).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = directory.0.join("outside");
            fs::create_dir(&outside).unwrap();
            symlink(&outside, root.join("linked")).unwrap();
            assert!(SessionWriter::open(Path::new("linked/logs/app.log"), &root).is_err());
        }
    }

    #[test]
    fn session_retention_keeps_thirty_sessions() {
        let directory = TestDirectory::new();
        let root = directory.0.join("state");
        let logs = root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        for index in 0..SESSION_LIMIT {
            let label = format!("amaterasu-20000101-000000-{index:03}");
            fs::write(logs.join(format!("{label}.log")), b"old").unwrap();
            fs::write(logs.join(format!("{label}.log.1")), b"rolled").unwrap();
        }
        let _writer = SessionWriter::open(&root.join("amaterasu.log"), &root).unwrap();
        let sessions = fs::read_dir(logs)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
            .count();
        assert_eq!(sessions, SESSION_LIMIT);
    }

    #[test]
    fn configured_policy_rejects_invalid_level_and_relative_path() {
        let directory = TestDirectory::new();
        let root = directory.0.join("state");
        fs::create_dir(&root).unwrap();
        assert!(LoggingPolicy::new(0, 7, "logs/app.log", &root).is_err());
        assert!(LoggingPolicy::new(0, 2, "../outside.log", &root).is_err());
    }

    #[test]
    fn product_logger_filters_before_writing_to_its_configured_session() {
        let directory = TestDirectory::new();
        let state_root = directory.0.join("state");
        fs::create_dir(&state_root).unwrap();
        let logger =
            ProductLogger::start(LoggingPolicy::new(0, 2, "product.log", &state_root).unwrap())
                .unwrap();
        logger
            .emit(1, "amaterasu.test", "filtered debug event")
            .unwrap();
        logger
            .emit(2, "amaterasu.test", "accepted info event")
            .unwrap();
        drop(logger);

        let log_directory = state_root.join("logs");
        let deadline = Instant::now() + Duration::from_secs(2);
        let contents = loop {
            let active = fs::read_dir(&log_directory).ok().and_then(|entries| {
                entries.filter_map(Result::ok).find(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "log")
                })
            });
            if let Some(active) = active
                && let Ok(contents) = fs::read_to_string(active.path())
                && contents.contains("accepted info event")
            {
                break contents;
            }
            assert!(
                Instant::now() < deadline,
                "product log worker did not flush"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!contents.contains("filtered debug event"));
        assert!(contents.contains("Info: amaterasu.test: accepted info event"));
    }
}
