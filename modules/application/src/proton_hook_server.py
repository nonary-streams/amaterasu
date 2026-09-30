#!/usr/bin/python3
"""Install and serve the active Steam Proton compatibility preference."""

import errno
import os
from pathlib import Path
import select
import signal
import socket
import stat
import struct
import sys
import tempfile
import time

MARKER = b"# BEGIN Amaterasu Proton compatibility v1"
HOOK = b'''\n# BEGIN Amaterasu Proton compatibility v1
try:
    import os as _am_os
    import socket as _am_socket
    import struct as _am_struct
    import sys as _am_sys
    _am_main = _am_sys.modules.get("__main__")
    _am_session = getattr(_am_main, "g_session", None)
    _am_launch_env = getattr(_am_session, "env", None)
    if not isinstance(_am_launch_env, dict):
        _am_launch_env = _am_os.environ
    _am_app_id = _am_launch_env.get("SteamAppId") or _am_launch_env.get("SteamGameId")
    if isinstance(_am_app_id, str) and _am_app_id.isdigit():
        _am_address = "\\0amaterasu.proton-compat.v1." + str(_am_os.getuid()) + "." + _am_app_id
        with _am_socket.socket(_am_socket.AF_UNIX, _am_socket.SOCK_STREAM) as _am_client:
            _am_client.settimeout(0.2)
            _am_client.connect(_am_address)
            _am_client.sendall(_am_app_id.encode("ascii"))
            _am_peer = _am_struct.unpack("3i", _am_client.getsockopt(_am_socket.SOL_SOCKET, _am_socket.SO_PEERCRED, 12))
            if _am_peer[1] == _am_os.getuid() and _am_client.recv(2) == b"1":
                _am_settings = globals().setdefault("user_settings", {})
                if isinstance(_am_settings, dict):
                    for _am_name in ("PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE", "PROTON_SONY_WINDOWS_DEVICE_NAMES"):
                        if _am_name not in _am_settings and _am_name not in _am_launch_env:
                            _am_settings[_am_name] = "1"
                            if isinstance(_am_launch_env, dict):
                                _am_launch_env[_am_name] = "1"
except Exception:
    pass  # An absent stream service must never prevent a game from starting.
# END Amaterasu Proton compatibility v1
'''


def socket_address(app_id):
    return "\0amaterasu.proton-compat.v1." + str(os.getuid()) + "." + app_id


def read_owned(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1:
            raise ValueError("settings must be a private user-owned regular file")
        data = source.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024:
            raise ValueError("settings file exceeds one MiB")
        return data, info


def replace_owned(path, data, previous):
    fd, temporary = tempfile.mkstemp(prefix=".amaterasu-proton-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            if previous and os.fstat(output.fileno()).st_gid != previous.st_gid:
                os.fchown(output.fileno(), -1, previous.st_gid)
            os.fchmod(output.fileno(), stat.S_IMODE(previous.st_mode) if previous else 0o600)
        if previous:
            current = path.lstat()
            if (current.st_dev, current.st_ino, current.st_mtime_ns, current.st_size) != (
                previous.st_dev, previous.st_ino, previous.st_mtime_ns, previous.st_size
            ):
                try:
                    current_data, _ = read_owned(path)
                except (FileNotFoundError, OSError, ValueError):
                    raise ValueError("settings changed during update")
                if current_data == data:
                    return
                raise ValueError("settings changed during update")
            os.replace(temporary, path)
        else:
            # Linking rather than replacing preserves a concurrently created file.
            try:
                os.link(temporary, path)
            except FileExistsError:
                try:
                    current_data, _ = read_owned(path)
                except (FileNotFoundError, OSError, ValueError):
                    raise ValueError("settings appeared during update")
                if current_data != data:
                    raise ValueError("settings appeared during update")
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def install(tool):
    if not (tool / "proton").is_file() or not (tool / "user_settings.sample.py").is_file():
        return False
    settings = tool / "user_settings.py"
    try:
        contents, info = read_owned(settings)
    except FileNotFoundError:
        contents, info = b"", None
    if MARKER in contents:
        if contents.count(MARKER) == 1 and contents.count(HOOK) == 1:
            return True
        raise ValueError("modified Amaterasu Proton hook")
    updated = contents + HOOK
    compile(updated, str(settings), "exec")
    replace_owned(settings, updated, info)
    return True


def tool_directories(roots):
    result = set()
    for root in roots:
        root = Path(root)
        candidates = [root]
        for parent in (root / "compatibilitytools.d", root / "steamapps" / "common"):
            try:
                with os.scandir(parent) as entries:
                    candidates.extend(Path(entry.path) for _, entry in zip(range(4096), entries))
            except OSError:
                pass
        for candidate in candidates:
            try:
                if (candidate / "proton").is_file() and (candidate / "user_settings.sample.py").is_file():
                    result.add(candidate.resolve())
            except OSError:
                pass
    return sorted(result)


def install_tools(roots):
    candidates = tool_directories(roots)
    installed = 0
    errors = []
    for candidate in candidates:
        try:
            installed += bool(install(candidate))
        except (OSError, ValueError, SyntaxError) as error:
            errors.append(str(error))
    return len(candidates), installed, errors


def serve(app_id, enabled, roots):
    if not app_id.isascii() or not app_id.isdigit() or not 1 <= len(app_id) <= 10:
        raise ValueError("invalid Steam app id")
    candidates, installed, errors = 0, 0, []
    if enabled:
        candidates, installed, errors = install_tools(roots)
        if candidates and not installed:
            raise RuntimeError("no discovered Proton installation accepted the hook")

    stopping = False

    def stop(*_):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    address = socket_address(app_id)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
        deadline = time.monotonic() + 5
        while True:
            try:
                server.bind(address)
                break
            except OSError as error:
                if error.errno != errno.EADDRINUSE or time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)
        server.listen(8)
        server.settimeout(0.25)
        print("READY " + str(installed if enabled else 0), flush=True)
        scanned_at = time.monotonic()
        while not stopping:
            if select.select([sys.stdin], [], [], 0)[0]:
                break
            if enabled and time.monotonic() - scanned_at >= 30:
                install_tools(roots)
                scanned_at = time.monotonic()
            try:
                client, _ = server.accept()
            except socket.timeout:
                continue
            with client:
                client.settimeout(0.2)
                try:
                    _, uid, _ = struct.unpack(
                        "3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
                    )
                    requested_app = client.recv(16)
                    if uid == os.getuid() and requested_app == app_id.encode("ascii"):
                        client.sendall(b"1" if enabled else b"0")
                except (OSError, ValueError):
                    pass


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "--install-only":
        candidates, installed, errors = install_tools(sys.argv[2:])
        print("{} {} {}".format(candidates, installed, len(errors)))
    elif len(sys.argv) == 3 and sys.argv[1] == "--test-racing-update":
        settings = Path(sys.argv[2])
        contents, previous = read_owned(settings)
        updated = contents + HOOK
        replace_owned(settings, updated, previous)
        replace_owned(settings, updated, previous)
        print("ok")
    else:
        serve(sys.argv[1], sys.argv[2] == "1", sys.argv[3:])
