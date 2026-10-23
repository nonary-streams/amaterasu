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
        # Inherit-only entries do not affect a leaf file. On a directory they
        # can grant writes to newly created children, except CREATOR OWNER:
        # that placeholder cannot create a child without an independent ACE.
        if (-not $item.PSIsContainer -or $rule.IdentityReference.Value -eq 'S-1-3-0') {
            continue
        }
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

fn windows_powershell_path(program_files: &Path) -> PathBuf {
    program_files.join("PowerShell").join("7").join("pwsh.exe")
}

fn require_protected_powershell_path(path: &Path, program_files: &Path) -> PackageResult<()> {
    if !program_files.is_absolute() || path != windows_powershell_path(program_files) {
        return Err("PowerShell executable is outside the protected Program Files path".into());
    }
    Ok(())
}

#[repr(C)]
struct WinTrustFileInfo {
    size: u32,
    file_path: *const u16,
    file: *mut std::ffi::c_void,
    known_subject: *const std::ffi::c_void,
}

#[repr(C)]
struct WinTrustData {
    size: u32,
    policy_callback_data: *mut std::ffi::c_void,
    sip_client_data: *mut std::ffi::c_void,
    ui_choice: u32,
    revocation_checks: u32,
    union_choice: u32,
    file_info: *mut WinTrustFileInfo,
    state_action: u32,
    state_data: *mut std::ffi::c_void,
    url_reference: *mut u16,
    provider_flags: u32,
    ui_context: u32,
    signature_settings: *mut std::ffi::c_void,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

fn program_files_directory() -> PackageResult<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetFolderPathW(
            window: *mut std::ffi::c_void,
            folder: i32,
            token: *mut std::ffi::c_void,
            flags: u32,
            buffer: *mut u16,
        ) -> i32;
    }
    let mut buffer = [0u16; 260];
    // SAFETY: SHGetFolderPathW writes a NUL-terminated path of at most MAX_PATH
    // characters to the documented 260-element buffer.
    if unsafe {
        SHGetFolderPathW(
            std::ptr::null_mut(),
            0x0026, // CSIDL_PROGRAM_FILES
            std::ptr::null_mut(),
            0, // SHGFP_TYPE_CURRENT
            buffer.as_mut_ptr(),
        )
    } != 0
    {
        return Err("Program Files known folder is unavailable".into());
    }
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .ok_or("invalid Program Files path")?;
    let root: PathBuf = std::ffi::OsString::from_wide(&buffer[..length]).into();
    if !root.is_absolute() {
        return Err("Program Files known folder is not absolute".into());
    }
    Ok(root)
}

fn system_windows_directory() -> PackageResult<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemWindowsDirectoryW(buffer: *mut u16, length: u32) -> u32;
    }
    let mut buffer = [0u16; 1024];
    // SAFETY: Windows writes at most `length` UTF-16 units to this stack buffer.
    let length = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 || length as usize >= buffer.len() {
        return Err("Windows system directory is unavailable".into());
    }
    Ok(std::ffi::OsString::from_wide(&buffer[..length as usize]).into())
}

fn trusted_sid(sid: &str) -> bool {
    matches!(
        sid,
        "S-1-5-18"
            | "S-1-5-32-544"
            | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    )
}

fn write_access(mask: u32) -> bool {
    const WRITE_MASK: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
        | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
        | 0x0000_0010 // FILE_WRITE_EA
        | 0x0000_0040 // FILE_DELETE_CHILD
        | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
        | 0x0001_0000 // DELETE
        | 0x0004_0000 // WRITE_DAC
        | 0x0008_0000 // WRITE_OWNER
        | 0x1000_0000 // GENERIC_ALL
        | 0x4000_0000; // GENERIC_WRITE
    mask & WRITE_MASK != 0
}

#[repr(C)]
struct AclSizeInformation {
    ace_count: u32,
    acl_bytes_in_use: u32,
    acl_bytes_free: u32,
}

#[repr(C)]
struct AceHeader {
    ace_type: u8,
    ace_flags: u8,
    ace_size: u16,
}

struct LocalSecurityDescriptor(*mut std::ffi::c_void);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        // SAFETY: GetNamedSecurityInfoW allocates this descriptor with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

fn sid_text(sid: *mut std::ffi::c_void) -> PackageResult<String> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, text: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: `sid` points into a live security descriptor or ACE returned by Windows.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err("invalid Windows security identifier".into());
    }
    // SID strings are small. Bound the read even if a malformed native result is returned.
    let mut length = 0;
    // SAFETY: ConvertSidToStringSidW allocated a NUL-terminated UTF-16 string.
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `text` belongs to LocalAlloc and is no longer used after this call.
    let value = if length < 256 {
        let slice = unsafe { std::slice::from_raw_parts(text, length) };
        String::from_utf16(slice).map_err(|error| error.to_string())
    } else {
        Err("Windows security identifier is too long".into())
    };
    unsafe { LocalFree(text.cast()) };
    value
}

fn validate_machine_owned_acl(path: &Path, directory: bool) -> PackageResult<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetNamedSecurityInfoW(
            object: *const u16,
            object_type: u32,
            information: u32,
            owner: *mut *mut std::ffi::c_void,
            group: *mut *mut std::ffi::c_void,
            dacl: *mut *mut std::ffi::c_void,
            sacl: *mut *mut std::ffi::c_void,
            descriptor: *mut *mut std::ffi::c_void,
        ) -> u32;
        fn GetAclInformation(
            acl: *mut std::ffi::c_void,
            information: *mut AclSizeInformation,
            length: u32,
            information_class: u32,
        ) -> i32;
        fn GetAce(acl: *mut std::ffi::c_void, index: u32, ace: *mut *mut std::ffi::c_void) -> i32;
    }
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut owner, mut dacl, mut raw) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    // SAFETY: Windows writes pointers into `raw`; all returned pointers remain valid until
    // LocalSecurityDescriptor is dropped below. Other optional outputs are null.
    let result = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            1,                         // SE_FILE_OBJECT
            0x0000_0001 | 0x0000_0004, // OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut raw,
        )
    };
    if result != 0 || raw.is_null() {
        return Err(format!("could not read PowerShell path ACL: {result}"));
    }
    let _descriptor = LocalSecurityDescriptor(raw);
    if owner.is_null() || dacl.is_null() || !trusted_sid(&sid_text(owner)?) {
        return Err("PowerShell path has an untrusted owner or null DACL".into());
    }
    let mut information = AclSizeInformation {
        ace_count: 0,
        acl_bytes_in_use: 0,
        acl_bytes_free: 0,
    };
    // SAFETY: `dacl` points into the live security descriptor and `information` has the
    // documented ACL_SIZE_INFORMATION layout and size.
    if unsafe {
        GetAclInformation(
            dacl,
            &mut information,
            std::mem::size_of::<AclSizeInformation>() as u32,
            2, // AclSizeInformation
        )
    } == 0
    {
        return Err("could not inspect PowerShell path ACL".into());
    }
    for index in 0..information.ace_count {
        let mut ace = std::ptr::null_mut();
        // SAFETY: Windows bounds the index against the validated ACL and returns an ACE
        // pointer valid for the lifetime of `_descriptor`.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return Err("could not read PowerShell path ACE".into());
        }
        let header = unsafe { &*ace.cast::<AceHeader>() };
        if header.ace_size < 12 {
            return Err("PowerShell path has a malformed ACE".into());
        }
        if header.ace_type == 1 {
            continue; // Explicit denial cannot create a writer.
        }
        if header.ace_type != 0 {
            return Err("PowerShell path has an unsupported ACE type".into());
        }
        let mask = unsafe { *ace.cast::<u8>().add(4).cast::<u32>() };
        if !write_access(mask) {
            continue;
        }
        let sid = sid_text(unsafe { ace.cast::<u8>().add(8).cast() })?;
        if trusted_sid(&sid) || header.ace_flags & 0x08 != 0 && (!directory || sid == "S-1-3-0") {
            continue;
        }
        return Err("PowerShell path permits a nonprivileged writer".into());
    }
    Ok(())
}

fn validate_protected_module_path(path: &Path) -> PackageResult<()> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err("PowerShell module path is not a protected directory".into());
    }
    validate_machine_owned_acl(path, true)
}

fn validate_windows_powershell(path: &Path) -> PackageResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;

    #[link(name = "wintrust")]
    unsafe extern "system" {
        fn WinVerifyTrust(
            window: *mut std::ffi::c_void,
            action: *const Guid,
            data: *mut WinTrustData,
        ) -> i32;
    }

    let program_files = program_files_directory()?;
    let expected = windows_powershell_path(&program_files);
    require_protected_powershell_path(path, &program_files)?;
    for candidate in [
        program_files.clone(),
        program_files.join("PowerShell"),
        program_files.join("PowerShell").join("7"),
        expected.clone(),
    ] {
        let metadata = fs::symlink_metadata(&candidate).map_err(|error| error.to_string())?;
        if metadata.file_attributes() & 0x400 != 0
            || if candidate == expected {
                !metadata.is_file()
            } else {
                !metadata.is_dir()
            }
        {
            return Err(
                "Windows PowerShell path contains a reparse point or unexpected file".into(),
            );
        }
        validate_machine_owned_acl(&candidate, metadata.is_dir())?;
    }

    let file_path: Vec<u16> = expected.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info = WinTrustFileInfo {
        size: std::mem::size_of::<WinTrustFileInfo>() as u32,
        file_path: file_path.as_ptr(),
        file: std::ptr::null_mut(),
        known_subject: std::ptr::null(),
    };
    let mut data = WinTrustData {
        size: std::mem::size_of::<WinTrustData>() as u32,
        policy_callback_data: std::ptr::null_mut(),
        sip_client_data: std::ptr::null_mut(),
        ui_choice: 2,
        revocation_checks: 0,
        union_choice: 1,
        file_info: &mut file_info,
        state_action: 0,
        state_data: std::ptr::null_mut(),
        url_reference: std::ptr::null_mut(),
        provider_flags: 0x0000_1000,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    let action = Guid {
        data1: 0x00aa_c56b,
        data2: 0xcd44,
        data3: 0x11d0,
        data4: [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee],
    };
    // SAFETY: WinVerifyTrust reads the valid path and structures above for this call.
    if unsafe { WinVerifyTrust(-1isize as *mut std::ffi::c_void, &action, &mut data) } != 0 {
        return Err("Windows PowerShell signature is not trusted".into());
    }
    Ok(())
}

fn powershell(script: &str, environment: &[(&str, &Path)]) -> PackageResult<String> {
    let program_files = program_files_directory()?;
    let program = windows_powershell_path(&program_files);
    validate_windows_powershell(&program)?;
    let system_root = system_windows_directory()?;
    let system32 = system_root.join("System32");
    let legacy_powershell = system32.join("WindowsPowerShell");
    let legacy_modules = system32
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("Modules");
    let modern_modules = program_files.join("PowerShell").join("7").join("Modules");
    for directory in [
        system_root.as_path(),
        system32.as_path(),
        legacy_powershell.as_path(),
    ] {
        validate_protected_module_path(directory)?;
    }
    validate_protected_module_path(&legacy_powershell.join("v1.0"))?;
    validate_protected_module_path(&legacy_modules)?;
    validate_protected_module_path(&legacy_modules.join("PnpDevice"))?;
    validate_protected_module_path(&modern_modules)?;
    let mut command = Command::new(program);
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    // Do not allow user profile modules to shadow the management cmdlets used
    // by the privileged installer. The two module roots are OS-protected.
    let module_paths = std::env::join_paths([modern_modules, legacy_modules])
        .map_err(|error| error.to_string())?;
    command.env("PSModulePath", module_paths);
    command.env("SystemRoot", &system_root);
    command.env(
        "PATH",
        std::env::join_paths([system32, program_files.join("PowerShell").join("7")])
            .map_err(|error| error.to_string())?,
    );
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
    persist_grant_policy(
        &policy_path,
        &InputPolicy {
            schema: 1,
            component_set_digest: digest,
            keyboard,
            mouse,
        },
        seal_machine_private_file,
    )?;
    Ok(())
}

fn persist_grant_policy(
    path: &Path,
    policy: &InputPolicy<'_>,
    seal: impl FnOnce(&Path) -> Result<(), String>,
) -> PackageResult<()> {
    let parent = path
        .parent()
        .ok_or("grant policy has no parent directory")?;
    let staged_path = parent.join(format!(
        ".input-companion-policy-{}.pending",
        amaterasu_storage::random_identifier()?
    ));
    write_json_atomically(&staged_path, policy)?;
    if let Err(error) = seal(&staged_path) {
        return match fs::remove_file(&staged_path) {
            Ok(()) => Err(format!(
                "grant policy ACL seal failed; staged policy removed: {error}"
            )),
            Err(cleanup) => Err(format!(
                "grant policy ACL seal failed: {error}; staged policy cleanup failed: {cleanup}"
            )),
        };
    }
    fs::rename(&staged_path, path).map_err(|error| {
        let cleanup = fs::remove_file(&staged_path);
        match cleanup {
            Ok(()) => format!("sealed grant policy could not be published: {error}"),
            Err(cleanup) => format!(
                "sealed grant policy could not be published: {error}; staged policy cleanup failed: {cleanup}"
            ),
        }
    })?;
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
    fn powershell_is_resolved_only_under_protected_program_files() {
        let program_files = Path::new(r"C:\Program Files");
        let executable = windows_powershell_path(program_files);
        assert_eq!(
            executable,
            PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );
        assert!(
            require_protected_powershell_path(
                Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
                program_files
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "reads the live PowerShell 7 signature and protected ACL"]
    fn protected_powershell_passes_independent_signature_validation() {
        let path = windows_powershell_path(&program_files_directory().unwrap());
        validate_windows_powershell(&path).unwrap();
    }

    #[test]
    #[ignore = "launches protected PowerShell 7 for a read-only module lookup"]
    fn protected_powershell_loads_device_management_module() {
        let output = powershell(
            "$ErrorActionPreference = 'Stop'; (Get-Command Get-PnpDevice).Name",
            &[],
        )
        .unwrap();
        assert_eq!(output.trim(), "Get-PnpDevice");
    }

    #[test]
    fn failed_grant_policy_seal_removes_the_active_policy() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "amaterasu-vhf-policy-seal-{}.json",
                amaterasu_storage::random_identifier().unwrap()
            ));
        let digest = "0".repeat(64);
        let policy = InputPolicy {
            schema: 1,
            component_set_digest: &digest,
            keyboard: true,
            mouse: false,
        };
        let staged_path = std::cell::RefCell::new(None);
        let result = persist_grant_policy(&path, &policy, |staged| {
            assert!(!path.exists(), "unsealed policy must not be active");
            assert!(staged.exists(), "policy should be staged before sealing");
            *staged_path.borrow_mut() = Some(staged.to_path_buf());
            Err("synthetic ACL failure".into())
        });
        assert!(result.is_err());
        assert!(!path.exists());
        assert!(!staged_path.borrow().as_ref().unwrap().exists());
    }

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
