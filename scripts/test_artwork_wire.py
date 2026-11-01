#!/usr/bin/env python3
"""Qualify concurrent artwork delivery using a signed, loopback-only host.

Pass a directory of Linux-session artifacts, plus the host and release binaries.
The fixture pairs its own test certificate and never touches the installed host.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from collections import Counter
import http.client
import json
import random
import socket
import struct
import tempfile
import time
import zlib
from pathlib import Path
from types import SimpleNamespace

from test_component_updates import run
from test_host_service import Console
from test_linux_session_profile import import_host_identity, isolated_environment, ready_status
from test_management_api import Api, available_moonlight_port_pair
from test_management_domains import token
from test_nvhttp import CLIENT_ID, pair, tls_context


def portrait(large: bool) -> bytes:
    def chunk(kind, value):
        payload = kind + value
        return struct.pack('>I', len(value)) + payload + struct.pack('>I', zlib.crc32(payload))
    row_bytes, bit_depth = (600, 8) if large else (75, 1)
    random_bytes = random.Random(123).randbytes(row_bytes * 900)
    rows = b''.join(b'\0' + random_bytes[i:i + row_bytes] for i in range(0, len(random_bytes), row_bytes))
    header = struct.pack('>IIBBBBB', 600, 900, bit_depth, 0, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--host', type=Path, required=True)
    parser.add_argument('--publisher', type=Path, required=True)
    parser.add_argument('--temporary-directory', type=Path, required=True)
    parser.add_argument('--expect-overload', action='store_true')
    parser.add_argument('--large', action='store_true')
    parser.add_argument('--workers', type=int, default=8)
    parser.add_argument('--skip-restart', action='store_true')
    args = parser.parse_args()
    base_port, management_port = available_moonlight_port_pair(), port()
    with tempfile.TemporaryDirectory(prefix='artwork-wire-', dir=args.temporary_directory) as temp:
        directory = Path(temp)
        root, package, steam = directory / 'host', directory / 'package', directory / 'steam'
        run(args.publisher, 'init-linux-session', root, '--lan', f'127.0.0.1:{base_port}', '--management-port', str(management_port))
        image = portrait(args.large)
        (steam / 'steamapps' / 'common' / 'Example').mkdir(parents=True)
        cache = steam / 'appcache' / 'librarycache'
        cache.mkdir(parents=True)
        executable = steam / 'steam.sh'
        executable.write_text('#!/bin/sh\nexit 0\n')
        executable.chmod(0o700)
        for app_id in range(1001, 1009):
            (steam / 'steamapps' / f'appmanifest_{app_id}.acf').write_text(
                f'"AppState" {{ "appid" "{app_id}" "name" "Fixture {app_id}" "installdir" "Example" "StateFlags" "4" }}')
            (cache / f'{app_id}_library_600x900.png').write_bytes(image)
        bootstrap_path = root / 'bootstrap.json'
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap['policy']['settings']['amaterasu.steam'] = {'roots': [str(steam)]}
        bootstrap['policy']['principals']['amaterasu.console']['call'].append('amaterasu.management/1')
        bootstrap_path.write_text(json.dumps(bootstrap))
        run(args.publisher, 'pack-linux-session', root, args.artifacts, package, '1')
        run(args.publisher, 'verify', root, package)
        run(args.publisher, 'stage', root, package)
        environment = isolated_environment(directory)
        console = Console(SimpleNamespace(host=args.host, root=root), environment=environment)
        try:
            import_host_identity(console)
            status = ready_status(console)
            api = Api(console)
            assert console.request('amaterasu.authentication/1', {'action': 'setup', 'username': 'fixture', 'password': 'local-only-artwork-fixture'})['ok']
            bearer = token(console, ['steam.read', 'steam.refresh', 'configuration.read', 'configuration.write', 'catalog.read'])
            status_code, settings, _ = api.post('/v1/steam/settings', token=bearer)
            assert status_code == 200 and settings['ok'], settings
            status_code, changed, _ = api.post('/v1/steam/settings_update', {'enabled': True, 'auto_sync': False, 'sync_all_installed': True, 'include_tools': True, 'exclude_games': [], 'expected_revision': settings['revision']}, token=bearer)
            assert status_code == 200 and changed['ok'], changed
            status_code, response, _ = api.post('/v1/steam/force_sync', token=bearer)
            assert status_code == 200 and response['ok'], response
            deadline = time.monotonic() + 60
            while True:
                _, state, _ = api.post('/v1/steam/status', token=bearer)
                _, catalog, _ = api.post('/v1/catalog/list', {'limit': 32}, token=bearer)
                entries = [entry for entry in catalog.get('entries', []) if entry.get('entry', {}).get('artwork', {}).get('provider') == 'steam']
                if len(entries) == 8 and state.get('complete') and not state.get('force_sync_pending'):
                    break
                assert not state.get('catalog_sync', {}).get('last_error'), state
                assert time.monotonic() < deadline, (state, catalog)
                time.sleep(.05)
            http_base = 'http://' + status['http_address']
            pair(console, http_base)
            context = tls_context()
            address = status['https_address'].rsplit(':', 1)
            app_ids = [str(int(entry['id'][4:], 16)) for entry in entries]
            def fetch(app_id):
                connection = http.client.HTTPSConnection(address[0], int(address[1]), context=context, timeout=90)
                try:
                    connection.request('GET', f'/appasset?appid={app_id}&uniqueid={CLIENT_ID}')
                    response = connection.getresponse()
                    body = response.read()
                    return response.status, response.getheader('Content-Type'), body
                finally:
                    connection.close()
            def fetch_all():
                start = time.monotonic()
                with ThreadPoolExecutor(max_workers=args.workers) as pool:
                    results = list(pool.map(fetch, app_ids))
                if args.expect_overload:
                    counts = Counter(result[0] for result in results)
                    assert counts[503] > 0, counts
                    print(f'Baseline artwork responses: {dict(counts)} in {time.monotonic() - start:.2f}s', flush=True)
                    return
                for result in results:
                    assert result == (200, 'image/png', image), (result[0], result[1], len(result[2]))
                print(f'Passed: 8 paired TLS artwork responses with {args.workers} workers, {len(image)} bytes each, in {time.monotonic() - start:.2f}s', flush=True)
            fetch_all()
            console.eof()
            if args.expect_overload or args.skip_restart:
                return
            console.close()
            console = Console(SimpleNamespace(host=args.host, root=root), environment=environment)
            api = Api(console)
            ready_status(console)
            deadline = time.monotonic() + 60
            while True:
                _, state, _ = api.post('/v1/steam/status', token=bearer)
                if state.get('complete') and state.get('game_count') == 8 and not state.get('scanning'):
                    break
                assert time.monotonic() < deadline, state
                time.sleep(.05)
            assert not state['catalog_sync']['enabled'], state
            fetch_all()
            print('Passed: startup discovery restored covers without enabling auto-sync', flush=True)
            console.eof()
        finally:
            console.close()


if __name__ == '__main__':
    main()
