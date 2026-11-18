//! Platform adapters for real OS installers. The signed descriptor names only
//! finite driver kinds and packaged payloads, never commands or system paths.
use amaterasu_module_sdk::drivers::{Action, DriverState, HelperReply, HelperRequest};
use amaterasu_package::PackageResult;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(any(windows, target_os = "linux"))]
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};
#[cfg(target_os = "linux")]
mod linux;
pub mod packaging;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_display;
#[cfg(windows)]
pub mod windows_job;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    LinuxInput,
    LinuxVirtualDisplay,
    WindowsVirtualDisplay,
    WindowsVigem,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub schema: u32,
    pub kind: Kind,
    pub version: String,
    pub payload: Option<String>,
    pub tool: Option<String>,
    pub windows_signer: Option<String>,
    #[serde(default)]
    pub catalog: Option<String>,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
}
#[cfg(any(windows, target_os = "linux"))]
fn payload(request: &HelperRequest, name: &Option<String>) -> PackageResult<PathBuf> {
    let name = name.as_ref().ok_or("driver_payload_missing")?;
    if !request
        .requirement
        .payloads
        .iter()
        .any(|artifact| artifact.file == *name)
    {
        return Err("driver_payload_not_signed".into());
    }
    Ok(std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(name))
}
#[cfg(any(windows, target_os = "linux"))]
fn run(program: &Path, args: &[&std::ffi::OsStr]) -> PackageResult<i32> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    Ok(status.code().unwrap_or(-1))
}
pub fn execute(request: &HelperRequest) -> PackageResult<HelperReply> {
    let descriptor_artifact = request
        .requirement
        .payloads
        .iter()
        .find(|artifact| artifact.file == "driver-adapter.json")
        .ok_or("driver_descriptor_missing")?;
    amaterasu_package::plugins::verify_artifact(
        &std::env::current_dir().map_err(|e| e.to_string())?,
        descriptor_artifact,
    )?;
    let spec: Descriptor = amaterasu_package::read_json(Path::new("driver-adapter.json"))?;
    if spec.schema != 1
        || spec.version.is_empty()
        || spec.version.len() > 128
        || !spec
            .version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'+'))
    {
        return Err("driver_descriptor_incompatible".into());
    }
    #[cfg(target_os = "linux")]
    let mut reply = linux::execute(request, &spec)?;
    #[cfg(windows)]
    let mut reply = windows::execute(request, &spec)?;
    #[cfg(not(any(windows, target_os = "linux")))]
    let mut reply = crate::helper::reply(
        request,
        DriverState::Unsupported,
        "This system is not supported by the driver adapter",
    );
    // Installer success is not a version probe; only detection/verification may
    // report the version actually read from the operating system.
    if request.action == Action::Install {
        reply.installed_version = None;
    }
    Ok(reply)
}
