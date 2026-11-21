//! Read-only inventory for the Linux drivers used by the built-in providers.
use amaterasu_contracts::RuntimeConfig;
use amaterasu_module_sdk::drivers::DriverState;
use amaterasu_package::ComponentManifest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const INSTALLER: &str = "amaterasu.plugin-installation";
const CAPTURE: &str = "amaterasu.capture-vkms";
const INPUT: &str = "amaterasu.input";
const UINPUT_DEVICE: &str = "/dev/uinput";
const SETTINGS_KEY: &str = "builtin_driver_inventory";

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct Inventory {
    capture: bool,
    uinput: Option<PathBuf>,
}

/// Derive the inventory from enabled providers in the verified graph at load
/// time. Only the coordinator receives this projection of their settings.
pub fn configure(components: &[ComponentManifest], runtime: &mut RuntimeConfig) {
    let loaded = |id| components.iter().any(|component| component.id == id);
    if !loaded(INSTALLER) {
        return;
    }
    let enabled = |id, setting: &str| {
        loaded(id)
            && runtime
                .settings
                .get(id)
                .is_some_and(|value| value[setting] == true)
    };
    let inventory = Inventory {
        capture: enabled(CAPTURE, "enabled"),
        uinput: enabled(INPUT, "allow_injection").then(|| {
            runtime.settings[INPUT]["uinput_path"]
                .as_str()
                .unwrap_or(UINPUT_DEVICE)
                .into()
        }),
    };
    let settings = runtime
        .settings
        .entry(INSTALLER.into())
        .or_insert_with(|| json!({}));
    if let Some(settings) = settings.as_object_mut() {
        settings.insert(SETTINGS_KEY.into(), json!(inventory));
    }
}

impl Inventory {
    pub(crate) fn from_settings(settings: &Value) -> Self {
        serde_json::from_value(settings[SETTINGS_KEY].clone()).unwrap_or_default()
    }

    pub(crate) fn observe(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        if self.capture {
            rows.push(vkms_driver(Path::new("/sys/module/vibeshine_drm")));
        }
        if let Some(device) = &self.uinput {
            rows.push(uinput_driver(device));
        }
        rows
    }
}

fn vkms_driver(module: &Path) -> Value {
    let (state, message) = match read_sysfs(&module.join("initstate")) {
        Ok(value) if value == "live" => {
            (DriverState::Ready, "The capture kernel module is loaded.")
        }
        Ok(_) => (
            DriverState::ManualActionRequired,
            "The capture kernel module is not active.",
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            DriverState::Missing,
            "The vibeshine_drm kernel module is not loaded.",
        ),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
            DriverState::AccessDenied,
            "The runtime account cannot read the capture module status.",
        ),
        Err(_) => (
            DriverState::Failed,
            "The capture kernel module status could not be read.",
        ),
    };
    json!({
        "id": "org.nonary.virtual-display",
        "name": "Vibeshine DRM (VKMS)",
        "publisher": "Nonary",
        "reason": "Provides the virtual display and framebuffer export used by VKMS capture.",
        "required_by": [CAPTURE],
        "required_components": [CAPTURE],
        "may_require_reboot": false,
        "state": state,
        "message": message,
        "installed_version": read_sysfs(&module.join("version")).ok(),
    })
}

fn uinput_driver(device: &Path) -> Value {
    let (state, message) = match uinput_interface_version(device) {
        Ok(5..) => (
            DriverState::Ready,
            "The runtime account can open uinput and query its device interface.",
        ),
        Ok(_) => (
            DriverState::Incompatible,
            "The input provider requires uinput interface version 5 or later.",
        ),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => (
            DriverState::Incompatible,
            "The configured input path is not a uinput device.",
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            DriverState::Missing,
            "The configured uinput device is unavailable.",
        ),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => (
            DriverState::AccessDenied,
            "The runtime account needs write access to the uinput device.",
        ),
        Err(_) => (
            DriverState::Failed,
            "The uinput device interface could not be checked.",
        ),
    };
    let installed_version = matches!(state, DriverState::Ready)
        .then(|| read_sysfs(Path::new("/proc/sys/kernel/osrelease")).ok())
        .flatten();
    json!({
        "id": "linux.uinput",
        "name": "Linux uinput",
        "publisher": "Linux kernel",
        "reason": "Creates the virtual keyboard, mouse, and uinput-backed gamepads.",
        "required_by": [INPUT],
        "required_components": [INPUT],
        "may_require_reboot": false,
        "state": state,
        "message": message,
        "installed_version": installed_version,
    })
}

fn uinput_interface_version(path: &Path) -> io::Result<u32> {
    // Linux's uinput is misc device 10:223. Check before opening and verify
    // the opened descriptor before sending an ioctl, including for custom paths.
    let check_device = |metadata: fs::Metadata| -> io::Result<()> {
        if metadata.file_type().is_char_device() && metadata.rdev() == libc::makedev(10, 223) {
            Ok(())
        } else {
            Err(io::ErrorKind::InvalidData.into())
        }
    };
    check_device(fs::symlink_metadata(path)?)?;
    let device = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    check_device(device.metadata()?)?;
    let mut version = 0u32;
    let get_version = libc::_IOR::<u32>(u32::from(b'U'), 45);
    // SAFETY: this is a verified uinput descriptor, and UI_GET_VERSION writes
    // one unsigned integer into the live storage supplied here.
    if unsafe { libc::ioctl(device.as_raw_fd(), get_version, &mut version) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(version)
}

fn read_sysfs(path: &Path) -> io::Result<String> {
    let mut value = String::new();
    File::open(path)?.take(129).read_to_string(&mut value)?;
    let value = value.trim();
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(id: &str) -> ComponentManifest {
        ComponentManifest {
            id: id.into(),
            version: "1.0.0".into(),
            file: format!("{id}.so"),
            sha256: "a".repeat(64),
            size: 1,
            abi_major: 1,
            runtime: false,
            requires: Default::default(),
            requested: Default::default(),
        }
    }

    #[test]
    fn inventory_follows_enabled_loaded_modules_and_the_configured_input_device() {
        let mut runtime = RuntimeConfig::default();
        runtime.settings.insert(
            INSTALLER.into(),
            json!({SETTINGS_KEY: {"capture": true, "uinput": "/claimed"}}),
        );
        runtime
            .settings
            .insert(CAPTURE.into(), json!({"enabled": true}));
        runtime.settings.insert(
            INPUT.into(),
            json!({"allow_injection": true, "uinput_path": "/dev/input-alias"}),
        );

        configure(&[component(INSTALLER), component(INPUT)], &mut runtime);
        let inventory = Inventory::from_settings(&runtime.settings[INSTALLER]);
        assert!(!inventory.capture);
        assert_eq!(
            inventory.uinput.as_deref(),
            Some(Path::new("/dev/input-alias"))
        );

        runtime
            .settings
            .insert(INPUT.into(), json!({"allow_injection": false}));
        configure(
            &[component(INSTALLER), component(CAPTURE), component(INPUT)],
            &mut runtime,
        );
        let inventory = Inventory::from_settings(&runtime.settings[INSTALLER]);
        assert!(inventory.capture);
        assert_eq!(inventory.uinput, None);
    }

    #[test]
    fn unavailable_capture_and_wrong_input_device_do_not_report_ready() {
        let absent = Path::new("/sys/module").join(amaterasu_package::random_identifier().unwrap());
        let capture = vkms_driver(&absent);
        assert_eq!(capture["state"], "missing");
        assert_eq!(capture["required_components"], json!([CAPTURE]));
        assert_eq!(capture["installed_version"], Value::Null);

        let input = uinput_driver(Path::new("/dev/null"));
        assert_eq!(input["state"], "incompatible");
        assert_eq!(input["required_components"], json!([INPUT]));
        assert_eq!(input["installed_version"], Value::Null);
    }
}
