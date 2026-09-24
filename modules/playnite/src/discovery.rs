//! Playnite process discovery used to avoid probing its extension pipe when no
//! desktop or fullscreen client is running.

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Discovery {
    pub running: bool,
    pub processes: Vec<ProcessImage>,
    pub executable_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessImage {
    pub process_id: u32,
    pub path: String,
}

#[cfg(windows)]
pub fn discover() -> Discovery {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    // SAFETY: CreateToolhelp32Snapshot returns an owned kernel handle which is
    // closed below. PROCESSENTRY32W is initialized with its required size.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == -1isize as HANDLE {
        return Discovery::default();
    }

    let mut found = Discovery::default();
    // SAFETY: zeroed is valid for PROCESSENTRY32W; dwSize is set before the API.
    let mut process: PROCESSENTRY32W = unsafe { zeroed() };
    process.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: snapshot is a valid process snapshot and process points to a sized buffer.
    let mut has_process = unsafe { Process32FirstW(snapshot, &mut process) } != 0;
    while has_process {
        let name_length = process
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(process.szExeFile.len());
        let executable_name = String::from_utf16_lossy(&process.szExeFile[..name_length]);
        if executable_name.eq_ignore_ascii_case("Playnite.DesktopApp.exe")
            || executable_name.eq_ignore_ascii_case("Playnite.FullscreenApp.exe")
        {
            // SAFETY: the process ID came from the snapshot; query access is read-only.
            let process_handle =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.th32ProcessID) };
            if !process_handle.is_null() {
                let mut image_path = [0u16; 32_768];
                let mut length = image_path.len() as u32;
                // SAFETY: the handle is valid and image_path is writable for length units.
                let queried = unsafe {
                    QueryFullProcessImageNameW(
                        process_handle,
                        0,
                        image_path.as_mut_ptr(),
                        &mut length,
                    )
                };
                if queried != 0 {
                    let path = String::from_utf16_lossy(&image_path[..length as usize]);
                    if valid_playnite_image(&path, &executable_name)
                        && !found
                            .processes
                            .iter()
                            .any(|existing| existing.process_id == process.th32ProcessID)
                    {
                        found.running = true;
                        found.processes.push(ProcessImage {
                            process_id: process.th32ProcessID,
                            path: path.clone(),
                        });
                        found.executable_paths.push(path);
                    }
                }
                // SAFETY: process_handle is owned by this function.
                unsafe { CloseHandle(process_handle) };
            }
        }
        // SAFETY: snapshot remains valid and process is a writable entry buffer.
        has_process = unsafe { Process32NextW(snapshot, &mut process) } != 0;
    }
    // SAFETY: snapshot is the owned handle created above.
    unsafe { CloseHandle(snapshot) };
    found
        .executable_paths
        .sort_unstable_by_key(|path| path.to_lowercase());
    found
        .processes
        .sort_unstable_by_key(|process| process.process_id);
    found
}

#[cfg(windows)]
fn valid_playnite_image(path: &str, expected_name: &str) -> bool {
    use std::path::Path;
    let path = Path::new(path);
    path.is_absolute()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

#[cfg(not(windows))]
pub fn discover() -> Discovery {
    Discovery::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_defaults_to_no_running_processes() {
        let discovery = Discovery::default();
        assert!(!discovery.running);
        assert!(discovery.processes.is_empty());
        assert!(discovery.executable_paths.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn process_image_validation_checks_the_absolute_executable_path() {
        assert!(valid_playnite_image(
            r"C:\Program Files\Playnite\Playnite.DesktopApp.exe",
            "Playnite.DesktopApp.exe"
        ));
        assert!(!valid_playnite_image(
            r"C:\Temp\Playnite.DesktopApp.exe",
            "Playnite.FullscreenApp.exe"
        ));
        assert!(!valid_playnite_image(
            r"C:\Temp\..\Playnite.DesktopApp.exe",
            "Playnite.DesktopApp.exe"
        ));
    }
}
