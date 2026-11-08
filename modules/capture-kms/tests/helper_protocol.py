#!/usr/bin/env python3
"""Exercise the isolated helper protocol against a deterministic fake adapter."""
import os
import array
import ctypes
import fcntl
from pathlib import Path
import select
import signal
import socket
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
FAKE = r'''
#include "adapter.h"
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
struct am_kms_session { int block; };
struct am_kms_session *am_kms_open(const char *id, uint32_t *error) {
    if (strcmp(id, "kms:test") != 0 && strcmp(id, "kms:block") != 0) { *error = 7; return NULL; }
    struct am_kms_session *session = calloc(1, sizeof(*session));
    if (session) session->block = strcmp(id, "kms:block") == 0;
    return session;
}
int am_kms_session_geometry(struct am_kms_session *session, struct am_kms_output *out) {
    (void)session; memset(out, 0, sizeof(*out)); out->width = 2; out->height = 2;
    out->attached = 1; out->framebuffer_export_supported = 1;
    memcpy(out->id, "kms:test", 9); return 0;
}
int am_kms_read(struct am_kms_session *s, uint8_t *dst, size_t len, uint32_t *w, uint32_t *h) {
    if (s->block) { (void)write(3, "B", 1); for (;;) pause(); }
    if (len != 16) return 5;
    memset(dst, 42, len); *w = 2; *h = 2; return 0;
}
void am_kms_close(struct am_kms_session *s) { free(s); }
int am_kms_probe(struct am_kms_info *out) { memset(out, 0, sizeof(*out)); return 0; }
int am_kms_enumerate(struct am_kms_output *out, size_t cap, size_t *count) {
    (void)out; (void)cap; *count = 0; return 0;
}
'''


def exact(sock, size):
    data = b""
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise EOFError("helper closed the channel")
        data += part
    return data


def request(op, payload=b""):
    return b"AMK1" + struct.pack("<I", op) + payload.ljust(192, b"\0")


class HelperProtocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        stub = Path(cls.temp.name) / "adapter.c"
        cls.helper = str(Path(cls.temp.name) / "helper")
        stub.write_text(FAKE)
        subprocess.run(["cc", "-std=c11", "-pthread", "-Wall", "-Wextra", "-Werror", "-I", str(ROOT / "src/platform"),
                        str(ROOT / "src/platform/helper.c"), str(stub), "-o", cls.helper], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def start_helper(self, invalid_pidfd=False):
        parent, child = socket.socketpair()
        host = os.open("/dev/null", os.O_RDONLY) if invalid_pidfd else os.pidfd_open(os.getpid())
        child_fd = fcntl.fcntl(child.fileno(), fcntl.F_DUPFD_CLOEXEC, 10)
        host_fd = fcntl.fcntl(host, fcntl.F_DUPFD_CLOEXEC, 10)
        os.close(host)
        try:
            pid = os.posix_spawn(self.helper, [self.helper], {}, file_actions=[
                (os.POSIX_SPAWN_DUP2, child_fd, 3), (os.POSIX_SPAWN_DUP2, host_fd, 4)])
        finally:
            os.close(child_fd)
            os.close(host_fd)
        child.close()
        parent.settimeout(3)
        return parent, pid

    def test_open_read_close(self):
        sock, pid = self.start_helper()
        with sock:
            self.assertEqual(exact(sock, 8), b"AMKREADY")
            sock.sendall(request(1, b"kms:test"))
            self.assertEqual(struct.unpack("<4sIIIII", exact(sock, 24)), (b"AMR1", 1, 0, 416, 0, 0))
            geometry = exact(sock, 416)
            self.assertEqual(struct.unpack_from("<II", geometry, 8), (2, 2))
            sock.sendall(request(2))
            self.assertEqual(struct.unpack("<4sIIIII", exact(sock, 24)), (b"AMR1", 2, 0, 16, 2, 2))
            self.assertEqual(exact(sock, 16), bytes([42]) * 16)
            sock.sendall(request(3))
            self.assertEqual(struct.unpack("<4sIIIII", exact(sock, 24)), (b"AMR1", 3, 0, 0, 0, 0))
        self.assertEqual(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]), 0)

    def test_rejects_nonzero_padding_and_closes(self):
        sock, pid = self.start_helper()
        with sock:
            self.assertEqual(exact(sock, 8), b"AMKREADY")
            sock.sendall(request(1, b"kms:test\0unexpected"))
            self.assertEqual(sock.recv(1), b"")
        self.assertEqual(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]), 0)

    def test_invalid_host_descriptor_fails_closed(self):
        sock, pid = self.start_helper(invalid_pidfd=True)
        with sock:
            self.assertEqual(sock.recv(1), b"")
        self.assertNotEqual(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]), 0)

    def test_host_death_kills_helper_blocked_in_capture(self):
        libc = ctypes.CDLL(None, use_errno=True)
        prior = ctypes.c_int()
        self.assertEqual(libc.prctl(37, ctypes.byref(prior), 0, 0, 0), 0)  # PR_GET_CHILD_SUBREAPER
        self.assertEqual(libc.prctl(36, 1, 0, 0, 0), 0)  # PR_SET_CHILD_SUBREAPER
        parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        supervisor = os.fork()
        if supervisor == 0:
            parent.close()
            try:
                connection, helper = self.start_helper()
                child.sendmsg([struct.pack("<I", helper)], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [connection.fileno()]))])
                connection.close()
                child.recv(1)
            finally:
                os._exit(0)
        child.close()
        helper_pidfd = None
        try:
            data, ancillary, _, _ = parent.recvmsg(4, socket.CMSG_SPACE(array.array("i").itemsize))
            helper = struct.unpack("<I", data)[0]
            descriptors = array.array("i")
            descriptors.frombytes(ancillary[0][2][:descriptors.itemsize])
            sock = socket.socket(fileno=descriptors[0])
            helper_pidfd = os.pidfd_open(helper)
            with sock:
                sock.settimeout(3)
                self.assertEqual(exact(sock, 8), b"AMKREADY")
                sock.sendall(request(1, b"kms:block"))
                self.assertEqual(struct.unpack("<4sIIIII", exact(sock, 24)), (b"AMR1", 1, 0, 416, 0, 0))
                exact(sock, 416)
                sock.sendall(request(2))
                self.assertEqual(exact(sock, 1), b"B")
                os.kill(supervisor, signal.SIGKILL)
                poller = select.poll()
                poller.register(helper_pidfd, select.POLLIN)
                self.assertTrue(poller.poll(3000), "helper survived host death")
                self.assertEqual(sock.recv(1), b"")
                result = os.waitid(os.P_PIDFD, helper_pidfd, os.WEXITED)
                self.assertEqual(result.si_code, os.CLD_KILLED)
                self.assertEqual(result.si_status, signal.SIGKILL)
        finally:
            parent.close()
            try:
                os.kill(supervisor, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(supervisor, 0)
            if helper_pidfd is not None:
                try:
                    signal.pidfd_send_signal(helper_pidfd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    os.waitid(os.P_PIDFD, helper_pidfd, os.WEXITED)
                except ChildProcessError:
                    pass
                os.close(helper_pidfd)
            self.assertEqual(libc.prctl(36, prior.value, 0, 0, 0), 0)


if __name__ == "__main__":
    unittest.main()
