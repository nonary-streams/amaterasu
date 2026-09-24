//! Resolves Playnite only through the current Windows `playnite://` association.

use crate::discovery::ProcessImage;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Installation {
    launch_executable: PathBuf,
    launch_with_fullscreen_argument: bool,
    fullscreen_executable: PathBuf,
    install_directory: PathBuf,
}

impl Installation {
    fn from_associated_executable(path: &Path) -> Option<Self> {
        let path_text = path.to_string_lossy();
        if path_text.len() > 32_768
            || path_text.chars().any(char::is_control)
            || path_text.contains('"')
            || !valid_associated_executable_name(path.file_name()?.to_str()?)
            || !path.is_absolute()
        {
            return None;
        }
        let associated_executable = std::fs::canonicalize(path).ok()?;
        if !associated_executable.is_file()
            || !valid_associated_executable_name(associated_executable.file_name()?.to_str()?)
        {
            return None;
        }
        let install_directory = associated_executable.parent()?.to_path_buf();
        let associated_name = associated_executable.file_name()?.to_str()?;
        let desktop_candidate = install_directory.join("Playnite.DesktopApp.exe");
        let desktop_executable =
            std::fs::canonicalize(desktop_candidate)
                .ok()
                .filter(|candidate| {
                    candidate.is_file()
                        && same_installation(&install_directory, candidate)
                        && candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.eq_ignore_ascii_case("Playnite.DesktopApp.exe")
                            })
                });
        let (launch_executable, launch_with_fullscreen_argument) =
            if let Some(desktop_executable) = desktop_executable {
                (desktop_executable, true)
            } else if associated_name.eq_ignore_ascii_case("Playnite.DesktopApp.exe") {
                (associated_executable.clone(), true)
            } else {
                (associated_executable.clone(), false)
            };
        let fullscreen_executable =
            std::fs::canonicalize(install_directory.join("Playnite.FullscreenApp.exe"))
                .ok()
                .filter(|candidate| {
                    candidate.is_file()
                        && same_installation(&install_directory, candidate)
                        && candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.eq_ignore_ascii_case("Playnite.FullscreenApp.exe")
                            })
                })?;
        Some(Self {
            launch_executable,
            launch_with_fullscreen_argument,
            fullscreen_executable,
            install_directory,
        })
    }

    pub fn discover_associated() -> Option<Self> {
        #[cfg(windows)]
        {
            let associated_executable = query_playnite_association()?;
            Self::from_associated_executable(&associated_executable)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    pub fn matches_running_processes(&self, processes: &[ProcessImage]) -> bool {
        processes.is_empty()
            || processes
                .iter()
                .all(|process| same_installation(&self.install_directory, Path::new(&process.path)))
    }

    pub fn fullscreen_processes<'a>(&self, processes: &'a [ProcessImage]) -> Vec<&'a ProcessImage> {
        processes
            .iter()
            .filter(|process| {
                normalized_path(&process.path)
                    == normalized_path(&self.fullscreen_executable.to_string_lossy())
            })
            .collect()
    }

    pub fn launch_process_is_running(&self, process_id: u32, processes: &[ProcessImage]) -> bool {
        processes.iter().any(|process| {
            process.process_id == process_id
                && normalized_path(&process.path)
                    == normalized_path(&self.launch_executable.to_string_lossy())
        })
    }

    pub fn focus_target(&self) -> crate::focus::FocusTarget {
        crate::focus::FocusTarget::from_status_paths(
            self.install_directory.to_str(),
            self.fullscreen_executable.to_str(),
        )
    }

    #[cfg(windows)]
    pub fn launch_fullscreen(&self) -> Result<u32, &'static str> {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null;
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
            PROCESS_INFORMATION, STARTUPINFOW,
        };

        let executable: Vec<u16> = self
            .launch_executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut command_line: Vec<u16> = self.launch_executable.as_os_str().encode_wide().collect();
        let mut quoted_command_line = Vec::with_capacity(command_line.len() + 24);
        quoted_command_line.push(b'"' as u16);
        quoted_command_line.append(&mut command_line);
        quoted_command_line.push(b'"' as u16);
        if self.launch_with_fullscreen_argument {
            quoted_command_line.extend(" --startfullscreen".encode_utf16());
        }
        quoted_command_line.push(0);
        let current_directory: Vec<u16> = self
            .install_directory
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: the executable and command line are NUL-terminated UTF-16
        // buffers owned for the duration of CreateProcessW. No shell is used,
        // no handles are inherited, and the working directory is the resolved
        // installation directory.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        // SAFETY: process_information is writable output and all pointer inputs
        // remain valid for this synchronous call.
        let mut process_information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                executable.as_ptr(),
                quoted_command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS,
                null(),
                current_directory.as_ptr(),
                &startup,
                &mut process_information,
            )
        };
        if created == 0 {
            return Err("fullscreen_launch_failed");
        }
        // SAFETY: CreateProcessW returned owned thread/process handles; this
        // provider retains only the PID and revalidates its image during polling.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(process_information.hThread);
            windows_sys::Win32::Foundation::CloseHandle(process_information.hProcess);
        }
        Ok(process_information.dwProcessId)
    }

    #[cfg(not(windows))]
    pub fn launch_fullscreen(&self) -> Result<u32, &'static str> {
        Err("unsupported_platform")
    }

    #[cfg(windows)]
    pub fn request_fullscreen_close(
        &self,
        processes: &[ProcessImage],
    ) -> Result<usize, &'static str> {
        windows::request_close(self, processes)
    }

    #[cfg(not(windows))]
    pub fn request_fullscreen_close(
        &self,
        _processes: &[ProcessImage],
    ) -> Result<usize, &'static str> {
        Err("unsupported_platform")
    }

    fn expected_fullscreen_path(&self) -> &Path {
        &self.fullscreen_executable
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            launch_executable: PathBuf::from(r"C:\Playnite\Playnite.DesktopApp.exe"),
            launch_with_fullscreen_argument: true,
            fullscreen_executable: PathBuf::from(r"C:\Playnite\Playnite.FullscreenApp.exe"),
            install_directory: PathBuf::from(r"C:\Playnite"),
        }
    }
}

fn valid_associated_executable_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("Playnite.DesktopApp.exe")
        || name.eq_ignore_ascii_case("Playnite.FullscreenApp.exe")
}

fn same_installation(install_directory: &Path, executable: &Path) -> bool {
    let Some(parent) = executable.parent() else {
        return false;
    };
    let Some(name) = executable.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    valid_associated_executable_name(name)
        && normalized_path(&parent.to_string_lossy())
            == normalized_path(&install_directory.to_string_lossy())
}

fn normalized_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(windows)]
fn query_playnite_association() -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell::{ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW};

    let association: Vec<u16> = "playnite"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut output = vec![0u16; 32_768];
    let mut length = output.len() as u32;
    // SAFETY: association and output are NUL-terminated/writable UTF-16 buffers;
    // the API is restricted to resolving the OS-registered executable handler.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_EXECUTABLE,
            association.as_ptr(),
            std::ptr::null(),
            output.as_mut_ptr(),
            &mut length,
        )
    };
    if result < 0 || length == 0 || length as usize > output.len() {
        return None;
    }
    let terminator = output[..length as usize]
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(length as usize);
    let value = String::from_utf16(&output[..terminator]).ok()?;
    if value.is_empty() || value.chars().any(char::is_control) || value.contains('"') {
        return None;
    }
    Some(PathBuf::from(value))
}

#[cfg(windows)]
mod windows {
    use super::{Installation, normalized_path};
    use crate::discovery::ProcessImage;
    use std::collections::BTreeSet;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };
    use windows_sys::core::BOOL;

    struct CloseSearch {
        expected_path: String,
        process_ids: BTreeSet<u32>,
        visited: usize,
        closed_windows: usize,
    }

    pub fn request_close(
        installation: &Installation,
        processes: &[ProcessImage],
    ) -> Result<usize, &'static str> {
        let process_ids: BTreeSet<u32> = installation
            .fullscreen_processes(processes)
            .into_iter()
            .map(|process| process.process_id)
            .collect();
        if process_ids.is_empty() {
            return Ok(0);
        }
        let mut search = CloseSearch {
            expected_path: normalized_path(
                &installation.expected_fullscreen_path().to_string_lossy(),
            ),
            process_ids,
            visited: 0,
            closed_windows: 0,
        };
        // SAFETY: EnumWindows calls the callback synchronously while the search
        // context remains exclusively borrowed and valid.
        unsafe { EnumWindows(Some(close_matching_window), &mut search as *mut _ as LPARAM) };
        if search.visited > 512 {
            return Err("window_enumeration_limit");
        }
        Ok(search.closed_windows)
    }

    unsafe extern "system" fn close_matching_window(window: HWND, context: LPARAM) -> BOOL {
        // SAFETY: context points to the live CloseSearch supplied to synchronous
        // EnumWindows above.
        let search = unsafe { &mut *(context as *mut CloseSearch) };
        search.visited += 1;
        if search.visited > 512 {
            return 0;
        }
        let mut process_id = 0;
        // SAFETY: window is supplied by EnumWindows for this callback.
        if unsafe { GetWindowThreadProcessId(window, &mut process_id) } == 0
            || !search.process_ids.contains(&process_id)
            // SAFETY: window is valid for the callback duration.
            || !unsafe { GetWindow(window, GW_OWNER) }.is_null()
        {
            return 1;
        }
        if query_image_path(process_id)
            .as_deref()
            .is_none_or(|path| normalized_path(path) != search.expected_path)
        {
            return 1;
        }
        // SAFETY: image identity was revalidated after enumerating the window;
        // PostMessageW is asynchronous and cannot block this service tick.
        if unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0 {
            search.closed_windows += 1;
        }
        1
    }

    fn query_image_path(process_id: u32) -> Option<String> {
        // SAFETY: process_id comes from a current window; query access cannot
        // write to or launch the process.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return None;
        }
        let mut path = [0u16; 32_768];
        let mut length = path.len() as u32;
        // SAFETY: process is valid and path is writable for length UTF-16 units.
        let queried =
            unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
        // SAFETY: this function owns the process handle returned by OpenProcess.
        unsafe { CloseHandle(process) };
        if queried == 0 || length as usize > path.len() {
            return None;
        }
        Some(String::from_utf16_lossy(&path[..length as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::{Installation, same_installation, valid_associated_executable_name};
    use crate::discovery::ProcessImage;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn association_accepts_only_fixed_playnite_executable_names() {
        assert!(valid_associated_executable_name("Playnite.DesktopApp.exe"));
        assert!(valid_associated_executable_name(
            "playnite.fullscreenapp.EXE"
        ));
        assert!(!valid_associated_executable_name("Playnite.exe"));
        assert!(!valid_associated_executable_name("cmd.exe"));
    }

    #[test]
    fn running_playnite_processes_must_match_the_associated_installation() {
        let directory = PathBuf::from(r"C:\Users\player\AppData\Local\Playnite");
        assert!(same_installation(
            &directory,
            &PathBuf::from(r"C:\Users\player\AppData\Local\Playnite\Playnite.DesktopApp.exe")
        ));
        assert!(!same_installation(
            &directory,
            &PathBuf::from(r"D:\Other\Playnite\Playnite.DesktopApp.exe")
        ));
        assert!(!same_installation(
            &directory,
            &PathBuf::from(r"C:\Users\player\AppData\Local\Playnite\cmd.exe")
        ));
    }

    #[test]
    fn fullscreen_target_is_tied_to_associated_install_root() {
        let installation = Installation::for_test();
        assert!(installation.matches_running_processes(&[]));
        assert!(installation.matches_running_processes(&[ProcessImage {
            process_id: 12,
            path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
        }]));
        assert!(!installation.matches_running_processes(&[
            ProcessImage {
                process_id: 12,
                path: r"C:\Playnite\Playnite.DesktopApp.exe".into(),
            },
            ProcessImage {
                process_id: 13,
                path: r"D:\Playnite\Playnite.FullscreenApp.exe".into(),
            }
        ]));
        assert_eq!(
            installation
                .fullscreen_processes(&[ProcessImage {
                    process_id: 13,
                    path: r"D:\Playnite\Playnite.FullscreenApp.exe".into(),
                }])
                .len(),
            0
        );
    }

    #[test]
    fn associated_installation_requires_existing_canonical_fullscreen_executable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-playnite-install-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let desktop = directory.join("Playnite.DesktopApp.exe");
        std::fs::write(&desktop, b"desktop fixture").unwrap();

        assert!(Installation::from_associated_executable(&desktop).is_none());

        let fullscreen = directory.join("Playnite.FullscreenApp.exe");
        std::fs::write(&fullscreen, b"fullscreen fixture").unwrap();
        let installation = Installation::from_associated_executable(&desktop).unwrap();
        assert_eq!(
            installation.launch_executable,
            std::fs::canonicalize(&desktop).unwrap()
        );
        assert_eq!(
            installation.fullscreen_executable,
            std::fs::canonicalize(&fullscreen).unwrap()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
