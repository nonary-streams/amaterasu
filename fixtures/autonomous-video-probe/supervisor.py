"""Best-effort bounded process-session cleanup for synthetic tests on Linux.

A descendant can detach with setsid. This is not a cgroup or containment boundary.
"""
from __future__ import annotations
import os
from pathlib import Path
import signal
import subprocess
import threading
import time


def live_session_members(session_id):
    """Read the process session from procfs; zombies are already dead."""
    members = []
    for path in Path('/proc').iterdir():
        if not path.name.isdecimal():
            continue
        try:
            record = (path / 'stat').read_text()
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise RuntimeError(f'cannot verify process session membership: {exc}') from exc
        _, separator, remainder = record.rpartition(')')
        fields = remainder.split()
        if not separator or len(fields) < 4:
            raise RuntimeError('malformed process status while verifying cleanup')
        if int(fields[3]) == session_id and fields[0] != 'Z':
            members.append(int(path.name))
    return members


class SessionProcess:
    def __init__(self, command):
        if not Path('/proc/self/stat').is_file() or not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
            raise RuntimeError('Linux procfs and pidfd signalling are required')
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1,
                                        start_new_session=True)
        self.session = self.process.pid
        self.errors = []
        self.output = []
        self.readers = [threading.Thread(target=self.errors.extend, args=(self.process.stderr,), daemon=True),
                        threading.Thread(target=self.output.extend, args=(self.process.stdout,), daemon=True)]
        for reader in self.readers:
            reader.start()

    def eof(self):
        self.process.stdin.close()
        if self.process.wait(timeout=8) != 0:
            raise RuntimeError('host exited unsuccessfully: ' + ''.join(self.errors))

    def close(self):
        # Verify every process still in the original session, including workers
        # that changed process group. A pidfd prevents signalling a reused PID.
        for sig, timeout in ((signal.SIGTERM, 2), (signal.SIGKILL, 2)):
            for pid in live_session_members(self.session):
                try:
                    fd = os.pidfd_open(pid)
                except ProcessLookupError:
                    continue
                try:
                    # Recheck membership after opening the identity-bearing handle.
                    if pid in live_session_members(self.session):
                        signal.pidfd_send_signal(fd, sig)
                except ProcessLookupError:
                    pass
                finally:
                    os.close(fd)
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                self.process.poll()
                if not live_session_members(self.session):
                    self.process.wait(timeout=1)
                    if not self.process.stdin.closed:
                        self.process.stdin.close()
                    for reader in self.readers:
                        reader.join(timeout=1)
                    if any(reader.is_alive() for reader in self.readers):
                        raise RuntimeError('probe pipes remain open; an escaped descendant may exist')
                    self.process.stdout.close()
                    self.process.stderr.close()
                    return
                time.sleep(0.02)
        raise RuntimeError('probe session did not drain; external intervention required')
