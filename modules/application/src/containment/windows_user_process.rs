//! Native user-token process creation for Windows hosts running as SYSTEM.
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_DUPLICATE, TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN,
    TOKEN_QUERY, TOKEN_USER, TokenElevationType, TokenElevationTypeDefault, TokenElevationTypeFull,
    TokenElevationTypeLimited, TokenLinkedToken, TokenUser, WinLocalSystemSid,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, ResumeThread,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

const MAX_TOKEN_INFORMATION_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_UNITS: usize = 32_767;
const ELEVATION_TYPE_DEFAULT: TOKEN_ELEVATION_TYPE = TokenElevationTypeDefault;
const ELEVATION_TYPE_FULL: TOKEN_ELEVATION_TYPE = TokenElevationTypeFull;
const ELEVATION_TYPE_LIMITED: TOKEN_ELEVATION_TYPE = TokenElevationTypeLimited;

pub(crate) struct NativeProcess {
    process: OwnedHandle,
    process_id: u32,
}

impl NativeProcess {
    pub fn id(&self) -> u32 {
        self.process_id
    }

    pub fn raw_handle(&self) -> HANDLE {
        self.process.as_raw_handle()
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        // SAFETY: this is the live, exclusively owned process handle created below.
        if unsafe { TerminateProcess(self.raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        // SAFETY: the handle is live and the zero timeout only polls its signaled state.
        match unsafe { WaitForSingleObject(self.raw_handle(), 0) } {
            WAIT_TIMEOUT => return Ok(None),
            WAIT_OBJECT_0 => {}
            _ => return Err(io::Error::last_os_error()),
        }

        let mut exit_code = 0u32;
        // SAFETY: the process handle remains owned and `exit_code` is writable.
        if unsafe { GetExitCodeProcess(self.raw_handle(), &mut exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        use std::os::windows::process::ExitStatusExt;
        Ok(Some(std::process::ExitStatus::from_raw(exit_code)))
    }
}

pub(crate) struct CreatedUserProcess {
    pub process: NativeProcess,
    pub primary_thread: OwnedHandle,
}

/// Return an active console user's token only when this module host is SYSTEM.
/// Non-SYSTEM hosts retain their existing process identity, matching the source.
pub fn active_user_token_for_system_host(elevated: bool) -> io::Result<Option<OwnedHandle>> {
    let process_token = open_current_process_token()?;
    if !is_system_token(process_token.as_raw_handle())? {
        return Ok(None);
    }

    // WTSGetActiveConsoleSessionId returns u32::MAX when no console user is active.
    // Never fall back to the service's SYSTEM identity for an app launch.
    let session_id = active_console_session_id(unsafe { WTSGetActiveConsoleSessionId() })?;
    let mut active_user = std::ptr::null_mut();
    // SAFETY: `active_user` is writable storage for the token handle returned by WTS.
    if unsafe { WTSQueryUserToken(session_id, &mut active_user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let active_user = owned_handle(active_user)?;

    if elevated {
        let elevation_type = token_elevation_type(active_user.as_raw_handle())?;
        match elevation_token_choice(elevation_type, elevated)? {
            ElevationTokenChoice::KeepCurrent => {}
            ElevationTokenChoice::UseLinked => {
                let linked = linked_token(active_user.as_raw_handle())?;
                return Ok(Some(linked));
            }
        }
    }
    Ok(Some(active_user))
}

fn active_console_session_id(session_id: u32) -> io::Result<u32> {
    if session_id == u32::MAX {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no active console user session is available",
        ));
    }
    Ok(session_id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElevationTokenChoice {
    KeepCurrent,
    UseLinked,
}

fn elevation_token_choice(
    elevation_type: TOKEN_ELEVATION_TYPE,
    requested: bool,
) -> io::Result<ElevationTokenChoice> {
    if !requested {
        return Ok(ElevationTokenChoice::KeepCurrent);
    }
    match elevation_type {
        ELEVATION_TYPE_LIMITED => Ok(ElevationTokenChoice::UseLinked),
        // Full tokens already have the requested rights. Default tokens include
        // standard users, for whom the source warns and keeps the same token.
        ELEVATION_TYPE_FULL | ELEVATION_TYPE_DEFAULT => Ok(ElevationTokenChoice::KeepCurrent),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "active user's token has an unsupported elevation type",
        )),
    }
}

pub fn create_suspended_user_process(
    command: &Command,
    token: HANDLE,
) -> io::Result<CreatedUserProcess> {
    create_user_process(command, token, true)
}

pub fn create_uncontained_user_process(
    command: &Command,
    token: HANDLE,
) -> io::Result<NativeProcess> {
    let created = create_user_process(command, token, false)?;
    drop(created.primary_thread);
    Ok(created.process)
}

fn create_user_process(
    command: &Command,
    token: HANDLE,
    suspended: bool,
) -> io::Result<CreatedUserProcess> {
    let executable = wide_nul(command.get_program())?;
    let mut command_line = command_line(command)?;
    let mut environment = user_environment(token, command)?;
    let working_directory = command
        .get_current_dir()
        .map(|directory| wide_nul(directory.as_os_str()))
        .transpose()?;

    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    let flags = CREATE_UNICODE_ENVIRONMENT
        | CREATE_NO_WINDOW
        | if suspended { CREATE_SUSPENDED } else { 0 };
    // SAFETY: all wide strings are NUL-terminated and remain alive for this call;
    // command_line and environment are mutable/complete Windows buffers; STARTUPINFOW
    // and PROCESS_INFORMATION are ABI-matched initialized structs. No handles inherit.
    let succeeded = unsafe {
        CreateProcessAsUserW(
            token,
            executable.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            environment.as_mut_ptr().cast(),
            working_directory
                .as_ref()
                .map_or(std::ptr::null(), |directory| directory.as_ptr()),
            &startup,
            &mut information,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    if information.hProcess.is_null()
        || information.hProcess == INVALID_HANDLE_VALUE
        || information.hThread.is_null()
        || information.hThread == INVALID_HANDLE_VALUE
    {
        // SAFETY: any non-null handles were just returned by CreateProcessAsUserW.
        if !information.hProcess.is_null() && information.hProcess != INVALID_HANDLE_VALUE {
            unsafe {
                let _ = TerminateProcess(information.hProcess, 1);
                windows_sys::Win32::Foundation::CloseHandle(information.hProcess);
            }
        }
        if !information.hThread.is_null() && information.hThread != INVALID_HANDLE_VALUE {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(information.hThread) };
        }
        return Err(io::Error::other(
            "Windows returned incomplete process handles",
        ));
    }

    // The two newly returned handles are exclusively owned here and are closed by
    // OwnedHandle on every return path. The caller resumes only after job assignment.
    let process = owned_handle(information.hProcess)?;
    let primary_thread = owned_handle(information.hThread)?;
    Ok(CreatedUserProcess {
        process: NativeProcess {
            process,
            process_id: information.dwProcessId,
        },
        primary_thread,
    })
}

pub fn resume_user_process(thread: &OwnedHandle) -> io::Result<()> {
    // SAFETY: this is the retained primary thread handle returned by process creation.
    let previous_count = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous_count == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    if previous_count != 1 {
        return Err(io::Error::other("unexpected primary thread suspend count"));
    }
    Ok(())
}

fn open_current_process_token() -> io::Result<OwnedHandle> {
    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle; `token` is writable output.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    owned_handle(token)
}

fn is_system_token(token: HANDLE) -> io::Result<bool> {
    let information = token_information(token, TokenUser)?;
    if information.len() * std::mem::size_of::<usize>() < std::mem::size_of::<TOKEN_USER>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a truncated token user record",
        ));
    }
    // SAFETY: token_information uses usize-aligned storage, the successful Windows
    // query initialized the TOKEN_USER prefix, and the bounds check covers its size.
    let user = unsafe { &*(information.as_ptr().cast::<TOKEN_USER>()) };
    // SAFETY: TOKEN_USER.Sid points into the still-live information buffer.
    Ok(unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) } != 0)
}

fn token_elevation_type(token: HANDLE) -> io::Result<TOKEN_ELEVATION_TYPE> {
    token_information_value(token, TokenElevationType)
}

fn linked_token(token: HANDLE) -> io::Result<OwnedHandle> {
    let mut information = TOKEN_LINKED_TOKEN::default();
    let mut returned = 0u32;
    // SAFETY: the token is live and the writable buffer matches TokenLinkedToken.
    if unsafe {
        GetTokenInformation(
            token,
            TokenLinkedToken,
            (&mut information as *mut TOKEN_LINKED_TOKEN).cast(),
            std::mem::size_of_val(&information) as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    owned_handle(information.LinkedToken)
}

fn token_information_value<T: Copy>(token: HANDLE, class: i32) -> io::Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut returned = 0u32;
    // SAFETY: `value` is suitably aligned writable storage with the exact ABI size.
    if unsafe {
        GetTokenInformation(
            token,
            class,
            value.as_mut_ptr().cast(),
            std::mem::size_of::<T>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation succeeded and initialized the requested value.
    Ok(unsafe { value.assume_init() })
}

fn token_information(token: HANDLE, class: i32) -> io::Result<Vec<usize>> {
    let mut required = 0u32;
    // SAFETY: a null buffer with zero length asks Windows for the required size.
    unsafe {
        GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut required);
    }
    let required = required as usize;
    if required == 0 || required > MAX_TOKEN_INFORMATION_BYTES {
        return Err(io::Error::last_os_error());
    }
    let mut aligned = vec![0usize; required.div_ceil(std::mem::size_of::<usize>())];
    let mut returned = 0u32;
    // SAFETY: the usize vector supplies aligned writable storage of at least the
    // requested byte count, and the token is retained by the caller.
    if unsafe {
        GetTokenInformation(
            token,
            class,
            aligned.as_mut_ptr().cast(),
            (aligned.len() * std::mem::size_of::<usize>()) as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    aligned.truncate((returned as usize).div_ceil(std::mem::size_of::<usize>()));
    Ok(aligned)
}

fn user_environment(token: HANDLE, command: &Command) -> io::Result<Vec<u16>> {
    let mut raw_environment = std::ptr::null_mut();
    // SAFETY: token remains live and `raw_environment` is writable output. FALSE
    // requests the target user's profile environment without host inheritance.
    if unsafe { CreateEnvironmentBlock(&mut raw_environment, token, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if raw_environment.is_null() {
        return Err(io::Error::other("Windows returned a null user environment"));
    }
    let environment = UserEnvironment(raw_environment);
    let mut entries = read_environment_block(environment.0)?;
    for (name, value) in command.get_envs() {
        let Some(value) = value else {
            continue;
        };
        let name = os_wide(name);
        let value = os_wide(value);
        if name.is_empty()
            || name.contains(&0)
            || value.contains(&0)
            || name.contains(&(b'=' as u16))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured environment contains an invalid name or NUL",
            ));
        }
        let key = name.clone();
        entries.retain(|entry| !environment_key(entry).is_some_and(|old| wide_ascii_eq(old, &key)));
        let mut entry = name;
        entry.push(b'=' as u16);
        entry.extend(value);
        entries.push(entry);
    }
    serialize_environment(entries)
}

struct UserEnvironment(*mut core::ffi::c_void);

impl Drop for UserEnvironment {
    fn drop(&mut self) {
        // SAFETY: this pointer is the exact environment block returned by
        // CreateEnvironmentBlock and has not been freed or transferred.
        unsafe {
            let _ = DestroyEnvironmentBlock(self.0);
        }
    }
}

fn read_environment_block(block: *mut core::ffi::c_void) -> io::Result<Vec<Vec<u16>>> {
    let words = block.cast::<u16>();
    let mut entries = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset >= MAX_ENVIRONMENT_UNITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "target user environment exceeds the Windows process limit",
            ));
        }
        // SAFETY: Windows returns a NUL-terminated environment block. The bounded
        // scan reads only through the documented maximum and the enclosing RAII
        // owner keeps the block allocated for this function.
        let mut end = offset;
        while end < MAX_ENVIRONMENT_UNITS && unsafe { *words.add(end) } != 0 {
            end += 1;
        }
        if end >= MAX_ENVIRONMENT_UNITS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "target user environment is not terminated within its bound",
            ));
        }
        if end == offset {
            return Ok(entries);
        }
        // SAFETY: the bounded scan established this range is within the block.
        let entry = unsafe { std::slice::from_raw_parts(words.add(offset), end - offset) };
        if environment_key(entry).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "target user environment contains a malformed entry",
            ));
        }
        entries.push(entry.to_vec());
        offset = end + 1;
    }
}

fn environment_key(entry: &[u16]) -> Option<&[u16]> {
    let start = usize::from(entry.first() == Some(&(b'=' as u16)));
    let separator = entry
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, unit)| (*unit == b'=' as u16).then_some(index))?;
    (separator > start).then_some(&entry[..separator])
}

fn serialize_environment(mut entries: Vec<Vec<u16>>) -> io::Result<Vec<u16>> {
    entries.sort_by(|left, right| {
        wide_ascii_cmp(
            environment_key(left).unwrap_or(left),
            environment_key(right).unwrap_or(right),
        )
    });
    let total = entries
        .iter()
        .map(|entry| entry.len().saturating_add(1))
        .sum::<usize>()
        .saturating_add(1);
    if total > MAX_ENVIRONMENT_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configured environment exceeds the Windows process limit",
        ));
    }
    let mut block = Vec::with_capacity(total);
    for entry in entries {
        block.extend(entry);
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn wide_ascii_eq(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| fold_ascii(*left) == fold_ascii(*right))
}

fn wide_ascii_cmp(left: &[u16], right: &[u16]) -> std::cmp::Ordering {
    left.iter()
        .map(|unit| fold_ascii(*unit))
        .cmp(right.iter().map(|unit| fold_ascii(*unit)))
}

fn fold_ascii(unit: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&unit) {
        unit + (b'a' - b'A') as u16
    } else {
        unit
    }
}

fn command_line(command: &Command) -> io::Result<Vec<u16>> {
    let program = os_wide(command.get_program());
    let mut output = quote_argument(&program, true);
    for argument in command.get_args() {
        let argument = os_wide(argument);
        if argument.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows process argument contains NUL",
            ));
        }
        output.push(b' ' as u16);
        output.extend(quote_argument(&argument, false));
    }
    if output.len().saturating_add(1) > MAX_ENVIRONMENT_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows command line exceeds 32767 UTF-16 code units",
        ));
    }
    output.push(0);
    Ok(output)
}

fn quote_argument(argument: &[u16], force: bool) -> Vec<u16> {
    let quoted = force
        || argument.is_empty()
        || argument
            .iter()
            .any(|unit| matches!(*unit, 0x20 | 0x09 | 0x22));
    if !quoted {
        return argument.to_vec();
    }
    let mut output = Vec::with_capacity(argument.len().saturating_add(2));
    output.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in argument {
        if *unit == b'\\' as u16 {
            backslashes += 1;
            continue;
        }
        if *unit == b'"' as u16 {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            output.push(*unit);
        } else {
            output.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            output.push(*unit);
        }
        backslashes = 0;
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    output.push(b'"' as u16);
    output
}

fn os_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().collect()
}

fn wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = os_wide(value);
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows process path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn owned_handle(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: every caller passes a newly returned, exclusively owned Win32 handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_keeps_standard_users_and_uses_linked_token_for_uac_admins() {
        assert_eq!(
            elevation_token_choice(TokenElevationTypeDefault, true).unwrap(),
            ElevationTokenChoice::KeepCurrent
        );
        assert_eq!(
            elevation_token_choice(TokenElevationTypeFull, true).unwrap(),
            ElevationTokenChoice::KeepCurrent
        );
        assert_eq!(
            elevation_token_choice(TokenElevationTypeLimited, true).unwrap(),
            ElevationTokenChoice::UseLinked
        );
        assert_eq!(
            elevation_token_choice(TokenElevationTypeLimited, false).unwrap(),
            ElevationTokenChoice::KeepCurrent
        );
        assert!(elevation_token_choice(99, true).is_err());
    }

    #[test]
    fn missing_active_session_is_a_launch_error_without_system_fallback() {
        assert_eq!(
            active_console_session_id(u32::MAX).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(active_console_session_id(7).unwrap(), 7);
    }

    #[test]
    fn windows_arguments_quote_spaces_quotes_and_trailing_backslashes() {
        let quote = |value: &str, force| {
            String::from_utf16(&quote_argument(
                &value.encode_utf16().collect::<Vec<_>>(),
                force,
            ))
            .unwrap()
        };
        assert_eq!(
            quote("C:\\Games\\game.exe", true),
            "\"C:\\Games\\game.exe\""
        );
        assert_eq!(quote("plain", false), "plain");
        assert_eq!(quote("two words", false), "\"two words\"");
        assert_eq!(quote("say\"hi", false), "\"say\\\"hi\"");
        assert_eq!(quote("C:\\a b\\", false), "\"C:\\a b\\\\\"");
    }

    #[test]
    fn native_command_line_rejects_embedded_nul_arguments() {
        let mut command = Command::new("C:\\Games\\game.exe");
        command.arg("valid").arg("bad\0argument");
        assert_eq!(
            command_line(&command).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn explicit_environment_overrides_are_case_insensitive_and_sorted() {
        let mut entries = vec!["Path=base".encode_utf16().collect::<Vec<_>>()];
        entries.retain(|entry| {
            !environment_key(entry)
                .is_some_and(|key| wide_ascii_eq(key, &"PATH".encode_utf16().collect::<Vec<_>>()))
        });
        entries.push("PATH=custom".encode_utf16().collect());
        entries.push("SystemRoot=C:\\Windows".encode_utf16().collect());
        let block = serialize_environment(entries).unwrap();
        let expected = "PATH=custom".encode_utf16().collect::<Vec<_>>();
        assert!(block.windows(expected.len()).any(|entry| entry == expected));
        assert_eq!(block.last(), Some(&0));
        assert_eq!(block.get(block.len().saturating_sub(2)), Some(&0));
    }
}
