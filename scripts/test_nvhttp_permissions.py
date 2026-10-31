#!/usr/bin/env python3
"""Signed mTLS permission regression using a synthetic launch and owned child.

No real capture, media decoding, physical display, or Moonlight client is tested.
Requires prebuilt signed-composition artifacts in CARGO_TARGET_DIR.
"""
import tempfile
from pathlib import Path
import secrets

from test_component_updates import ComponentHarness, run
from test_host_service import Console
from test_management_api import Api, PASSWORD, stop_console
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_public_launch import child_running, launch_query, other_paired_context, wait_until
from test_public_launch_workflow import configure, create_managed_application, wait_for_worker
from test_software_encoder_policy import checked, login


def update(api, bearer, action, **fields):
    clients = checked(api, '/v1/pairing/list_clients', bearer=bearer)
    result = checked(api, '/v1/pairing/' + action,
                     {'expected_revision': clients['revision'], **fields}, bearer)
    assert result['ok'], result


def expect_status(https, path, context, status, query=None):
    actual, xml = get(https, path, query, context)
    assert actual == status and xml.get('status_code') == str(status), (actual, xml.attrib)


def exercise(directory):
    harness = ComponentHarness(directory)
    configure(harness, synthetic=True)
    package = directory / 'signed-stream-probe'
    run(harness.publisher, 'pack-stream-probe', harness.root, harness.artifacts, package, '1')
    harness.stage(package)
    console = Console(harness)
    pid = None
    try:
        wait_for_worker(console)
        assert console.request('amaterasu.pairing/1', {
            'action': 'import_identity',
            'certificate_pem': (FIXTURES / 'host-cert.pem').read_text(),
            'private_key_pem': (FIXTURES / 'host-key.pem').read_text(),
        })['ok']
        state = wait_for(console, 'amaterasu.nvhttp/1', {'action': 'status'}, lambda v: v.get('tls_ready'))
        http, https = 'http://' + state['http_address'], 'https://' + state['https_address']
        assert console.request('amaterasu.authentication/1', {
            'action': 'setup', 'username': 'administrator', 'password': PASSWORD,
        })['ok']
        api = Api(console)
        bearer = login(api)
        certificate_id = pair(console, http)
        context = tls_context()
        other = other_paired_context(console, directory)
        marker = directory / 'owned.pid'
        create_managed_application(api, bearer, marker)
        listed = successful(get(https, '/applist', context=context))
        app = next(row for row in listed.findall('App') if row.findtext('AppTitle') == 'Controlled software policy process')
        app_id = app.findtext('ID')
        permissions = {'list_apps': False, 'view_streams': False, 'launch_apps': False, 'terminate_streams': False}
        update(api, bearer, 'update_client_permissions', certificate_id=certificate_id, permissions=permissions)
        # Disabled mode grants access even when explicit client fields are false.
        successful(get(https, '/applist', context=context))
        update(api, bearer, 'set_permission_mode', enabled=True)
        expect_status(https, '/applist', context, 403, {'uniqueid': 'other-launch-owner'})
        query = launch_query(secrets.token_bytes(16), app_id)
        query['uniqueid'] = 'other-launch-owner'
        expect_status(https, '/launch', context, 403, query)
        expect_status(https, '/resume', context, 403, query)
        successful(get(https, '/applist', context=other))
        assert not marker.exists()
        # Launch access implies list access even if the list bit is false.
        permissions['launch_apps'] = True
        update(api, bearer, 'update_client_permissions', certificate_id=certificate_id, permissions=permissions)
        successful(get(https, '/applist', context=context))
        successful(get(https, '/launch', query, context))
        wait_until(lambda: marker.exists() and marker.read_text().isdigit(), 'owned application did not start')
        pid = int(marker.read_text())
        assert child_running(pid)
        permissions['launch_apps'] = False
        update(api, bearer, 'update_client_permissions', certificate_id=certificate_id, permissions=permissions)
        wait_until(lambda: not child_running(pid), 'revoked launch left owned application alive', 15)
        paired = successful(get(https, '/serverinfo', context=context))
        assert paired.findtext('PairStatus') == '1', paired.attrib
        expect_status(https, '/launch', context, 403, query)
        successful(get(https, '/applist', context=other))
        console.eof()
    except Exception:
        print(''.join(console.errors))
        raise
    finally:
        stop_console(console)
        if pid is not None:
            assert not child_running(pid), 'controlled application survived host cleanup'


def main():
    with tempfile.TemporaryDirectory(prefix='amaterasu-nvhttp-permissions-') as temporary:
        exercise(Path(temporary))
    print('Passed signed paired TLS permission mode, certificate-bound denial, inherited list, known-ID launch/resume denial and active owned-child revocation. Synthetic composition; no media playback or physical capture verified.')


if __name__ == '__main__':
    main()
