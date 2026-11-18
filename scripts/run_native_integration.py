#!/usr/bin/env python3
"""Run the platform fixtures without administrator privileges on hosted Windows."""
from __future__ import annotations

import ctypes as ct
from ctypes import wintypes as wt
import os
from pathlib import Path
import subprocess
import sys
import tempfile


INTEGRATION_SCRIPTS = {
    "test_component_updates.py",
    "test_platform_modules.py",
    "test_host_service.py",
    "test_management_api.py",
}
VERIFY = "--verify-token"
TOKEN_ASSIGN_PRIMARY = 0x0001
TOKEN_DUPLICATE = 0x0002
TOKEN_QUERY = 0x0008
TOKEN_ADJUST_DEFAULT = 0x0080
TOKEN_USER = 1
TOKEN_OWNER = 4
TOKEN_ELEVATION = 20
TOKEN_INTEGRITY_LEVEL = 25
DISABLE_MAX_PRIVILEGE = 0x1
LUA_TOKEN = 0x4
SE_GROUP_INTEGRITY = 0x20


class SidAndAttributes(ct.Structure):
    _fields_ = [("sid", wt.LPVOID), ("attributes", wt.DWORD)]


class StartupInfo(ct.Structure):
    _fields_ = (
        [("size", wt.DWORD)]
        + [(field, wt.LPWSTR) for field in ("reserved", "desktop", "title")]
        + [(field, wt.DWORD) for field in (
            "x", "y", "width", "height", "columns", "rows", "fill", "flags",
        )]
        + [("show", wt.WORD), ("reserved_size", wt.WORD), ("reserved_bytes", wt.LPVOID)]
        + [(field, wt.HANDLE) for field in ("stdin", "stdout", "stderr")]
    )


class ProcessInfo(ct.Structure):
    _fields_ = [
        ("process", wt.HANDLE), ("thread", wt.HANDLE),
        ("process_id", wt.DWORD), ("thread_id", wt.DWORD),
    ]


def windows_api():
    security = ct.WinDLL("advapi32", use_last_error=True)
    kernel = ct.WinDLL("kernel32", use_last_error=True)
    functions = [
        (security.OpenProcessToken, wt.BOOL, wt.HANDLE, wt.DWORD, ct.POINTER(wt.HANDLE)),
        (security.GetTokenInformation,
            wt.BOOL, wt.HANDLE, wt.DWORD, wt.LPVOID, wt.DWORD, ct.POINTER(wt.DWORD),
        ),
        (security.SetTokenInformation, wt.BOOL, wt.HANDLE, wt.DWORD, wt.LPVOID, wt.DWORD),
        (security.ConvertStringSidToSidW, wt.BOOL, wt.LPCWSTR, ct.POINTER(wt.LPVOID)),
        (security.GetLengthSid, wt.DWORD, wt.LPVOID),
        (security.CreateRestrictedToken,
            wt.BOOL, wt.HANDLE, wt.DWORD, wt.DWORD, ct.POINTER(SidAndAttributes),
            wt.DWORD, wt.LPVOID, wt.DWORD, wt.LPVOID, ct.POINTER(wt.HANDLE),
        ),
        (security.CreateProcessAsUserW,
            wt.BOOL, wt.HANDLE, wt.LPCWSTR, wt.LPWSTR, wt.LPVOID, wt.LPVOID,
            wt.BOOL, wt.DWORD, wt.LPVOID, wt.LPCWSTR,
            ct.POINTER(StartupInfo), ct.POINTER(ProcessInfo),
        ),
        (kernel.GetCurrentProcess, wt.HANDLE,),
        (kernel.LocalFree, wt.LPVOID, wt.LPVOID),
        (kernel.CloseHandle, wt.BOOL, wt.HANDLE),
        (kernel.WaitForSingleObject, wt.DWORD, wt.HANDLE, wt.DWORD),
        (kernel.GetExitCodeProcess, wt.BOOL, wt.HANDLE, ct.POINTER(wt.DWORD)),
        (kernel.TerminateProcess, wt.BOOL, wt.HANDLE, wt.UINT),
    ]
    for function, result, *arguments in functions:
        function.restype = result
        function.argtypes = arguments
    return security, kernel


def require(result):
    if not result:
        raise ct.WinError(ct.get_last_error())


def elevated(security, token):
    value = wt.DWORD()
    returned = wt.DWORD()
    require(security.GetTokenInformation(
        token, TOKEN_ELEVATION, ct.byref(value), ct.sizeof(value), ct.byref(returned),
    ))
    if returned.value != ct.sizeof(value):
        raise RuntimeError("Windows did not return valid token elevation data")
    return bool(value.value)


def restrict_token(security, kernel, source):
    administrators = wt.LPVOID()
    medium_integrity = wt.LPVOID()
    result = wt.HANDLE()
    try:
        require(security.ConvertStringSidToSidW("S-1-5-32-544", ct.byref(administrators)))
        require(security.ConvertStringSidToSidW("S-1-16-8192", ct.byref(medium_integrity)))
        disabled = SidAndAttributes(administrators, 0)
        require(security.CreateRestrictedToken(
            source, DISABLE_MAX_PRIVILEGE | LUA_TOKEN, 1, ct.byref(disabled),
            0, None, 0, None, ct.byref(result),
        ))
        label = SidAndAttributes(medium_integrity, SE_GROUP_INTEGRITY)
        require(security.SetTokenInformation(
            result, TOKEN_INTEGRITY_LEVEL, ct.byref(label),
            ct.sizeof(label) + security.GetLengthSid(medium_integrity),
        ))

        size = wt.DWORD()
        security.GetTokenInformation(source, TOKEN_USER, None, 0, ct.byref(size))
        require(size.value)
        user = ct.create_string_buffer(size.value)
        require(security.GetTokenInformation(
            source, TOKEN_USER, user, size.value, ct.byref(size),
        ))
        # TOKEN_USER begins with SID_AND_ATTRIBUTES. Keep its buffer alive
        # while making that user the default owner of fixture files.
        user_sid = wt.LPVOID(ct.cast(user, ct.POINTER(SidAndAttributes)).contents.sid)
        require(security.SetTokenInformation(
            result, TOKEN_OWNER, ct.byref(user_sid), ct.sizeof(user_sid),
        ))
        if elevated(security, result):
            raise RuntimeError("the Windows integration token is still elevated")
        return result
    except BaseException:
        if result:
            kernel.CloseHandle(result)
        raise
    finally:
        if medium_integrity:
            kernel.LocalFree(medium_integrity)
        if administrators:
            kernel.LocalFree(administrators)


def run_restricted(security, kernel, token, task):
    temp_root = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())).resolve()
    with tempfile.TemporaryDirectory(prefix="amaterasu-native-ci-", dir=temp_root) as directory:
        log = Path(directory) / "integration.log"
        command = ct.create_unicode_buffer(subprocess.list2cmdline([
            sys.executable, str(Path(__file__).resolve()), "--child", str(log), task,
        ]))
        startup = StartupInfo()
        startup.size = ct.sizeof(startup)
        child = ProcessInfo()
        # The child opens its own log. It inherits no privileged handles.
        require(security.CreateProcessAsUserW(
            token, sys.executable, command, None, None, False, 0, None,
            str(Path.cwd()), ct.byref(startup), ct.byref(child),
        ))
        kernel.CloseHandle(child.thread)
        try:
            waited = kernel.WaitForSingleObject(child.process, 20 * 60 * 1000)
            if waited == 258:
                require(kernel.TerminateProcess(child.process, 1))
                kernel.WaitForSingleObject(child.process, 5000)
                raise TimeoutError(f"Windows integration timed out running {task}")
            if waited != 0:
                raise ct.WinError(ct.get_last_error())
            exit_code = wt.DWORD()
            require(kernel.GetExitCodeProcess(child.process, ct.byref(exit_code)))
            return exit_code.value if exit_code.value <= 255 else 1
        finally:
            kernel.CloseHandle(child.process)
            if log.exists():
                print(log.read_text(encoding="utf-8", errors="replace"), end="", flush=True)


def run_fixture(task, output=None):
    if task == VERIFY:
        print("Windows integration token is unelevated", file=output, flush=True)
        return 0
    script = Path(__file__).with_name(task)
    return subprocess.run([sys.executable, str(script)], stdout=output, stderr=output).returncode


def main():
    args = sys.argv[1:]
    child = len(args) == 3 and args[0] == "--child"
    if (not child and len(args) != 1) or args[-1] not in INTEGRATION_SCRIPTS | {VERIFY}:
        raise SystemExit("usage: run_native_integration.py <integration script or --verify-token>")
    task = args[-1]
    if os.name != "nt":
        if child or task == VERIFY:
            raise SystemExit("Windows token verification requires Windows")
        return run_fixture(task)

    security, kernel = windows_api()
    source = wt.HANDLE()
    access = TOKEN_QUERY
    if not child:
        access |= TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT
    require(security.OpenProcessToken(kernel.GetCurrentProcess(), access, ct.byref(source)))
    try:
        is_elevated = elevated(security, source)
        if child:
            with Path(args[1]).open("w", encoding="utf-8") as output:
                if is_elevated:
                    print("Refusing to run fixtures in an elevated child", file=output)
                    return 1
                # Avoid the hosted runner's short-name alias and retain a
                # separate temporary root for this standard-user process.
                os.environ["TMPDIR"] = str(Path(args[1]).parent)
                return run_fixture(task, output)
        if not is_elevated:
            return run_fixture(task)
        token = restrict_token(security, kernel, source)
        try:
            return run_restricted(security, kernel, token, task)
        finally:
            kernel.CloseHandle(token)
    finally:
        kernel.CloseHandle(source)


if __name__ == "__main__":
    sys.exit(main())
