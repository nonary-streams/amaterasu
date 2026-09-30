//! One append-open destination shared by an application's entire command lifecycle.

use crate::containment::DetachedChild;
use serde::{Deserialize, Deserializer};
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct SharedOutput(Arc<File>);

impl SharedOutput {
    pub fn open(path: Option<&str>) -> io::Result<Option<Self>> {
        path.map(open_regular_output).transpose()
    }

    /// Duplicates refer to the same opened file. No later command resolves the
    /// configured pathname again, including after failed undo or file rotation.
    pub fn attach(&self, command: &mut Command) -> io::Result<()> {
        command
            .stdout(self.0.try_clone()?)
            .stderr(self.0.try_clone()?);
        Ok(())
    }
}

fn open_regular_output(path: &str) -> io::Result<SharedOutput> {
    if let Ok(metadata) = std::fs::metadata(path)
        && !metadata.is_file()
    {
        return Err(non_regular_output());
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    // A path may change after metadata inspection. Nonblocking open prevents a
    // replacement FIFO without a reader from hanging the managed-module tick.
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(non_regular_output());
    }
    Ok(SharedOutput(Arc::new(file)))
}

fn non_regular_output() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "application output must be a regular file",
    )
}

pub(crate) fn deserialize_output<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .filter(|path| !path.is_empty() && path != "null"))
}

pub(crate) fn validate_output(path: Option<&str>) -> Result<(), &'static str> {
    if path.is_some_and(|path| {
        path.len() > 1_024 || path.chars().any(char::is_control) || !Path::new(path).is_absolute()
    }) {
        return Err("application output must be a bounded absolute path");
    }
    Ok(())
}

pub(crate) fn check_output_support(path: Option<&str>, elevated: bool) -> Result<(), &'static str> {
    check_output_support_with(path, elevated, DetachedChild::output_redirection_supported)
}

fn check_output_support_with(
    path: Option<&str>,
    elevated: bool,
    supported: impl FnOnce(bool) -> io::Result<bool>,
) -> Result<(), &'static str> {
    if path.is_some() && !supported(elevated).map_err(|_| "application_output_unavailable")? {
        return Err("application_output_unsupported");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_output_requires_supported_process_handles_before_spawning() {
        assert_eq!(
            check_output_support_with(Some("output"), true, |elevated| {
                assert!(elevated);
                Ok(false)
            }),
            Err("application_output_unsupported")
        );
        assert_eq!(
            check_output_support_with(Some("output"), false, |_| Err(io::Error::other("token"))),
            Err("application_output_unavailable")
        );
        assert_eq!(
            check_output_support_with(None, false, |_| panic!(
                "null output must not request handles"
            )),
            Ok(())
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_output_without_a_reader_is_rejected_without_waiting() {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::env::temp_dir().join(format!("amaterasu-output-fifo-{}", std::process::id()));
        let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is live, NUL-terminated, and identifies a unique
        // local test path. No descriptor or pointer ownership is transferred.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        let result = SharedOutput::open(path.to_str());
        std::fs::remove_file(&path).unwrap();
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
