#!/usr/bin/env python3
import ipaddress
import fcntl
import contextlib
import io
from unittest.mock import patch
import json
import os
from pathlib import Path
import tempfile
import subprocess
import ssl
import socket
import threading
import unittest

import setup_linux_management as setup


class NetworkSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / 'data/state').mkdir(parents=True, mode=0o700)
        (self.root / 'data').chmod(0o700)
        self.bootstrap = self.root / 'bootstrap.json'
        self.bootstrap.write_text(json.dumps({'development': True, 'policy': {'settings': {'amaterasu.management': {'bind': '127.0.0.1:47990', 'web': {'directory': 'preserve'}}}}}))
        self.bootstrap.chmod(0o600)

    def tearDown(self):
        self.temp.cleanup()

    def test_private_assigned_addresses_only(self):
        self.assertEqual(str(setup.validate_address('192.168.2.4', {'192.168.2.4'})), '192.168.2.4')
        self.assertEqual(str(setup.validate_address('100.64.0.1', {'100.64.0.1'})), '100.64.0.1')
        for address in ('100.128.0.1', '127.0.0.1', '8.8.8.8', '0.0.0.0', '169.254.1.2', '192.168.2.5'):
            with self.assertRaises(ValueError):
                setup.validate_address(address, {'192.168.2.4', address} if address != '192.168.2.5' else set())

    def test_provision_reuse_and_mismatched_ip_rejected(self):
        ip = ipaddress.ip_address('192.168.2.4')
        url, fingerprint, cert = setup.setup(self.root, ip, 48081)
        self.assertEqual(url, 'https://192.168.2.4:48081')
        self.assertTrue(fingerprint)
        settings = json.loads(self.bootstrap.read_text())['policy']['settings']['amaterasu.management']
        self.assertEqual(settings['bind'], '127.0.0.1:47990')
        self.assertEqual(settings['web']['directory'], 'preserve')
        self.assertEqual(settings['tls_bind'], '192.168.2.4:48081')
        self.assertEqual(setup.setup(self.root, ip, 48081)[2], cert)
        before = self.bootstrap.read_bytes()
        with self.assertRaises((ValueError, subprocess.CalledProcessError)):
            setup.setup(self.root, ipaddress.ip_address('192.168.2.5'), 48081)
        self.assertEqual(self.bootstrap.read_bytes(), before)
        self.assertEqual(os.stat(cert).st_mode & 0o777, 0o600)

    def test_cli_refuses_busy_host_root(self):
        lock = self.root / 'host.lock'
        with lock.open('w') as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            before = self.bootstrap.read_bytes()
            with patch('sys.argv', ['setup', str(self.root), '--address', '192.168.2.4']), \
                 patch.object(setup, 'interface_addresses', return_value={'192.168.2.4'}), \
                 contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(setup.main(), 1)
            self.assertEqual(self.bootstrap.read_bytes(), before)

    def test_generated_certificate_works_with_hostname_verified_tls(self):
        _, _, cert = setup.setup(self.root, ipaddress.ip_address('127.0.0.1'), 48081)
        settings = json.loads(self.bootstrap.read_text())['policy']['settings']['amaterasu.management']
        key = self.root / 'data/state/management-tls' / settings['tls_private_key_path'].removeprefix(setup.TOKEN)
        server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_context.load_cert_chain(cert, key)
        listener = socket.socket()
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        errors = []
        def serve():
            try:
                connection, _ = listener.accept()
                with server_context.wrap_socket(connection, server_side=True) as tls:
                    tls.sendall(b'ok')
            except Exception as error:
                errors.append(error)
            finally:
                listener.close()
        thread = threading.Thread(target=serve)
        thread.start()
        try:
            client_context = ssl.create_default_context(cafile=str(cert))
            with socket.create_connection(listener.getsockname(), timeout=5) as connection:
                with client_context.wrap_socket(connection, server_hostname='127.0.0.1') as tls:
                    self.assertEqual(tls.recv(2), b'ok')
        finally:
            thread.join(timeout=5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])

    def test_rejects_client_only_certificate(self):
        cert, key = self.root / 'client.pem', self.root / 'client-key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                        '-subj', '/CN=test', '-addext', 'subjectAltName=IP:192.168.2.4',
                        '-addext', 'extendedKeyUsage=clientAuth', '-keyout', str(key),
                        '-out', str(cert)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        key.chmod(0o600)
        before = self.bootstrap.read_bytes()
        with self.assertRaises(subprocess.CalledProcessError):
            setup.setup(self.root, ipaddress.ip_address('192.168.2.4'), 48081, cert, key)
        self.assertEqual(self.bootstrap.read_bytes(), before)

    def test_symlink_root_rejected(self):
        link = self.root.parent / (self.root.name + '-link')
        link.symlink_to(self.root)
        try:
            with self.assertRaises(ValueError):
                setup.setup(link, ipaddress.ip_address('192.168.2.4'), 48081)
        finally:
            link.unlink()


if __name__ == '__main__':
    unittest.main()
