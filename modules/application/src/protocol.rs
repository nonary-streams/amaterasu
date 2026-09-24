use crate::commands::DetachedCommand;
use crate::lifecycle::PreparationStep;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Action {
    Launch {
        operation_id: String,
        executable: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        cwd: Option<String>,
        #[serde(default = "default_auto_detach")]
        auto_detach: bool,
        #[serde(default = "default_wait_all")]
        wait_all: bool,
        #[serde(default = "default_exit_timeout_seconds")]
        exit_timeout_seconds: u64,
        #[serde(default)]
        steam_id: Option<String>,
        #[serde(default)]
        steam_install_dir: Option<String>,
        #[serde(default)]
        detached: Vec<String>,
        #[serde(default)]
        preparation: Box<[PreparationStep]>,
    },
    Lookup {
        operation_id: String,
    },
    Status {
        process_id: u64,
    },
    Terminate {
        process_id: u64,
    },
    List {
        #[serde(default)]
        offset: usize,
        #[serde(default = "list_limit")]
        limit: usize,
    },
}

fn list_limit() -> usize {
    16
}

fn default_auto_detach() -> bool {
    true
}

fn default_wait_all() -> bool {
    true
}

fn default_exit_timeout_seconds() -> u64 {
    10
}

pub(crate) const MAX_EXIT_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LaunchSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub auto_detach: bool,
    pub wait_all: bool,
    pub exit_timeout_seconds: u64,
    pub steam_id: Option<String>,
    pub steam_install_dir: Option<String>,
    pub detached: Vec<DetachedCommand>,
    pub preparation: Vec<PreparationStep>,
}

impl LaunchSpec {
    pub fn validate(&self, operation_id: &str) -> Result<(), &'static str> {
        if operation_id.is_empty()
            || operation_id.len() > 128
            || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("operation_id must be 1 to 128 printable ASCII bytes without spaces");
        }
        if self.exit_timeout_seconds > MAX_EXIT_TIMEOUT_SECONDS {
            return Err("exit timeout exceeds bounds");
        }
        if self
            .steam_id
            .as_deref()
            .is_some_and(|steam_id| !valid_steam_id(steam_id))
            || self
                .steam_install_dir
                .as_deref()
                .is_some_and(|directory| !valid_steam_install_directory(directory))
        {
            return Err("invalid Steam app identity or installation directory");
        }
        if !valid_path(&self.executable) || self.cwd.as_deref().is_some_and(|cwd| !valid_path(cwd))
        {
            return Err("executable and cwd must be absolute paths of 1 to 1024 bytes without NUL");
        }
        // std::process may invoke cmd.exe implicitly for batch targets on Windows.
        // Disallow them on every platform to keep the explicit-executable contract.
        if Path::new(&self.executable)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
        {
            return Err("batch targets are not supported; interpreters must be explicit");
        }
        if self.args.len() > 64
            || self
                .args
                .iter()
                .any(|arg| arg.len() > 1024 || arg.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>() > 8192
        {
            return Err(
                "args exceed 64 items, 1024 bytes per item, or 8192 total bytes, or contain NUL",
            );
        }
        if self.environment.len() > 32
            || self.environment.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 128
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    || value.len() > 1024
                    || value.contains('\0')
            })
            || self
                .environment
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>()
                > 4096
        {
            return Err("environment exceeds limits or contains invalid keys or NUL values");
        }
        // Windows environment variable names are case insensitive. Reject ambiguity on
        // every platform so the same request has one interpretation everywhere.
        let mut names: Vec<_> = self
            .environment
            .keys()
            .map(|key| key.to_ascii_uppercase())
            .collect();
        names.sort();
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("environment keys must be unique ignoring ASCII case");
        }
        PreparationStep::validate_all(&self.preparation)?;
        Ok(())
    }
}

fn valid_steam_id(value: &str) -> bool {
    value
        .parse::<u32>()
        .is_ok_and(|id| id != 0 && id.to_string() == value)
}

fn valid_steam_install_directory(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 1_024
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|component| matches!(component, "." | ".."))
    {
        return false;
    }
    let components = value
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let drive_rooted = value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\');
    let unc_rooted = value.starts_with("\\\\") || value.starts_with("//");
    let posix_rooted = value.starts_with('/') && !unc_rooted;
    (drive_rooted && components.len() > 1)
        || (unc_rooted && components.len() >= 3)
        || (posix_rooted && !components.is_empty())
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.contains('\0')
        && Path::new(value).is_absolute()
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ProcessState {
    Preparing,
    Running,
    Stopping,
    CleanupFailed {
        outcome: TerminalOutcome,
    },
    Exited {
        exit_code: Option<i32>,
        success: bool,
    },
    LaunchFailed {
        os_error: Option<i32>,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalOutcome {
    Exited,
    LaunchFailed,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(steam_id: Option<&str>, install_directory: Option<&str>) -> LaunchSpec {
        LaunchSpec {
            executable: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            auto_detach: true,
            wait_all: false,
            exit_timeout_seconds: 10,
            steam_id: steam_id.map(str::to_owned),
            steam_install_dir: install_directory.map(str::to_owned),
            detached: Vec::new(),
            preparation: Vec::new(),
        }
    }

    #[test]
    fn sparse_steam_fields_validate_independently_and_reject_invalid_values() {
        spec(None, None).validate("legacy").unwrap();
        spec(Some("42"), None).validate("id-only").unwrap();
        spec(None, Some("/games/title"))
            .validate("directory-only")
            .unwrap();
        spec(Some("4294967295"), Some("/games/title"))
            .validate("steam-game")
            .unwrap();

        for steam_id in ["0", "01", "+42", "4294967296"] {
            assert!(spec(Some(steam_id), None).validate("invalid").is_err());
        }
        for directory in ["relative/game", "/", "C:\\", "\\foo", "//server/share"] {
            assert!(spec(None, Some(directory)).validate("invalid").is_err());
        }
    }
}
