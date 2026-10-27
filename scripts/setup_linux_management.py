#!/usr/bin/env python3
"""Provision the Linux session's separate browser HTTPS identity."""
import argparse
import hashlib
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

TOKEN = '__AMATERASU_WORKER_STATE_ROOT__/management-tls/'


def private_directory(path):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError(f'{path} must be an owned private directory (mode 0700)')


def regular_file(path, private=False):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError(f'{path} must be an owned regular file')
    if private and info.st_mode & 0o077:
        raise ValueError(f'{path} must be private (mode 0600)')


def run(*args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def validate_address(value, addresses):
    address = ipaddress.ip_address(value)
    # RFC 1918, shared VPN address space, and IPv6 ULA only.
    permitted = any(address in network for network in (ipaddress.ip_network('10.0.0.0/8'), ipaddress.ip_network('172.16.0.0/12'), ipaddress.ip_network('192.168.0.0/16'), ipaddress.ip_network('100.64.0.0/10'), ipaddress.ip_network('fc00::/7')))
    if not permitted or str(address) not in addresses:
        raise ValueError('address must be an assigned RFC 1918, 100.64.0.0/10, or IPv6 ULA address')
    return address


def interface_addresses():
    entries = json.loads(run('ip', '-j', 'address', 'show'))
    return {str(ipaddress.ip_address(item['local'])) for entry in entries for item in entry.get('addr_info', []) if 'local' in item}


def verify_identity(cert, key, address):
    # Trust this exact leaf for verification: validate its time and TLS server purpose
    # without claiming that its issuer is trusted by any browser.
    run('openssl', 'verify', '-partial_chain', '-purpose', 'sslserver', '-CAfile', str(cert), str(cert))
    match = run('openssl', 'x509', '-in', str(cert), '-noout', '-checkip', str(address))
    if b'does match' not in match:
        raise ValueError('certificate does not match the selected IP address')
    cert_key = run('openssl', 'x509', '-in', str(cert), '-pubkey', '-noout')
    key_public = run('openssl', 'pkey', '-in', str(key), '-pubout')
    if cert_key != key_public:
        raise ValueError('certificate and private key do not match')


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def store_file(directory, role, data):
    name = f'management-{role}-{hashlib.sha256(data).hexdigest()}.pem'
    path = directory / name
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except FileExistsError:
        regular_file(path, True)
        if path.read_bytes() != data:
            raise ValueError(f'existing identity file differs: {path}')
    else:
        with os.fdopen(fd, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        sync_directory(directory)
    return name


def setup(root, address, port, certificate=None, key=None):
    if not root.is_absolute() or root.resolve() != root:
        raise ValueError('root must be an absolute, normalized path without symlinks')
    private_directory(root)
    for directory in (root / 'data', root / 'data/state'):
        directory.mkdir(mode=0o700, exist_ok=True)
        sync_directory(directory.parent)
        info = directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise ValueError(f'{directory} must be an owned directory without group or other write access')
    private_directory(root / 'data/state')
    bootstrap_path = root / 'bootstrap.json'
    regular_file(bootstrap_path, True)
    bootstrap = json.loads(bootstrap_path.read_text())
    if bootstrap.get('development') is not True:
        raise ValueError('only a development Linux session root is supported')
    management = bootstrap['policy']['settings']['amaterasu.management']
    if not isinstance(management, dict):
        raise ValueError('management settings must be an object')
    if (certificate is None) != (key is None):
        raise ValueError('certificate and key must be supplied together')
    directory = root / 'data/state/management-tls'
    directory.mkdir(mode=0o700, exist_ok=True)
    sync_directory(directory.parent)
    private_directory(directory)
    with tempfile.TemporaryDirectory(prefix='.setup-', dir=directory) as temporary:
        temp = Path(temporary)
        cert_source, key_source = temp / 'cert.pem', temp / 'key.pem'
        if certificate is not None:
            for source in (certificate, key):
                regular_file(source, source == key)
            cert_source.write_bytes(certificate.read_bytes())
            key_source.write_bytes(key.read_bytes())
        else:
            existing = [management.get('tls_certificate_path'), management.get('tls_private_key_path')]
            if any(existing):
                if not all(isinstance(item, str) and item.startswith(TOKEN) and '/' not in item[len(TOKEN):] for item in existing):
                    raise ValueError('existing TLS settings are incomplete or invalid')
                for destination, value in zip((cert_source, key_source), existing):
                    source = directory / value[len(TOKEN):]
                    regular_file(source, True)
                    destination.write_bytes(source.read_bytes())
            else:
                run('openssl', 'req', '-x509', '-newkey', 'rsa:3072', '-nodes', '-sha256', '-days', '825', '-subj', '/CN=Amaterasu management', '-addext', f'subjectAltName=IP:{address}', '-addext', 'extendedKeyUsage=serverAuth', '-addext', 'basicConstraints=critical,CA:FALSE', '-addext', 'keyUsage=critical,digitalSignature,keyEncipherment', '-keyout', str(key_source), '-out', str(cert_source))
        verify_identity(cert_source, key_source, address)
        cert_name = store_file(directory, 'cert', cert_source.read_bytes())
        key_name = store_file(directory, 'key', key_source.read_bytes())
        fingerprint = run('openssl', 'x509', '-in', str(cert_source), '-noout', '-fingerprint', '-sha256').decode().strip().split('=', 1)[1]
    host = f'[{address}]' if address.version == 6 else str(address)
    url = f'https://{host}:{port}'
    management.update({'enabled': True, 'bind': management.get('bind', '127.0.0.1:47990'), 'tls_bind': f'{host}:{port}', 'tls_certificate_path': TOKEN + cert_name, 'tls_private_key_path': TOKEN + key_name})
    origins = management.setdefault('allowed_origins', [])
    if not isinstance(origins, list):
        raise ValueError('allowed_origins must be an array')
    if url not in origins:
        origins.append(url)
    fd, tmp = tempfile.mkstemp(prefix='.bootstrap-', dir=root)
    try:
        with os.fdopen(fd, 'w') as output:
            os.fchmod(output.fileno(), stat.S_IMODE(bootstrap_path.stat().st_mode))
            json.dump(bootstrap, output, indent=2)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        os.replace(tmp, bootstrap_path)
        sync_directory(root)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    return url, fingerprint, directory / cert_name


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--address', required=True, help='assigned RFC 1918, 100.64.0.0/10, or IPv6 ULA address')
    parser.add_argument('--port', type=int, default=48081)
    parser.add_argument('--certificate', type=Path, help='trusted browser certificate PEM')
    parser.add_argument('--private-key', type=Path, help='matching private key PEM, owned and private')
    args = parser.parse_args()
    try:
        if not 1024 <= args.port <= 65535:
            raise ValueError('port must be between 1024 and 65535')
        address = validate_address(args.address, interface_addresses())
        private_directory(args.root)
        lock_path = args.root / 'host.lock'
        lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(lock_fd, 'r+') as lock:
            regular_file(lock_path)
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            url, fingerprint, cert = setup(args.root, address, args.port, args.certificate, args.private_key)
        print(f'URL: {url}\nCertificate SHA-256: {fingerprint}\nCertificate: {cert}\nSettings take effect on the next host start.')
        auth = json.loads((args.root / 'bootstrap.json').read_text())['policy']['settings'].get('amaterasu.authentication', {})
        if not isinstance(auth, dict):
            auth = {}
        def contains(field, value):
            values = auth.get(field)
            return isinstance(values, list) and value in values
        if (not contains('allowed_scopes', 'account.self')
                or contains('administrator_principals', 'amaterasu.management')
                or not contains('legacy_verifier_principals', 'amaterasu.management')):
            print('Warning: this root has an older authentication policy; upgrade it using the current Linux profile before using accounts. This command does not change authentication policy.', file=sys.stderr)
    except (ValueError, OSError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f'setup failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
