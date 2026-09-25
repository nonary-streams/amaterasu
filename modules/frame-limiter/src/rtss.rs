//! Narrow dynamic binding for RTSS profile control.
use crate::lease::{FrameLimitProvider, ProfileValues, ProviderError};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const LIMITER_DISABLED_FLAG: u32 = 1 << 2;

pub(crate) struct RtssProvider {
    #[cfg(windows)]
    api: Option<WindowsRtssApi>,
    #[cfg(windows)]
    install_path: Option<PathBuf>,
    #[cfg(not(windows))]
    _unsupported: (),
}

impl RtssProvider {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(windows)]
            api: None,
            #[cfg(windows)]
            install_path: None,
            #[cfg(not(windows))]
            _unsupported: (),
        }
    }
}

#[cfg(windows)]
impl FrameLimitProvider for RtssProvider {
    fn select_installation(&mut self, path: &str) -> Result<String, ProviderError> {
        let canonical = resolve_install_path(path)?;
        let api = WindowsRtssApi::load(&canonical)?;
        self.api = Some(api);
        self.install_path = Some(canonical.clone());
        Ok(canonical.to_string_lossy().into_owned())
    }

    fn profile_exists(&mut self, profile: &str) -> Result<bool, ProviderError> {
        validate_profile_name(profile)?;
        let path = self.profile_path(profile)?;
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            _ => Err(ProviderError::ReadFailed("rtss_profile_path_invalid")),
        }
    }

    fn read_profile(&mut self, profile: &str) -> Result<ProfileValues, ProviderError> {
        validate_profile_name(profile)?;
        let api = self.api()?;
        api.load_profile(profile)?;
        api.read_profile_values()
    }

    fn write_profile_verified(
        &mut self,
        profile: &str,
        values: ProfileValues,
    ) -> Result<(), ProviderError> {
        validate_profile_name(profile)?;
        let api = self.api()?;
        api.load_profile(profile)?;
        api.write_profile_values(values)?;
        api.save_profile(profile)?;
        api.update_profiles();
        api.load_profile(profile)?;
        let actual = api.read_profile_values()?;
        if actual != values || !self.profile_exists(profile)? {
            return Err(ProviderError::VerifyFailed(
                "rtss_profile_readback_mismatch",
            ));
        }
        Ok(())
    }

    fn delete_profile_verified(
        &mut self,
        profile: &str,
        expected: ProfileValues,
    ) -> Result<(), ProviderError> {
        validate_profile_name(profile)?;
        let path = self.profile_path(profile)?;
        let api = self.api()?;
        api.load_profile(profile)?;
        if api.read_profile_values()? != expected {
            return Err(ProviderError::VerifyFailed(
                "rtss_profile_changed_before_delete",
            ));
        }
        api.delete_profile(profile)?;
        api.update_profiles();
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => {
                return Err(ProviderError::VerifyFailed(
                    "rtss_profile_delete_unverified",
                ));
            }
        }
        api.load_profile(profile)?;
        Ok(())
    }

    fn limiter_disabled(&mut self) -> Result<bool, ProviderError> {
        self.api()?.limiter_disabled()
    }
}

#[cfg(not(windows))]
impl FrameLimitProvider for RtssProvider {
    fn select_installation(&mut self, _path: &str) -> Result<String, ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }

    fn profile_exists(&mut self, _profile: &str) -> Result<bool, ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }

    fn read_profile(&mut self, _profile: &str) -> Result<ProfileValues, ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }

    fn write_profile_verified(
        &mut self,
        _profile: &str,
        _values: ProfileValues,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }

    fn delete_profile_verified(
        &mut self,
        _profile: &str,
        _expected: ProfileValues,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }

    fn limiter_disabled(&mut self) -> Result<bool, ProviderError> {
        Err(ProviderError::Unavailable("rtss_supported_on_windows_only"))
    }
}

#[cfg(windows)]
impl RtssProvider {
    fn api(&mut self) -> Result<&mut WindowsRtssApi, ProviderError> {
        let install_path = self
            .install_path
            .as_deref()
            .ok_or(ProviderError::Unavailable("rtss_install_path_required"))?;
        let active_install_path = discover_install_path()?;
        if active_install_path != install_path {
            self.api = None;
            return Err(ProviderError::Unavailable(
                "rtss_install_path_does_not_match_running_instance",
            ));
        }
        if self.api.is_none() {
            self.api = Some(WindowsRtssApi::load(install_path)?);
        }
        self.api
            .as_mut()
            .ok_or(ProviderError::Unavailable("rtss_api_unavailable"))
    }

    fn profile_path(&self, profile: &str) -> Result<PathBuf, ProviderError> {
        let install_path = self
            .install_path
            .as_deref()
            .ok_or(ProviderError::Unavailable("rtss_install_path_required"))?;
        let profiles = install_path.join("Profiles");
        match std::fs::symlink_metadata(&profiles) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(profiles.join(format!("{profile}.cfg")));
            }
            _ => return Err(ProviderError::ReadFailed("rtss_profile_directory_invalid")),
        }
        Ok(profiles.join(format!("{profile}.cfg")))
    }
}

#[cfg(windows)]
const RTSS_SHARED_MEMORY_NAME: &str = "RTSSSharedMemoryV2";

#[cfg(windows)]
type GetProfileProperty = unsafe extern "system" fn(*const i8, *mut u8, u32) -> i32;
#[cfg(windows)]
type SetProfileProperty = unsafe extern "system" fn(*const i8, *const u8, u32) -> i32;
#[cfg(windows)]
type ProfileOperation = unsafe extern "system" fn(*const i8);
#[cfg(windows)]
type UpdateProfiles = unsafe extern "system" fn();
#[cfg(windows)]
type GetFlags = unsafe extern "system" fn() -> u32;

#[cfg(windows)]
struct WindowsRtssApi {
    library: *mut std::ffi::c_void,
    get_profile_property: GetProfileProperty,
    set_profile_property: SetProfileProperty,
    load_profile: ProfileOperation,
    save_profile: ProfileOperation,
    delete_profile: ProfileOperation,
    update_profiles: UpdateProfiles,
    get_flags: GetFlags,
}

#[cfg(windows)]
impl WindowsRtssApi {
    fn load(install_path: &Path) -> Result<Self, ProviderError> {
        if !rtss_is_running()? {
            return Err(ProviderError::Unavailable("rtss_not_running"));
        }
        let dll_name = if cfg!(target_pointer_width = "64") {
            "RTSSHooks64.dll"
        } else {
            "RTSSHooks.dll"
        };
        let dll_path = install_path.join(dll_name);
        let canonical_dll = std::fs::canonicalize(&dll_path)
            .map_err(|_| ProviderError::Unavailable("rtss_api_dll_not_found"))?;
        if canonical_dll.parent() != Some(install_path) {
            return Err(ProviderError::Unavailable(
                "rtss_api_dll_outside_install_path",
            ));
        }
        let wide_path: Vec<u16> = canonical_dll
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: the path is canonicalized beneath the configured RTSS install
        // directory. Search is limited to that DLL's directory and System32.
        let library = unsafe {
            LoadLibraryExW(
                wide_path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if library.is_null() {
            return Err(ProviderError::Unavailable("rtss_api_load_failed"));
        }

        // SAFETY: every symbol is resolved from the configured RTSSHooks DLL;
        // each pointer is converted only to its documented C ABI signature.
        let result = unsafe {
            Ok(Self {
                library,
                get_profile_property: resolve_symbol(library, b"GetProfileProperty\0")?,
                set_profile_property: resolve_symbol(library, b"SetProfileProperty\0")?,
                load_profile: resolve_symbol(library, b"LoadProfile\0")?,
                save_profile: resolve_symbol(library, b"SaveProfile\0")?,
                delete_profile: resolve_symbol(library, b"DeleteProfile\0")?,
                update_profiles: resolve_symbol(library, b"UpdateProfiles\0")?,
                get_flags: resolve_symbol(library, b"GetFlags\0")?,
            })
        };
        if result.is_err() {
            // SAFETY: library is a valid handle returned by LoadLibraryExW.
            unsafe { FreeLibrary(library) };
        }
        result
    }

    fn load_profile(&mut self, profile: &str) -> Result<(), ProviderError> {
        let profile = std::ffi::CString::new(profile)
            .map_err(|_| ProviderError::ReadFailed("rtss_profile_name_invalid"))?;
        // SAFETY: the profile name is a valid NUL-terminated ANSI string, and
        // the loaded module owns the resolved function pointer.
        unsafe { (self.load_profile)(profile.as_ptr()) };
        Ok(())
    }

    fn read_profile_values(&self) -> Result<ProfileValues, ProviderError> {
        Ok(ProfileValues {
            frame_limit: self.get_i32_property("FramerateLimit")?,
            frame_limit_denominator: self.get_i32_property("FramerateLimitDenominator")?,
            sync_limiter: self.get_i32_property("SyncLimiter")?,
        })
    }

    fn write_profile_values(&self, values: ProfileValues) -> Result<(), ProviderError> {
        self.set_i32_property("FramerateLimit", values.frame_limit)?;
        self.set_i32_property("FramerateLimitDenominator", values.frame_limit_denominator)?;
        self.set_i32_property("SyncLimiter", values.sync_limiter)
    }

    fn save_profile(&self, profile: &str) -> Result<(), ProviderError> {
        let profile = std::ffi::CString::new(profile)
            .map_err(|_| ProviderError::WriteFailed("rtss_profile_name_invalid"))?;
        // SAFETY: the profile name is a valid NUL-terminated ANSI string. This
        // is the RTSS API write path; the module never edits profile files.
        unsafe { (self.save_profile)(profile.as_ptr()) };
        Ok(())
    }

    fn delete_profile(&self, profile: &str) -> Result<(), ProviderError> {
        let profile = std::ffi::CString::new(profile)
            .map_err(|_| ProviderError::WriteFailed("rtss_profile_name_invalid"))?;
        // SAFETY: this is called only for a profile created by this lease after
        // its exact applied values have been read back.
        unsafe { (self.delete_profile)(profile.as_ptr()) };
        Ok(())
    }

    fn update_profiles(&self) {
        // SAFETY: the function pointer belongs to the live RTSSHooks module.
        unsafe { (self.update_profiles)() };
    }

    fn limiter_disabled(&self) -> Result<bool, ProviderError> {
        // SAFETY: GetFlags is a read-only RTSSHooks export with no arguments.
        let flags = unsafe { (self.get_flags)() };
        Ok(flags & LIMITER_DISABLED_FLAG != 0)
    }

    fn get_i32_property(&self, property: &str) -> Result<i32, ProviderError> {
        let property = std::ffi::CString::new(property)
            .map_err(|_| ProviderError::ReadFailed("rtss_property_name_invalid"))?;
        let mut value = 0_i32;
        // SAFETY: value is writable for exactly four bytes, and the property
        // string remains alive through the API call.
        let success = unsafe {
            (self.get_profile_property)(
                property.as_ptr(),
                (&mut value as *mut i32).cast(),
                std::mem::size_of::<i32>() as u32,
            )
        };
        if success == 0 {
            return Err(ProviderError::ReadFailed(
                "rtss_profile_property_unreadable",
            ));
        }
        Ok(value)
    }

    fn set_i32_property(&self, property: &str, value: i32) -> Result<(), ProviderError> {
        let property = std::ffi::CString::new(property)
            .map_err(|_| ProviderError::WriteFailed("rtss_property_name_invalid"))?;
        // SAFETY: value points to a live four-byte integer for the duration of
        // the synchronous RTSS property update call.
        let success = unsafe {
            (self.set_profile_property)(
                property.as_ptr(),
                (&value as *const i32).cast(),
                std::mem::size_of::<i32>() as u32,
            )
        };
        if success == 0 {
            return Err(ProviderError::WriteFailed("rtss_profile_property_rejected"));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsRtssApi {
    fn drop(&mut self) {
        if !self.library.is_null() {
            // SAFETY: this handle is owned by this struct and no function calls
            // are in flight while it is dropped on the single module thread.
            unsafe { FreeLibrary(self.library) };
        }
    }
}

#[cfg(windows)]
fn resolve_install_path(path: &str) -> Result<PathBuf, ProviderError> {
    if path.is_empty() {
        return discover_install_path();
    }
    let active_install = discover_install_path()?;
    let candidate = Path::new(path);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        active_install.join(candidate)
    };
    let canonical = std::fs::canonicalize(candidate)
        .map_err(|_| ProviderError::Unavailable("rtss_install_path_not_found"))?;
    if !canonical.is_dir() {
        return Err(ProviderError::Unavailable("rtss_install_path_invalid"));
    }
    if active_install != canonical {
        return Err(ProviderError::Unavailable(
            "rtss_install_path_does_not_match_running_instance",
        ));
    }
    Ok(canonical)
}

#[cfg(windows)]
fn discover_install_path() -> Result<PathBuf, ProviderError> {
    if !rtss_is_running()? {
        return Err(ProviderError::Unavailable("rtss_not_running"));
    }
    // An empty configured path means auto-detect the running RTSS image and
    // load only the sibling API DLL from that exact canonical installation.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(ProviderError::Unavailable("rtss_process_probe_failed"));
    }
    let result = (|| {
        let mut entry = ProcessEntry32W {
            dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
            ..ProcessEntry32W::default()
        };
        let mut found = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while found {
            let image_name = String::from_utf16_lossy(&entry.sz_exe_file);
            if image_name
                .trim_end_matches('\0')
                .eq_ignore_ascii_case("RTSS.exe")
            {
                let process = unsafe {
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32_process_id)
                };
                if process.is_null() {
                    return Err(ProviderError::Unavailable("rtss_process_path_unavailable"));
                }
                let mut image_path = vec![0_u16; 32768];
                let mut length = image_path.len() as u32;
                // SAFETY: image_path is writable for `length` UTF-16 units; the
                // process handle is opened only for query access.
                let queried = unsafe {
                    QueryFullProcessImageNameW(process, 0, image_path.as_mut_ptr(), &mut length)
                };
                unsafe { CloseHandle(process) };
                if queried == 0 || length == 0 {
                    return Err(ProviderError::Unavailable("rtss_process_path_unavailable"));
                }
                image_path.truncate(length as usize);
                let image = PathBuf::from(std::ffi::OsString::from_wide(&image_path));
                if !image
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("RTSS.exe"))
                {
                    return Err(ProviderError::Unavailable("rtss_process_path_invalid"));
                }
                let directory = image
                    .parent()
                    .ok_or(ProviderError::Unavailable("rtss_install_path_invalid"))?;
                return std::fs::canonicalize(directory)
                    .map_err(|_| ProviderError::Unavailable("rtss_install_path_not_found"));
            }
            found = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        Err(ProviderError::Unavailable("rtss_process_not_found"))
    })();
    unsafe { CloseHandle(snapshot) };
    result
}

#[cfg(windows)]
fn validate_profile_name(profile: &str) -> Result<(), ProviderError> {
    let safe = profile.ends_with(".exe")
        && profile.len() <= 255
        && profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if safe {
        Ok(())
    } else {
        Err(ProviderError::Unavailable("rtss_profile_name_unsupported"))
    }
}

#[cfg(windows)]
fn rtss_is_running() -> Result<bool, ProviderError> {
    let name: Vec<u16> = RTSS_SHARED_MEMORY_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the mapping name is a fixed, valid NUL-terminated string. The
    // handle is closed immediately and never mapped or modified.
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
    if handle.is_null() {
        return Ok(false);
    }
    // SAFETY: this closes only the handle returned by OpenFileMappingW above.
    unsafe { CloseHandle(handle) };
    Ok(true)
}

#[cfg(windows)]
unsafe fn resolve_symbol<T: Copy>(
    library: *mut std::ffi::c_void,
    symbol_name: &[u8],
) -> Result<T, ProviderError> {
    // SAFETY: callers pass a live module handle and a NUL-terminated symbol name.
    let symbol = unsafe { GetProcAddress(library, symbol_name.as_ptr().cast()) };
    let Some(symbol) = symbol else {
        return Err(ProviderError::Unavailable("rtss_api_symbol_missing"));
    };
    // SAFETY: each call site requests the exact RTSS C ABI function signature.
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

#[cfg(windows)]
const FILE_MAP_READ: u32 = 0x0004;
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
#[cfg(windows)]
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
#[cfg(windows)]
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1_isize as *mut std::ffi::c_void;

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

#[cfg(windows)]
impl Default for ProcessEntry32W {
    fn default() -> Self {
        Self {
            dw_size: 0,
            cnt_usage: 0,
            th32_process_id: 0,
            th32_default_heap_id: 0,
            th32_module_id: 0,
            cnt_threads: 0,
            th32_parent_process_id: 0,
            pc_pri_class_base: 0,
            dw_flags: 0,
            sz_exe_file: [0; 260],
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(
        file_name: *const u16,
        file: *mut std::ffi::c_void,
        flags: u32,
    ) -> *mut std::ffi::c_void;
    fn GetProcAddress(
        module: *mut std::ffi::c_void,
        name: *const i8,
    ) -> Option<unsafe extern "system" fn()>;
    fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
    fn OpenFileMappingW(
        desired_access: u32,
        inherit_handle: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut std::ffi::c_void;
    fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
    fn OpenProcess(
        desired_access: u32,
        inherit_handle: i32,
        process_id: u32,
    ) -> *mut std::ffi::c_void;
    fn QueryFullProcessImageNameW(
        process: *mut std::ffi::c_void,
        flags: u32,
        image_name: *mut u16,
        size: *mut u32,
    ) -> i32;
}
