use super::*;
use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    time::Duration,
};
const CONTROL: &str = "/run/vibeshine/vkms-control.sock";
pub(super) fn execute(request: &HelperRequest, spec: &Descriptor) -> PackageResult<HelperReply> {
    match spec.kind {
        Kind::LinuxInput => input(request),
        Kind::LinuxVirtualDisplay => virtual_display(request, spec),
        _ => Ok(crate::helper::reply(
            request,
            DriverState::Unsupported,
            "This driver is for another operating system",
        )),
    }
}
fn runtime_uid(request: &HelperRequest) -> PackageResult<u32> {
    request
        .runtime_identity
        .parse::<u32>()
        .ok()
        .filter(|uid| *uid != 0)
        .ok_or("driver_invalid_runtime_uid".into())
}
fn arch_supported() -> bool {
    fs::read_to_string("/etc/os-release").is_ok_and(|release| {
        release.lines().any(|line| {
            matches!(line.trim_matches('"'), "ID=arch" | "ID=cachyos")
                || matches!(line, "ID=\"arch\"" | "ID=\"cachyos\"")
        })
    })
}
fn input(request: &HelperRequest) -> PackageResult<HelperReply> {
    let reply = |state, message: &str| Ok(crate::helper::reply(request, state, message));
    let (module, node) = match request.requirement.feature.as_str() {
        "input.uinput" => ("uinput", "/dev/uinput"),
        "input.uhid" => ("uhid", "/dev/uhid"),
        _ => {
            return reply(
                DriverState::Unsupported,
                "The selected input backend is unsupported",
            );
        }
    };
    if request.action == Action::Install {
        if !arch_supported() {
            return reply(
                DriverState::Unsupported,
                "Automatic input setup supports Arch and CachyOS",
            );
        }
        let uid = runtime_uid(request)?;
        if run(Path::new("/usr/bin/modprobe"), &[module.as_ref()])? != 0 {
            return reply(
                DriverState::Failed,
                "The input kernel module could not be loaded",
            );
        }
        let directory = Path::new("/etc/udev/rules.d");
        crate::security::real_path(directory)?;
        let path = directory.join(format!("71-amaterasu-{module}-{uid}.rules"));
        write_system_file(
            &path,
            format!("KERNEL==\"{module}\", SUBSYSTEM==\"misc\", OWNER=\"{uid}\", MODE=\"0600\"\n")
                .as_bytes(),
        )?;
        if run(
            Path::new("/usr/bin/udevadm"),
            &["control".as_ref(), "--reload-rules".as_ref()],
        )? != 0
            || run(
                Path::new("/usr/bin/udevadm"),
                &[
                    "trigger".as_ref(),
                    "--subsystem-match=misc".as_ref(),
                    format!("--sysname-match={module}").as_ref(),
                ],
            )? != 0
        {
            return reply(
                DriverState::Failed,
                "Input access rules could not be applied",
            );
        }
        return reply(
            DriverState::Ready,
            "Input access configured; checking with the runtime account",
        );
    }
    if !Path::new(node).exists() {
        return reply(
            if arch_supported() {
                DriverState::Missing
            } else {
                DriverState::Unsupported
            },
            "The selected input device is unavailable; automatic setup supports Arch and CachyOS",
        );
    }
    match OpenOptions::new().read(true).write(true).open(node) {
        Ok(_) => reply(DriverState::Ready, "Input device access verified"),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => reply(
            DriverState::AccessDenied,
            "The runtime account needs input device access",
        ),
        Err(_) => reply(DriverState::Failed, "The input device could not be opened"),
    }
}
fn write_system_file(path: &Path, bytes: &[u8]) -> PackageResult<()> {
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err("driver_unsafe_system_file".into());
    }
    let parent = path.parent().ok_or("driver_invalid_system_path")?;
    crate::security::real_path(parent)?;
    let metadata = fs::metadata(parent).map_err(|e| e.to_string())?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("driver_unprotected_system_directory".into());
    }
    amaterasu_package::write_json_atomically(
        &Path::new(".io").join("system-write-intent.json"),
        &serde_json::json!({"path":path,"sha256":amaterasu_package::sha256(bytes)}),
    )?;
    // Atomic replacement never opens a pre-existing system-file symlink.
    let temporary = parent.join(format!(
        ".amaterasu-{}",
        amaterasu_package::random_identifier()?
    ));
    let result = (|| {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o644)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        amaterasu_package::sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn virtual_display(request: &HelperRequest, spec: &Descriptor) -> PackageResult<HelperReply> {
    if request.action == Action::Install {
        return install_virtual_display(request, spec);
    }
    let observed = fs::read_to_string("/sys/module/vibeshine_drm/version");
    let reply = |state, message: &str| {
        let mut reply = crate::helper::reply(request, state, message);
        reply.installed_version = observed
            .as_ref()
            .ok()
            .map(|version| version.trim().to_owned());
        Ok(reply)
    };
    match &observed {
        Ok(version) if version.trim() != spec.version => {
            return reply(
                DriverState::Incompatible,
                "A different virtual display module is loaded; a restart may be required",
            );
        }
        Err(_) if !arch_supported() => {
            return reply(
                DriverState::Unsupported,
                "Automatic virtual display installation supports Arch and CachyOS",
            );
        }
        Err(_) => {
            if Path::new(&format!("/usr/src/vibeshine-drm-{}", spec.version)).exists() {
                return reply(
                    DriverState::RebootRequired,
                    "The virtual display package is installed; load its signed module or restart the computer",
                );
            }
            return reply(
                DriverState::Missing,
                "The virtual display module is not loaded",
            );
        }
        Ok(_) => {}
    }
    let output = spec
        .output
        .as_deref()
        .filter(|output| {
            !output.is_empty()
                && output.len() <= 128
                && output
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        })
        .ok_or("driver_output_not_declared")?;
    match probe_broker(Path::new(CONTROL), output, 0) {
        Ok(()) => reply(
            DriverState::Ready,
            "Virtual display version and runtime broker access verified",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => reply(
            DriverState::AccessDenied,
            "The runtime account needs virtual display broker access",
        ),
        Err(_) => reply(
            DriverState::ManualActionRequired,
            "The virtual display control service is unavailable or rejected its status query",
        ),
    }
}
fn probe_broker(endpoint: &Path, output: &str, expected_uid: u32) -> std::io::Result<()> {
    let mut socket = UnixStream::connect(endpoint)?;
    use std::os::fd::AsRawFd;
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&peer) as libc::socklen_t;
    // SAFETY: the connected socket owns the descriptor and writable credential
    // storage remains live. Production accepts only the root-owned broker.
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
        || peer.uid != expected_uid
    {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    socket.write_all(format!("status {output}\n").as_bytes())?;
    let mut bytes = Vec::new();
    socket.take(1025).read_to_end(&mut bytes)?;
    let connected = format!("STATUS connected {output}\n");
    let disconnected = format!("STATUS disconnected {output}\n");
    if bytes == connected.as_bytes() || bytes == disconnected.as_bytes() {
        Ok(())
    } else {
        Err(std::io::Error::other("broker status was not recognized"))
    }
}
fn install_virtual_display(
    request: &HelperRequest,
    spec: &Descriptor,
) -> PackageResult<HelperReply> {
    let reply = |state, message: &str| Ok(crate::helper::reply(request, state, message));
    if !arch_supported() {
        return reply(
            DriverState::Unsupported,
            "Automatic virtual display installation supports Arch and CachyOS",
        );
    }
    let package = payload(request, &spec.payload)?;
    let signature = format!(
        "{}.sig",
        package
            .file_name()
            .ok_or("driver_payload_missing")?
            .to_string_lossy()
    );
    if !request
        .requirement
        .payloads
        .iter()
        .any(|artifact| artifact.file == signature)
    {
        return Err("driver_arch_signature_missing".into());
    }
    let kernel = Command::new("/usr/bin/uname")
        .arg("-r")
        .output()
        .map_err(|e| e.to_string())?;
    let kernel = String::from_utf8(kernel.stdout).map_err(|e| e.to_string())?;
    let kernel = kernel.trim();
    if kernel.is_empty()
        || !kernel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+'))
    {
        return Err("driver_invalid_kernel".into());
    }
    let headers = Path::new(&format!("/usr/lib/modules/{kernel}/build")).exists();
    if spec.prerequisites.is_empty()
        && (!headers
            || !Path::new("/usr/bin/dkms").exists()
            || !Path::new("/usr/bin/make").exists()
            || !Path::new("/usr/bin/cc").exists()
            || !Path::new("/usr/bin/setfacl").exists())
    {
        return reply(
            DriverState::ManualActionRequired,
            &format!(
                "Install headers for the running kernel {kernel}, dkms, base-devel and acl, or choose a publisher bundle containing their signed packages"
            ),
        );
    }
    let mut packages = Vec::new();
    for name in &spec.prerequisites {
        if ![".pkg.tar.zst", ".pkg.tar.xz", ".pkg.tar.gz"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            return Err("driver_invalid_prerequisite_package".into());
        }
        let file = payload(request, &Some(name.clone()))?;
        if !request
            .requirement
            .payloads
            .iter()
            .any(|artifact| artifact.file == format!("{name}.sig"))
        {
            return Err("driver_prerequisite_signature_missing".into());
        }
        packages.push(file);
    }
    packages.push(package);
    let config = Path::new(".io/pacman-driver.conf");
    fs::write(
        config,
        "Include = /etc/pacman.conf\n[options]\nLocalFileSigLevel = Required\n",
    )
    .map_err(|e| e.to_string())?;
    let mut arguments: Vec<&std::ffi::OsStr> = vec![
        "--config".as_ref(),
        config.as_os_str(),
        "-U".as_ref(),
        "--needed".as_ref(),
        "--noconfirm".as_ref(),
    ];
    arguments.extend(packages.iter().map(|package| package.as_os_str()));
    if run(Path::new("/usr/bin/pacman"), &arguments)? != 0 {
        return reply(
            DriverState::ManualActionRequired,
            "The signed virtual display package or its local prerequisites were rejected; check the package signature and running-kernel compatibility",
        );
    }
    if !Path::new(&format!("/usr/lib/modules/{kernel}/build")).exists() {
        return reply(
            DriverState::ManualActionRequired,
            &format!(
                "The installed headers do not match the running kernel {kernel}; install matching headers or restart into the matching kernel"
            ),
        );
    }
    configure_broker_access(runtime_uid(request)?)?;
    reply(
        if fs::read_to_string("/sys/module/vibeshine_drm/version")
            .is_ok_and(|version| version.trim() == spec.version)
        {
            DriverState::Ready
        } else {
            DriverState::RebootRequired
        },
        "Virtual display package installed; restart or complete kernel module signing/enrollment if required. The computer will not restart automatically",
    )
}
fn configure_broker_access(uid: u32) -> PackageResult<()> {
    let directory = Path::new("/etc/systemd/system/vibeshine-vkms-control.socket.d");
    amaterasu_package::create_private_directory(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    write_system_file(
        &directory.join(format!("90-amaterasu-{uid}.conf")),
        format!("[Socket]\nExecStartPost=/usr/bin/setfacl -m u:{uid}:rw {CONTROL}\n").as_bytes(),
    )?;
    if run(Path::new("/usr/bin/systemctl"), &["daemon-reload".as_ref()])? != 0
        || run(
            Path::new("/usr/bin/systemctl"),
            &[
                "enable".as_ref(),
                "--now".as_ref(),
                "vibeshine-vkms-control.socket".as_ref(),
            ],
        )? != 0
    {
        return Err("driver_broker_prerequisites_failed".into());
    }
    if let Ok(metadata) = fs::symlink_metadata(CONTROL) {
        if metadata.uid() != 0 || !metadata.file_type().is_socket() {
            return Err("driver_unsafe_broker_socket".into());
        }
        if run(
            Path::new("/usr/bin/setfacl"),
            &[
                "-m".as_ref(),
                format!("u:{uid}:rw").as_ref(),
                CONTROL.as_ref(),
            ],
        )? != 0
        {
            return Err("driver_broker_access_failed".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_probe_requires_recognized_response_and_exact_output() {
        use std::os::unix::net::UnixListener;
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-driver-probe-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("socket");
        for response in [
            "STATUS disconnected Virtual-9\n",
            "STATUS connected Virtual-8\n",
            "not a status\n",
        ] {
            let listener = UnixListener::bind(&path).unwrap();
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 17];
                socket.read_exact(&mut request).unwrap();
                assert_eq!(&request, b"status Virtual-9\n");
                socket.write_all(response.as_bytes()).unwrap();
            });
            assert_eq!(
                probe_broker(&path, "Virtual-9", unsafe { libc::geteuid() }).is_ok(),
                response == "STATUS disconnected Virtual-9\n"
            );
            worker.join().unwrap();
            fs::remove_file(&path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
}
