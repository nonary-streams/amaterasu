use super::*;
use std::fs;
const POWERSHELL: &str = "WindowsPowerShell/v1.0/powershell.exe";
pub(super) fn execute(request: &HelperRequest, spec: &Descriptor) -> PackageResult<HelperReply> {
    let system = PathBuf::from(std::env::var_os("SystemRoot").ok_or("driver_system_root_missing")?)
        .join("System32");
    if !matches!(spec.kind, Kind::WindowsVigem | Kind::WindowsVirtualDisplay) {
        return Ok(crate::helper::reply(
            request,
            DriverState::Unsupported,
            "This driver is for another operating system",
        ));
    }
    if request.action == Action::Install {
        return install(request, spec, &system);
    }
    let detected = detect(request, spec, &system)?;
    if detected.state != DriverState::Ready {
        return Ok(detected);
    }
    let probe = || -> PackageResult<HelperReply> {
        match spec.kind {
            Kind::WindowsVigem => match probe_vigem(&system.join("ViGEmClient.dll")) {
                Ok(()) => Ok(crate::helper::reply(
                    request,
                    DriverState::Ready,
                    "ViGEm driver version and runtime device connection verified",
                )),
                Err(message) => Ok(crate::helper::reply(
                    request,
                    DriverState::AccessDenied,
                    &message,
                )),
            },
            Kind::WindowsVirtualDisplay => {
                let tool = installed_broker_tool(request, spec, &system);
                let Ok(tool) = tool else {
                    return Ok(crate::helper::reply(
                        request,
                        DriverState::AccessDenied,
                        "The virtual display broker needs the approved tools and runtime access",
                    ));
                };
                let _ = tool; // SCM binary identity and protected installed tools were verified.
                match super::windows_display::probe() {
                    Ok(()) => Ok(crate::helper::reply(
                        request,
                        DriverState::Ready,
                        "Virtual display version, broker prerequisites and runtime device access verified",
                    )),
                    Err(message) => Ok(crate::helper::reply(
                        request,
                        DriverState::AccessDenied,
                        &message,
                    )),
                }
            }
            _ => unreachable!(),
        }
    };
    let mut reply = probe()?;
    reply.installed_version = detected.installed_version;
    Ok(reply)
}
fn detect(request: &HelperRequest, spec: &Descriptor, system: &Path) -> PackageResult<HelperReply> {
    let id = match spec.kind {
        Kind::WindowsVigem => "ROOT\\VIGEMBUS*",
        Kind::WindowsVirtualDisplay => "ROOT\\SUNSHINEVIRTUALDISPLAY*",
        _ => return Err("driver_kind_unsupported".into()),
    };
    let script = "$ErrorActionPreference='Stop'; $d=Get-CimInstance Win32_PnPSignedDriver | Where-Object {$_.DeviceID -like $env:AMATERASU_DRIVER_ID}; if (!$d) {exit 2}; $v=($d | Select-Object -First 1).DriverVersion; [Console]::Out.Write($v); if ($v -ne $env:AMATERASU_DRIVER_VERSION) {exit 3}; exit 0";
    let output = Command::new(system.join(POWERSHELL))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("AMATERASU_DRIVER_ID", id)
        .env("AMATERASU_DRIVER_VERSION", &spec.version)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let (state, message) = match output.status.code() {
        Some(0) => (
            DriverState::Ready,
            "Installed Windows driver version verified",
        ),
        Some(2) => (
            DriverState::Missing,
            "The required Windows driver is not installed",
        ),
        Some(3) => (
            DriverState::Incompatible,
            "The installed Windows driver version differs",
        ),
        _ => (DriverState::Failed, "Windows driver detection failed"),
    };
    let mut reply = crate::helper::reply(request, state, message);
    if matches!(state, DriverState::Ready | DriverState::Incompatible) {
        let version = std::str::from_utf8(&output.stdout)
            .map_err(|_| "driver_invalid_detected_version")?
            .trim();
        if version.is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
            return Err("driver_invalid_detected_version".into());
        }
        reply.installed_version = Some(version.to_owned());
    }
    Ok(reply)
}
fn installed_broker_tool(
    request: &HelperRequest,
    spec: &Descriptor,
    system: &Path,
) -> PackageResult<PathBuf> {
    let script = r#"$ErrorActionPreference='Stop'; $s=Get-CimInstance Win32_Service -Filter "Name='SunshineVirtualDisplayBroker'"; if (!$s -or $s.State -ne 'Running') {exit 2}; if ($s.PathName -match '^\s*"([^"]+)"') {$matches[1]} else {exit 3}"#;
    let output = Command::new(system.join(POWERSHELL))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() || output.stdout.len() > 65536 {
        return Err("driver_broker_path_unavailable".into());
    }
    let broker = PathBuf::from(
        String::from_utf8(output.stdout)
            .map_err(|e| e.to_string())?
            .trim(),
    );
    let directory = broker.parent().ok_or("driver_broker_path_unavailable")?;
    crate::security::real_path(directory)?;
    crate::security::windows_parent(directory)?;
    let tool = payload(request, &spec.tool)?
        .file_name()
        .ok_or("driver_tool_missing")?
        .to_owned();
    for name in [
        broker.file_name().ok_or("driver_broker_path_unavailable")?,
        tool.as_os_str(),
    ] {
        let artifact = request
            .requirement
            .payloads
            .iter()
            .find(|artifact| std::ffi::OsStr::new(&artifact.file) == name)
            .ok_or("driver_broker_not_in_package")?;
        amaterasu_package::plugins::verify_artifact(directory, artifact)?;
    }
    Ok(directory.join(tool))
}
fn verify_signature(path: &Path, spec: &Descriptor, system: &Path) -> PackageResult<()> {
    let signer = spec
        .windows_signer
        .as_deref()
        .filter(|signer| signer.len() == 40 && signer.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or("driver_invalid_windows_signer")?;
    let script = "$ErrorActionPreference='Stop'; $s=Get-AuthenticodeSignature -LiteralPath $env:AMATERASU_DRIVER_PAYLOAD; if ($s.Status -ne 'Valid' -or $s.SignerCertificate.Thumbprint -ne $env:AMATERASU_DRIVER_SIGNER) {exit 1}";
    let status = Command::new(system.join(POWERSHELL))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("AMATERASU_DRIVER_PAYLOAD", path)
        .env("AMATERASU_DRIVER_SIGNER", signer)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("driver_windows_signature_rejected".into());
    }
    Ok(())
}
fn install(
    request: &HelperRequest,
    spec: &Descriptor,
    system: &Path,
) -> PackageResult<HelperReply> {
    let package = payload(request, &spec.payload)?;
    let code = match spec.kind {
        Kind::WindowsVigem => {
            verify_signature(&package, spec, system)?;
            let code = run(
                &system.join("msiexec.exe"),
                &[
                    "/i".as_ref(),
                    package.as_os_str(),
                    "/qn".as_ref(),
                    "/norestart".as_ref(),
                ],
            )?;
            if matches!(code, 0 | 3010) && spec.client.is_some() {
                let client = payload(request, &spec.client)?;
                verify_signature(&client, spec, system)?;
                install_client(&client, &system.join("ViGEmClient.dll"))?;
            }
            code
        }
        Kind::WindowsVirtualDisplay => {
            // The signed catalog is pinned before Windows validates INF/catalog
            // membership through UpdateDriverForPlugAndPlayDevices in the tool.
            verify_signature(&payload(request, &spec.catalog)?, spec, system)?;
            let directory = retain_virtual_display_assets(request)?;
            let tool = directory.join(spec.tool.as_ref().ok_or("driver_tool_missing")?);
            let inf = directory.join(spec.payload.as_ref().ok_or("driver_inf_missing")?);
            let code = run(
                &tool,
                &[
                    "driver".as_ref(),
                    "install".as_ref(),
                    "--inf".as_ref(),
                    inf.as_os_str(),
                ],
            )?;
            if !matches!(code, 0 | 3) {
                return Ok(crate::helper::reply(
                    request,
                    DriverState::Failed,
                    "Windows rejected the virtual display driver package",
                ));
            }
            if run(&tool, &["broker".as_ref(), "install".as_ref()])? != 0
                || run(&tool, &["broker".as_ref(), "start".as_ref()])? != 0
            {
                return Ok(crate::helper::reply(
                    request,
                    DriverState::ManualActionRequired,
                    "The driver was installed but its protected broker requires attention",
                ));
            }
            match grant_display_access(request, system) {
                Ok(changed) if changed || code == 3 => 3010,
                Ok(_) => 0,
                Err(_) => {
                    return Ok(crate::helper::reply(
                        request,
                        DriverState::ManualActionRequired,
                        "The driver was installed but runtime device access could not be configured; an administrator must check its device security",
                    ));
                }
            }
        }
        _ => return Err("driver_kind_unsupported".into()),
    };
    let (state, message) = match code {
        0 => (
            DriverState::Ready,
            "Driver installed; checking with the runtime account",
        ),
        3010 => (
            DriverState::RebootRequired,
            "Restart the computer to finish driver setup. It will not restart automatically",
        ),
        _ => (DriverState::Failed, "Windows driver installation failed"),
    };
    Ok(crate::helper::reply(request, state, message))
}
fn grant_display_access(request: &HelperRequest, system: &Path) -> PackageResult<bool> {
    let source = include_str!("windows_device_access.cs");
    let script = format!(
        "$ErrorActionPreference='Stop'; Add-Type -TypeDefinition @'\n{source}\n'@; if ([AmaterasuDisplayDeviceAccess]::Grant($env:AMATERASU_RUNTIME_SID)) {{exit 3}}; exit 0"
    );
    let status = Command::new(system.join(POWERSHELL))
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("AMATERASU_RUNTIME_SID", &request.runtime_identity)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    match status.code() {
        Some(0) => Ok(false),
        Some(3) => Ok(true),
        _ => Err("driver_runtime_device_access_failed".into()),
    }
}
fn retain_virtual_display_assets(request: &HelperRequest) -> PackageResult<PathBuf> {
    let data = PathBuf::from(std::env::var_os("ProgramData").ok_or("driver_program_data_missing")?);
    crate::security::real_path(&data)?;
    let root = data.join("AmaterasuDrivers");
    amaterasu_package::create_machine_private_directory(&root)?;
    let identity = root.join(&request.runtime_identity);
    amaterasu_package::create_machine_private_directory(&identity)?;
    let driver = identity.join(&request.requirement.id);
    amaterasu_package::create_machine_private_directory(&driver)?;
    let digest = amaterasu_package::sha256(
        &serde_json::to_vec(&request.requirement).map_err(|e| e.to_string())?,
    );
    let directory = driver.join(digest);
    if !directory.exists() {
        let stage = driver.join(format!(
            ".stage-{}",
            amaterasu_package::random_identifier()?
        ));
        amaterasu_package::create_machine_private_directory(&stage)?;
        let copy = (|| {
            for artifact in &request.requirement.payloads {
                fs::copy(&artifact.file, stage.join(&artifact.file)).map_err(|e| e.to_string())?;
                amaterasu_package::plugins::verify_artifact(&stage, artifact)?;
                crate::security::runtime_read(
                    &stage.join(&artifact.file),
                    &request.runtime_identity,
                )?;
            }
            crate::security::runtime_read(&stage, &request.runtime_identity)?;
            fs::rename(&stage, &directory).map_err(|e| e.to_string())
        })();
        if copy.is_err() {
            let _ = fs::remove_dir_all(stage);
        }
        copy?;
    }
    for path in [&root, &identity, &driver] {
        crate::security::runtime_read(path, &request.runtime_identity)?;
    }
    for artifact in &request.requirement.payloads {
        amaterasu_package::plugins::verify_artifact(&directory, artifact)?;
    }
    Ok(directory)
}
fn install_client(source: &Path, destination: &Path) -> PackageResult<()> {
    if destination.exists() {
        crate::security::real_path(destination)?;
        if amaterasu_package::sha256(&fs::read(destination).map_err(|e| e.to_string())?)
            == amaterasu_package::sha256(&fs::read(source).map_err(|e| e.to_string())?)
        {
            return Ok(());
        }
    }
    let parent = destination.parent().ok_or("driver_client_path_invalid")?;
    crate::security::real_path(parent)?;
    let temporary = parent.join(format!(
        ".amaterasu-vigem-{}.dll",
        amaterasu_package::random_identifier()?
    ));
    let result = (|| {
        fs::copy(source, &temporary).map_err(|e| e.to_string())?;
        fs::rename(&temporary, destination).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn probe_vigem(path: &Path) -> PackageResult<()> {
    if !path.is_file() {
        return Err("The ViGEm client library is missing from System32".into());
    }
    // SAFETY: use only the installed system directory; no current-directory DLL
    // search. This probe allocates a client and connects without creating input.
    let library = unsafe { libloading::os::windows::Library::load_with_flags(path, 0x800) }
        .map_err(|_| "The ViGEm client library could not be loaded")?;
    unsafe {
        let alloc = library
            .get::<unsafe extern "C" fn() -> *mut std::ffi::c_void>(b"vigem_alloc\0")
            .map_err(|e| e.to_string())?;
        let free = library
            .get::<unsafe extern "C" fn(*mut std::ffi::c_void)>(b"vigem_free\0")
            .map_err(|e| e.to_string())?;
        let connect = library
            .get::<unsafe extern "C" fn(*mut std::ffi::c_void) -> u32>(b"vigem_connect\0")
            .map_err(|e| e.to_string())?;
        let disconnect = library
            .get::<unsafe extern "C" fn(*mut std::ffi::c_void)>(b"vigem_disconnect\0")
            .map_err(|e| e.to_string())?;
        let client = alloc();
        if client.is_null() {
            return Err("The ViGEm client could not be allocated".into());
        }
        let status = connect(client);
        if status == 0x2000_0000 {
            disconnect(client);
        }
        free(client);
        if status != 0x2000_0000 {
            return Err("The runtime account could not connect to ViGEmBus".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_access_update_preserves_existing_aces_and_is_idempotent() {
        // Compile and exercise only the pure descriptor function. Grant is
        // never invoked and no device/service state is read or changed.
        let source = include_str!("windows_device_access.cs");
        let script = format!(
            "$ErrorActionPreference='Stop'; Add-Type -TypeDefinition @'\n{source}\n'@\n$before='D:P(A;;GA;;;SY)(A;;GA;;;BA)'; $user='S-1-5-21-1-2-3-1001'; $next=[AmaterasuDisplayDeviceAccess]::Merge($before,$user); $again=[AmaterasuDisplayDeviceAccess]::Merge($next,$user); if ($next -ne $again -or !$next.Contains($user) -or !$next.Contains('(A;;GA;;;SY)') -or !$next.Contains('(A;;GA;;;BA)')) {{exit 1}}"
        );
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let status = Command::new(system.join(POWERSHELL))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .unwrap();
        assert!(status.success());
    }
}
