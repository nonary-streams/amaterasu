#!/usr/bin/env python3
"""Issue a one-time browser enrollment code while the Linux host is stopped."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

from setup_linux_management import TOKEN, private_directory, regular_file, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('host', type=Path, help='absolute path to the amaterasu host executable')
    args = parser.parse_args()
    try:
        root = args.root
        if not root.is_absolute() or root.resolve() != root or not args.host.is_absolute():
            raise ValueError('root and host must be absolute paths; root must not contain symlinks')
        private_directory(root)
        bootstrap_path = root / 'bootstrap.json'
        regular_file(bootstrap_path, True)
        settings = json.loads(bootstrap_path.read_text())['policy']['settings']['amaterasu.management']
        name = settings['tls_certificate_path']
        if not name.startswith(TOKEN) or '/' in name[len(TOKEN):]:
            raise ValueError('invalid management certificate path')
        cert = root / 'data/state/management-tls' / name[len(TOKEN):]
        regular_file(cert, True)
        fingerprint = run('openssl', 'x509', '-in', str(cert), '-noout', '-fingerprint', '-sha256').decode().strip().split('=', 1)[1]
        command = {'op': 'request', 'service': 'amaterasu.authentication/1', 'payload': {'action': 'generate_enrollment_code'}, 'timeout_ms': 2000, 'operation_id': 'local.enrollment'}
        # probe holds the host root lock and exits before the normal host starts.
        result = subprocess.run([str(args.host), 'probe', str(root), json.dumps(command)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        reply = json.loads(result.stdout)
        payload = reply.get('payload', {})
        if reply.get('kind') != 'result' or payload.get('ok') is not True or not isinstance(payload.get('code'), str):
            raise ValueError('enrollment issuance failed')
        print(f"URL: https://{settings['tls_bind']}\nCertificate SHA-256: {fingerprint}\nEnrollment code: {payload['code']}\nExpires at: {payload['expires_at']}")
    except (ValueError, OSError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f'enrollment failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
