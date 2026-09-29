//! Resolve trusted raw command targets before selecting their working directory.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

const MAX_SEARCH_PATH_BYTES: usize = 16_384;
const MAX_SEARCH_PATH_ENTRIES: usize = 128;
const MAX_EXECUTABLE_PATH_BYTES: usize = 1_024;

pub(crate) fn resolve_command_executable(
    executable: &str,
    environment: &BTreeMap<String, String>,
    working_directory: Option<&str>,
) -> io::Result<PathBuf> {
    if executable.is_empty()
        || executable.len() > MAX_EXECUTABLE_PATH_BYTES
        || executable.chars().any(char::is_control)
        || executable.contains("://")
        || Path::new(executable)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command target requires a bounded executable path and an explicit interpreter",
        ));
    }
    let executable_path = Path::new(executable);
    if executable_path.is_absolute() {
        return Ok(executable_path.to_path_buf());
    }

    let base_directory = match working_directory {
        Some(directory) => PathBuf::from(directory),
        None => std::env::current_dir()?,
    };
    if !base_directory.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command working directory must be absolute",
        ));
    }
    if executable_path.components().count() > 1 {
        return bounded_executable_path(base_directory.join(executable_path));
    }

    let search_path = path_environment_value(environment)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "command PATH is unavailable"))?;
    if search_path.len() > MAX_SEARCH_PATH_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command PATH exceeds byte bounds",
        ));
    }
    let directories = std::env::split_paths(&search_path)
        .take(MAX_SEARCH_PATH_ENTRIES + 1)
        .collect::<Vec<_>>();
    if directories.len() > MAX_SEARCH_PATH_ENTRIES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command PATH exceeds entry bounds",
        ));
    }
    let candidates = executable_names(executable_path);
    for directory in directories {
        let directory = if directory.is_absolute() {
            directory
        } else {
            base_directory.join(directory)
        };
        for candidate in &candidates {
            let candidate = directory.join(candidate);
            if is_executable_file(&candidate)
                && let Ok(path) = bounded_executable_path(candidate)
            {
                return Ok(path);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "command executable was not found on its effective PATH",
    ))
}

fn bounded_executable_path(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute()
        && path.to_str().is_some_and(|value| {
            value.len() <= MAX_EXECUTABLE_PATH_BYTES && !value.chars().any(char::is_control)
        })
    {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved command executable exceeds path bounds",
        ))
    }
}

fn path_environment_value(environment: &BTreeMap<String, String>) -> Option<OsString> {
    #[cfg(windows)]
    let configured = environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value));
    #[cfg(not(windows))]
    let configured = environment.get("PATH").map(OsString::from);

    configured.or_else(|| std::env::var_os("PATH"))
}

#[cfg(windows)]
fn executable_names(executable: &Path) -> Vec<PathBuf> {
    if executable.extension().is_some() {
        vec![executable.to_path_buf()]
    } else {
        vec![
            executable.with_extension("com"),
            executable.with_extension("exe"),
        ]
    }
}

#[cfg(not(windows))]
fn executable_names(executable: &Path) -> Vec<PathBuf> {
    vec![executable.to_path_buf()]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_empty_path_entries_resolve_against_command_directory() {
        let executable = std::env::current_exe().unwrap();
        let directory = executable.parent().unwrap();
        let name = executable.file_name().unwrap().to_str().unwrap();
        for search_path in ["", "."] {
            let environment = BTreeMap::from([("PATH".into(), search_path.into())]);
            let resolved =
                resolve_command_executable(name, &environment, directory.to_str()).unwrap();
            assert_eq!(
                resolved.canonicalize().unwrap(),
                executable.canonicalize().unwrap()
            );
            assert!(resolved.is_absolute());
        }
    }

    #[test]
    fn explicit_empty_path_does_not_fall_back_to_host_search_path() {
        let environment = BTreeMap::from([("PATH".into(), String::new())]);
        let error = resolve_command_executable(
            "amaterasu-missing-command-92831",
            &environment,
            std::env::temp_dir().to_str(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn search_bounds_are_checked_before_selecting_an_early_executable() {
        let executable = std::env::current_exe().unwrap();
        let directory = executable.parent().unwrap();
        let name = executable.file_name().unwrap().to_str().unwrap();
        let entries =
            std::env::join_paths(std::iter::repeat_n(directory, MAX_SEARCH_PATH_ENTRIES + 1))
                .unwrap();
        let environment = BTreeMap::from([("PATH".into(), entries.to_str().unwrap().into())]);
        assert_eq!(
            resolve_command_executable(name, &environment, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let environment = BTreeMap::from([("PATH".into(), "x".repeat(MAX_SEARCH_PATH_BYTES + 1))]);
        assert_eq!(
            resolve_command_executable(name, &environment, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn raw_targets_require_explicit_interpreters_for_batch_commands() {
        for target in ["task.cmd", "task.BAT", "scheme://task", "", "task\n"] {
            assert_eq!(
                resolve_command_executable(target, &BTreeMap::new(), None)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn path_search_skips_non_executable_regular_files() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-command-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let blocked_directory = directory.join("blocked");
        let executable_directory = directory.join("bin");
        std::fs::create_dir_all(&blocked_directory).unwrap();
        std::fs::create_dir(&executable_directory).unwrap();
        let name = "amaterasu-command-probe";
        let blocked = blocked_directory.join(name);
        std::fs::write(&blocked, b"not executable").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let executable = executable_directory.join(name);
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let search_path = std::env::join_paths([Path::new("blocked"), Path::new("bin")]).unwrap();
        let environment = BTreeMap::from([("PATH".into(), search_path.to_str().unwrap().into())]);

        let resolved = resolve_command_executable(name, &environment, directory.to_str()).unwrap();
        assert_eq!(resolved, executable);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
