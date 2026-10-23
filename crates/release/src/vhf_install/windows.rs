use super::*;
use amaterasu_storage::{seal_machine_private_directory, seal_machine_private_file};
use std::process::Command;

const DEVICE_ID_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$nodes = @(Get-PnpDevice | Where-Object {
    if ($_.InstanceId -notlike 'ROOT\SYSTEM\*') { return $false }
    $ids = @(Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction SilentlyContinue).Data
    @($ids) -contains 'ROOT\AMATERASUVHFINPUT'
})
if ($nodes.Count -gt 1) { throw 'multiple Amaterasu VHF root nodes exist' }
if ($nodes.Count -eq 1) {
    $matches = @(Get-CimInstance Win32_PnPSignedDriver | Where-Object {
        $_.DeviceID -eq $nodes[0].InstanceId
    })
    if ($matches.Count -gt 1) { throw 'multiple signed drivers for Amaterasu VHF root node' }
    [pscustomobject]@{
        instance = [string]$nodes[0].InstanceId
        inf = $(if ($matches.Count -eq 1) { [string]$matches[0].InfName } else { '' })
    } | ConvertTo-Json -Compress
}
"#;

const SERVICE_MATCH_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$service = Get-CimInstance Win32_Service -Filter "Name='AmaterasuInputCompanion'"
if ($null -eq $service) { Write-Output 'absent'; exit 0 }
$expected = '"' + $env:AMATERASU_VHF_SERVICE_EXE + '" "' + $env:AMATERASU_VHF_SERVICE_ROOT + '"'
if ($service.PathName -cne $expected -or $service.StartName -notin @('LocalSystem', 'NT AUTHORITY\LocalSystem')) {
    Write-Output 'different'; exit 0
}
Write-Output 'matches'
"#;

const SERVICE_ABSENT_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$service = Get-CimInstance Win32_Service -Filter "Name='AmaterasuInputCompanion'"
if ($null -ne $service) { throw 'Amaterasu input companion service already exists' }
Write-Output 'absent'
"#;

const STOPPED_SERVICE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$service = Get-CimInstance Win32_Service -Filter "Name='$env:AMATERASU_VHF_CHECK_SERVICE'"
if ($null -ne $service -and $service.State -ne 'Stopped') {
    throw 'stop the affected service through the reviewed maintenance procedure first'
}
Write-Output 'stopped'
"#;

const HOST_SERVICE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$service = Get-CimInstance Win32_Service -Filter "Name='Amaterasu'"
if ($null -eq $service -or $service.StartName -notin @('LocalSystem', 'NT AUTHORITY\LocalSystem')) {
    throw 'the installed Amaterasu LocalSystem service is unavailable'
}
$exe = $env:AMATERASU_VHF_HOST_EXE
$root = $env:AMATERASU_VHF_HOST_ROOT
$quoted = '"' + $exe + '" service "' + $root + '"'
$bare = $exe + ' service "' + $root + '"'
if ($service.PathName -cnotin @($quoted, $bare)) {
    throw 'the Amaterasu service does not use the protected installation and runtime'
}
Write-Output 'matches'
"#;

const MICROSOFT_TOOL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$signature = Get-AuthenticodeSignature -LiteralPath $env:AMATERASU_VHF_TOOL_PATH
if ($signature.Status -ne 'Valid' -or
    $signature.SignerCertificate.Subject -cne 'CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US') {
    throw 'VHF installation tool is not signed by the expected Microsoft publisher'
}
Write-Output 'verified'
"#;

const PROTECTED_DIRECTORY_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$path = $env:AMATERASU_VHF_PROTECTED_DIRECTORY
$item = Get-Item -LiteralPath $path -Force
if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'VHF installation path contains a reparse point'
}
$acl = Get-Acl -LiteralPath $path
$sidType = [Security.Principal.SecurityIdentifier]
$owner = $acl.GetOwner($sidType).Value
$trustedInstaller = 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
if ($owner -notin @('S-1-5-18', 'S-1-5-32-544', $trustedInstaller)) {
    throw 'VHF installation parent has an untrusted owner'
}
$writeMask = [Security.AccessControl.FileSystemRights]::WriteData -bor
    [Security.AccessControl.FileSystemRights]::AppendData -bor
    [Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
    [Security.AccessControl.FileSystemRights]::WriteAttributes -bor
    [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
    [Security.AccessControl.FileSystemRights]::Delete -bor
    [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
    [Security.AccessControl.FileSystemRights]::TakeOwnership
foreach ($rule in $acl.GetAccessRules($true, $true, $sidType)) {
    if (($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) {
        continue
    }
    if ($rule.AccessControlType -eq 'Allow' -and
        $rule.IdentityReference.Value -notin @('S-1-5-18', 'S-1-5-32-544', $trustedInstaller) -and
        ($rule.FileSystemRights -band $writeMask) -ne 0) {
        throw 'VHF installation parent permits a nonprivileged writer'
    }
}
Write-Output 'protected'
"#;

fn run(program: &Path, arguments: &[&str]) -> PackageResult<String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{}: {error}", program.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            program.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn system_tool(name: &str) -> PackageResult<PathBuf> {
    let root = PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?);
    let path = root.join("System32").join(name);
    if !path.is_file() {
        return Err(format!("Windows system tool unavailable: {name}"));
    }
    Ok(path)
}

fn microsoft_tool(path: &Path, expected_name: &str) -> PackageResult<()> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_attributes() & 0x400 != 0
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Microsoft tool path contains a reparse point or parent traversal".into());
    }
    if !path.is_file()
        || !path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
    {
        return Err(format!(
            "expected Microsoft tool is unavailable: {expected_name}"
        ));
    }
    let trusted_roots = ["ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
        .collect::<Vec<_>>();
    let trusted_root = trusted_roots
        .iter()
        .find(|root| path.starts_with(root))
        .ok_or("Microsoft tool must be located under protected Program Files")?;
    let mut directory = path
        .parent()
        .ok_or("Microsoft tool has no parent directory")?;
    loop {
        if powershell(
            PROTECTED_DIRECTORY_SCRIPT,
            &[("AMATERASU_VHF_PROTECTED_DIRECTORY", directory)],
        )?
        .trim()
            != "protected"
        {
            return Err("Microsoft tool has a writable ancestor".into());
        }
        if directory == trusted_root {
            break;
        }
        directory = directory
            .parent()
            .ok_or("Microsoft tool escaped Program Files")?;
    }
    if powershell(
        PROTECTED_DIRECTORY_SCRIPT,
        &[("AMATERASU_VHF_PROTECTED_DIRECTORY", path)],
    )?
    .trim()
        != "protected"
    {
        return Err("Microsoft tool executable is writable".into());
    }
    if powershell(MICROSOFT_TOOL_SCRIPT, &[("AMATERASU_VHF_TOOL_PATH", path)])?.trim() != "verified"
    {
        return Err(format!(
            "expected Microsoft tool could not be verified: {expected_name}"
        ));
    }
    Ok(())
}

fn powershell(script: &str, environment: &[(&str, &Path)]) -> PackageResult<String> {
    let program_files =
        PathBuf::from(std::env::var_os("ProgramFiles").ok_or("ProgramFiles unavailable")?);
    let mut command = Command::new(program_files.join("PowerShell").join("7").join("pwsh.exe"));
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Windows identity query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

struct WindowsSignatures<'a> {
    signtool: &'a Path,
    fingerprint: String,
}

impl<'a> WindowsSignatures<'a> {
    fn new(signtool: &'a Path, fingerprint: &str) -> PackageResult<Self> {
        Ok(Self {
            signtool,
            fingerprint: normalized_fingerprint(fingerprint)?,
        })
    }
}

impl Signatures for WindowsSignatures<'_> {
    fn verify_catalog_member(&self, catalog: &Path, member: &Path) -> PackageResult<()> {
        let catalog = catalog.to_str().ok_or("non-Unicode VHF catalog path")?;
        let member = member.to_str().ok_or("non-Unicode VHF member path")?;
        run(
            self.signtool,
            &[
                "verify",
                "/pa",
                "/sha1",
                &self.fingerprint,
                "/c",
                catalog,
                member,
            ],
        )?;
        Ok(())
    }

    fn verified_signer(&self, file: &Path) -> PackageResult<String> {
        let file = file.to_str().ok_or("non-Unicode VHF signed file path")?;
        run(
            self.signtool,
            &["verify", "/pa", "/sha1", &self.fingerprint, file],
        )?;
        Ok(self.fingerprint.clone())
    }
}

fn require_admin() -> PackageResult<()> {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
    }
    // SAFETY: this no-argument Windows call reads the caller's token.
    if unsafe { IsUserAnAdmin() } == 0 {
        return Err("VHF installation requires an elevated administrator".into());
    }
    Ok(())
}

fn checked_root(destination: &Path, runtime: &Path) -> PackageResult<()> {
    let program_files =
        PathBuf::from(std::env::var_os("ProgramFiles").ok_or("ProgramFiles is unavailable")?);
    let expected = program_files.join("Amaterasu").join("vhf-input");
    if destination != expected {
        return Err(format!("VHF install root must be {}", expected.display()));
    }
    let program_data =
        PathBuf::from(std::env::var_os("ProgramData").ok_or("ProgramData is unavailable")?);
    let expected_runtime = program_data.join("Amaterasu").join("runtime");
    if runtime != expected_runtime {
        return Err(format!(
            "VHF runtime must be {}",
            expected_runtime.display()
        ));
    }
    let parent = destination
        .parent()
        .ok_or("VHF install root has no parent")?;
    let runtime_parent = runtime.parent().ok_or("VHF runtime has no parent")?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
    let runtime_parent_metadata =
        fs::symlink_metadata(runtime_parent).map_err(|error| error.to_string())?;
    let runtime_metadata = fs::symlink_metadata(runtime).map_err(|error| error.to_string())?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || !runtime_parent_metadata.is_dir()
        || runtime_parent_metadata.file_type().is_symlink()
        || !runtime_metadata.is_dir()
        || runtime_metadata.file_type().is_symlink()
        || !runtime.join("bootstrap.json").is_file()
    {
        return Err("installed Amaterasu directory or runtime is unavailable".into());
    }
    for directory in [parent, runtime_parent, runtime] {
        if powershell(
            PROTECTED_DIRECTORY_SCRIPT,
            &[("AMATERASU_VHF_PROTECTED_DIRECTORY", directory)],
        )?
        .trim()
            != "protected"
        {
            return Err("installed Amaterasu directory is not protected".into());
        }
    }
    if powershell(
        HOST_SERVICE_SCRIPT,
        &[
            ("AMATERASU_VHF_HOST_EXE", &parent.join("amaterasu.exe")),
            ("AMATERASU_VHF_HOST_ROOT", runtime),
        ],
    )?
    .trim()
        != "matches"
    {
        return Err("installed Amaterasu service identity changed".into());
    }
    Ok(())
}

struct WindowsActions<'a> {
    destination: &'a Path,
    devcon: &'a Path,
}

impl InstallActions for WindowsActions<'_> {
    fn ensure_ready(&self, destination: &Path) -> PackageResult<()> {
        require_admin()?;
        if destination != self.destination {
            return Err("VHF destination identity changed".into());
        }
        seal_machine_private_directory(destination)
    }

    fn service_absent(&self) -> PackageResult<bool> {
        let result = powershell(SERVICE_ABSENT_SCRIPT, &[])?;
        Ok(result.trim() == "absent")
    }

    fn service_stopped(&self) -> PackageResult<()> {
        powershell(
            STOPPED_SERVICE_SCRIPT,
            &[("AMATERASU_VHF_CHECK_SERVICE", Path::new(SERVICE_NAME))],
        )?;
        Ok(())
    }

    fn create_service(&self, executable: &Path, runtime: &Path) -> PackageResult<()> {
        if self.service_matches(executable, runtime)? {
            return Err("VHF companion service already exists".into());
        }
        let binary_path = format!(
            "\"{}\" \"{}\"",
            executable.display(),
            runtime.join("input-companion").display()
        );
        run(
            &system_tool("sc.exe")?,
            &[
                "create",
                SERVICE_NAME,
                "binPath=",
                &binary_path,
                "start=",
                "demand",
                "obj=",
                "LocalSystem",
            ],
        )?;
        run(
            &system_tool("sc.exe")?,
            &["sidtype", SERVICE_NAME, "unrestricted"],
        )?;
        seal_machine_private_directory(&runtime.join("input-companion"))?;
        Ok(())
    }

    fn service_matches(&self, executable: &Path, runtime: &Path) -> PackageResult<bool> {
        let service_root = runtime.join("input-companion");
        let result = powershell(
            SERVICE_MATCH_SCRIPT,
            &[
                ("AMATERASU_VHF_SERVICE_EXE", executable),
                ("AMATERASU_VHF_SERVICE_ROOT", &service_root),
            ],
        )?;
        match result.trim() {
            "absent" => Ok(false),
            "matches" => Ok(true),
            _ => Err("existing companion service has an unexpected identity".into()),
        }
    }

    fn remove_service(&self) -> PackageResult<()> {
        powershell(
            STOPPED_SERVICE_SCRIPT,
            &[("AMATERASU_VHF_CHECK_SERVICE", Path::new(SERVICE_NAME))],
        )?;
        run(&system_tool("sc.exe")?, &["delete", SERVICE_NAME])?;
        Ok(())
    }

    fn install_device(&self, inf: &Path) -> PackageResult<(String, String)> {
        let inf = inf.to_str().ok_or("non-Unicode VHF INF path")?;
        run(self.devcon, &["install", inf, HARDWARE_ID])?;
        let (instance, published_inf) = self
            .find_owned_device()?
            .ok_or("device was not registered after VHF install")?;
        let installed_inf =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?)
                .join("INF")
                .join(&published_inf);
        if file_hash(&installed_inf)? != file_hash(&self.destination.join(FILES[0]))? {
            return Err("published VHF INF does not match the approved package".into());
        }
        Ok((instance, published_inf))
    }

    fn find_owned_device(&self) -> PackageResult<Option<(String, String)>> {
        let response = powershell(DEVICE_ID_SCRIPT, &[])?;
        if response.trim().is_empty() {
            return Ok(None);
        }
        #[derive(Deserialize)]
        struct Identity {
            instance: String,
            inf: String,
        }
        let identity: Identity =
            serde_json::from_str(response.trim()).map_err(|error| error.to_string())?;
        validate_owned_instance(&identity.instance)?;
        if !identity.inf.is_empty() {
            validate_owned_identity(&identity.instance, &identity.inf)?;
        }
        Ok(Some((identity.instance, identity.inf)))
    }

    fn find_matching_published_inf(&self, staged_inf: &Path) -> PackageResult<Option<String>> {
        let expected = file_hash(staged_inf)?;
        let inf_root =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?)
                .join("INF");
        let mut matches = Vec::new();
        for entry in fs::read_dir(inf_root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if validate_owned_identity("ROOT\\SYSTEM\\0", &name).is_ok()
                && file_hash(&entry.path())? == expected
            {
                matches.push(name);
            }
        }
        match matches.as_slice() {
            [] => Ok(None),
            [name] => Ok(Some(name.clone())),
            _ => Err("multiple identical published VHF INFs; refusing ownership".into()),
        }
    }

    fn published_inf_matches(&self, inf: &str, staged_inf: &Path) -> PackageResult<bool> {
        validate_owned_identity("ROOT\\SYSTEM\\0", inf)?;
        let published =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?)
                .join("INF")
                .join(inf);
        Ok(file_hash(&published)? == file_hash(staged_inf)?)
    }

    fn remove_device(&self, instance: &str) -> PackageResult<()> {
        validate_owned_identity(instance, "oem1.inf")?;
        run(self.devcon, &["remove", &format!("@{instance}")])?;
        Ok(())
    }

    fn remove_published_inf(&self, inf: &str) -> PackageResult<()> {
        validate_owned_identity("ROOT\\SYSTEM\\0", inf)?;
        let published =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?)
                .join("INF")
                .join(inf);
        if !published.exists() {
            // A previous rollback invocation may have completed this deletion
            // immediately before its journal update was interrupted.
            return Ok(());
        }
        if file_hash(&published)? != file_hash(&self.destination.join(FILES[0]))? {
            return Err("published VHF INF changed; refusing deletion".into());
        }
        run(&system_tool("pnputil.exe")?, &["/delete-driver", inf])?;
        Ok(())
    }
}

pub(super) fn verify_command(
    package: &Path,
    fingerprint: &str,
    signtool: &Path,
) -> PackageResult<()> {
    microsoft_tool(signtool, "signtool.exe")?;
    verify_package(
        package,
        fingerprint,
        &WindowsSignatures::new(signtool, fingerprint)?,
    )?;
    println!("VHF package and independently confirmed signer verified.");
    Ok(())
}

pub(super) fn seal_command(
    package: &Path,
    fingerprint: &str,
    signtool: &Path,
) -> PackageResult<()> {
    microsoft_tool(signtool, "signtool.exe")?;
    seal_package(
        package,
        fingerprint,
        &WindowsSignatures::new(signtool, fingerprint)?,
    )?;
    println!("Sealed an authenticated VHF package without using a signing key.");
    Ok(())
}

pub(super) fn install_command(
    package: &Path,
    destination: &Path,
    runtime: &Path,
    fingerprint: &str,
    signtool: &Path,
    devcon: &Path,
) -> PackageResult<()> {
    require_admin()?;
    checked_root(destination, runtime)?;
    microsoft_tool(signtool, "signtool.exe")?;
    microsoft_tool(devcon, "devcon.exe")?;
    let actions = WindowsActions {
        destination,
        devcon,
    };
    install(
        package,
        destination,
        runtime,
        fingerprint,
        &WindowsSignatures::new(signtool, fingerprint)?,
        &actions,
    )
}

pub(super) fn rollback_command(
    destination: &Path,
    runtime: &Path,
    devcon: &Path,
) -> PackageResult<()> {
    require_admin()?;
    checked_root(destination, runtime)?;
    microsoft_tool(devcon, "devcon.exe")?;
    rollback(
        destination,
        runtime,
        &WindowsActions {
            destination,
            devcon,
        },
    )
}

#[derive(Serialize)]
struct InputPolicy<'a> {
    schema: u32,
    component_set_digest: &'a str,
    keyboard: bool,
    mouse: bool,
}

pub(super) fn grant_command(
    destination: &Path,
    runtime: &Path,
    digest: &str,
    keyboard: &str,
    mouse: &str,
) -> PackageResult<()> {
    require_admin()?;
    checked_root(destination, runtime)?;
    let _operation_lock = operation_lock(runtime)?;
    let enabled = |value: &str| match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err("VHF grant flags must be 0 or 1".to_owned()),
    };
    let keyboard = enabled(keyboard)?;
    let mouse = enabled(mouse)?;
    if !keyboard && !mouse {
        return Err("VHF grant must enable keyboard or mouse".into());
    }
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("selected component digest must be SHA-256 hexadecimal".into());
    }
    let selected: amaterasu_package::ActivationState = read_json(&runtime.join("selection.json"))?;
    if selected.active.as_deref() != Some(digest) {
        return Err("VHF grant does not match the active selected component set".into());
    }
    let journal: InstallJournal = read_json(&journal_path(destination))?;
    if journal.schema != 1
        || !journal.service_created
        || journal.service_pending
        || journal.driver_pending
        || journal.device_instance.is_none()
        || journal.published_inf.is_none()
    {
        return Err("VHF companion package installation is incomplete".into());
    }
    let actions = WindowsActions {
        destination,
        devcon: Path::new(""),
    };
    if !actions.service_matches(&destination.join(FILES[3]), runtime)?
        || actions.find_owned_device()?.as_ref()
            != Some(&(
                journal.device_instance.unwrap(),
                journal.published_inf.unwrap(),
            ))
    {
        return Err("installed VHF service or device no longer matches its journal".into());
    }
    powershell(
        STOPPED_SERVICE_SCRIPT,
        &[("AMATERASU_VHF_CHECK_SERVICE", Path::new("Amaterasu"))],
    )?;
    let policy_path = runtime.join("input-companion-policy.json");
    if policy_path.exists() {
        return Err("VHF policy already exists; revoke it before changing grants".into());
    }
    write_json_atomically(
        &policy_path,
        &InputPolicy {
            schema: 1,
            component_set_digest: digest,
            keyboard,
            mouse,
        },
    )?;
    seal_machine_private_file(&policy_path)?;
    Ok(())
}

pub(super) fn revoke_command(destination: &Path, runtime: &Path) -> PackageResult<()> {
    require_admin()?;
    checked_root(destination, runtime)?;
    let _operation_lock = operation_lock(runtime)?;
    let policy_path = runtime.join("input-companion-policy.json");
    if policy_path.exists() {
        safe_file(runtime, "input-companion-policy.json")?;
        fs::remove_file(policy_path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "reads the live Windows service inventory and ACLs"]
    fn installed_root_guard_rejects_ordinary_user_modify_rights() {
        let Some(path) = std::env::var_os("AMATERASU_VHF_TEST_INSECURE_ROOT") else {
            return;
        };
        let result = powershell(
            PROTECTED_DIRECTORY_SCRIPT,
            &[("AMATERASU_VHF_PROTECTED_DIRECTORY", Path::new(&path))],
        );
        let error = result.unwrap_err();
        assert!(error.contains("nonprivileged writer"), "{error}");
    }

    #[test]
    #[ignore = "reads the live Windows PnP device inventory"]
    fn new_keyboard_mouse_root_is_absent_before_installation() {
        let response = powershell(DEVICE_ID_SCRIPT, &[]).unwrap();
        assert!(response.trim().is_empty(), "{response}");
    }
}
