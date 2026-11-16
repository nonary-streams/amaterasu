"""Qualify encrypted PyroWave ANNOUNCE with an isolated signed host.

Opens only loopback listeners and never issues PLAY or opens capture/audio/input.
GPU source preflight and the production media/framing adapters remain real.
Hardware capture and Windows playback are separate qualification gates.
"""
from pathlib import Path
import argparse
from types import SimpleNamespace
import hashlib, json, shutil, sys, time
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
sys.path.insert(0, str(Path.cwd() / 'scripts'))
from test_host_service import Console
from test_rtsp import RtspClient, request
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('root', type=Path)
parser.add_argument('candidate', type=Path)
parser.add_argument('host', type=Path)
args = parser.parse_args()
r = args.root.resolve()
c = args.candidate.resolve()
b = json.loads((c / 'candidate-bootstrap.json').read_text())
source = c / 'bundle/native/amaterasu-linux-session-rtx-hdr/0.1.0'
m = json.loads((source / 'manifest.json').read_text())
ids = {'amaterasu.runtime', 'amaterasu.network', 'amaterasu.state', 'amaterasu.session', 'amaterasu.rtsp', 'amaterasu.stream-session', 'amaterasu.encode', 'amaterasu.linux-session-video', 'amaterasu.media-transport', 'amaterasu.control-transport', 'amaterasu.capture-vkms', 'amaterasu.audio-encode'}
m['components'] = [x for x in m['components'] if x['id'] in ids]
for x in m['components']:
    x['requires'] = {k: v for k, v in x['requires'].items() if k in ids}
b['policy']['principals'] = {k: v for k, v in b['policy']['principals'].items() if k in ids}
services = sorted({v for g in b['policy']['principals'].values() for k in ['call', 'provide'] for v in g.get(k, [])})
b['policy']['principals']['amaterasu.console'] = {'call': services, 'native_use': [], 'native_provide': [], 'provide': [], 'publish': [], 'subscribe': [], 'hook': []}
b['policy']['settings'] = {k: v for k, v in b['policy']['settings'].items() if k in ids}
s = b['policy']['settings']
s['amaterasu.rtsp'] = {'enabled': True, 'extension_enabled': True, 'extension_bit_depth': 10, 'bind': '127.0.0.1:0', 'allow_remote': False, 'allowed_principals': ['amaterasu.console'], 'launch_owner': 'amaterasu.console'}
s['amaterasu.stream-session']['enable_input'] = False
s['amaterasu.stream-session']['lifecycle_admitters'] = ['amaterasu.rtsp']
s['amaterasu.stream-session'].pop('source_preparation', None)
s['amaterasu.capture-vkms'] = {'enabled': False}
s['amaterasu.media-transport']['allow_remote'] = False
s['amaterasu.control-transport']['allow_remote'] = False
s['amaterasu.linux-session-video']['capture_providers'] = ['vkms']
b['development'] = True
b['policy']['processing_compositions'] = {}
b['policy']['api_methods'] = []
b['policy']['hooks'] = {}
b['policy']['legacy_native_contracts'] = {}
b['policy'].pop('service_layers', None)
root = c / f'encrypted-loopback-fixture-{time.monotonic_ns()}'
root.mkdir(mode=448, exist_ok=True)
(root / 'data/state').mkdir(parents=True, exist_ok=True)
raw = json.dumps(m, separators=(',', ':')).encode()
d = hashlib.sha256(raw).hexdigest()
pkg = root / 'sets' / d
pkg.mkdir(parents=True, exist_ok=True)
for x in m['components']:
    shutil.copy2(source / x['file'], pkg / x['file'])
key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(json.loads((r / 'development-key.json').read_text())))
fp = json.loads((source / 'manifest.sig').read_text())['key_id']
(pkg / 'manifest.json').write_bytes(raw)
(pkg / 'manifest.sig').write_text(json.dumps({'key_id': fp, 'signature': key.sign(raw).hex()}))
(root / 'bootstrap.json').write_text(json.dumps(b))
(root / 'selection.json').write_text(json.dumps({'active': d, 'previous': None, 'pending': None, 'trial': None, 'highest_sequence': m['sequence']}))
host = args.host.resolve()
console = Console(SimpleNamespace(root=root, host=host))
try:
    status = console.request('amaterasu.rtsp/1', {'action': 'status'})
    print('status', status)
    key = bytes.fromhex('00112233445566778899aabbccddeeff')
    opened = request(console, {'action': 'open', 'operation_id': 'actual-encrypted-pyrowave-sdp', 'peer_ip': '127.0.0.1', 'video_port': 51001, 'audio_port': 51002, 'key_hex': key.hex(), 'key_id': 17, 'connect_data': 4321, 'sw_preset': 'superfast', 'sw_tune': 'zerolatency', 'capture': {'capture_provider': 'vkms', 'output': 'not-opened-by-negotiation', 'encoder': {'clsid': 'pyrowave', 'friendly_name': 'PyroWave', 'hardware_url': None}, 'audio_endpoint': 'default'}, 'profile': {'width': 2560, 'height': 1440, 'fps': 120, 'bitrate_kbps': 100000, 'max_reference_frames': 1, 'packet_size': 1360, 'packet_ms': 5, 'fec_percentage': 20, 'min_parity_shards': 2}})
    print('opened', opened)
    assert opened.get('ok') and opened.get('state') == 'ready', opened
    client = RtspClient(status['local'], opened['rtsp_url'], key)
    client.setup()
    client.transact('ANNOUNCE', 'streamid=control/13/0', {'Content-Type': 'application/sdp'}, Path('crates/gamestream/src/sdp/fixtures/moonlight-pyrowave10.sdp').read_bytes())
    print('ACTUAL_ENCRYPTED_ANNOUNCE_ACCEPTED')
    closed = request(console, {'action': 'close', 'ticket': opened['ticket']})
    assert closed.get('state') == 'closed' and closed.get('cleanup_error') is None, closed
    print('CLEANUP_CONFIRMED')
    console.eof()
finally:
    (c / 'encrypted-loopback.log').write_text(''.join(console.errors))
    print('errors', ''.join(console.errors)[-4000:])
    console.close()
