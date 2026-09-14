#!/usr/bin/env python3
"""Independent struct.pack fixtures for the pinned primary client's layouts."""
import pathlib
import struct
import sys


def input_packet(magic, body):
    return struct.pack('>I', len(body) + 4) + struct.pack('<I', magic) + body


vectors = {
    'ping': (0x0200, struct.pack('<HI', 4, 0x12345678) + b'\xab\xcd'),
    'idr': (0x0302, b'\0\0'),
    'start': (0x0307, b'\0'),
    'references': (0x0301, struct.pack('<6I', 0x12345678, 0, 0x12345679, 0, 0, 0)),
    'ltr': (0x0350, struct.pack('<II', 0x12345678, 0)),
    'termination': (0x0109, struct.pack('>I', 0x800e9403)),
    'fec': (0x5502, struct.pack('>I7H3B', 0x12345678, 123, 120, 2, 100, 20, 99, 19, 20, 1, 2)),
    'keyboard': (0x0206, input_packet(3, struct.pack('<BHBH', 1, 0x8041, 5, 0))),
    'relative': (0x0206, input_packet(7, struct.pack('>hh', -300, 700))),
    'absolute': (0x0206, input_packet(5, struct.pack('>5H', 1000, 500, 0, 1919, 1079))),
    'button': (0x0206, input_packet(9, b'\x05')),
    'scroll': (0x0206, input_packet(10, struct.pack('>hhh', -120, -120, 0))),
    'horizontal': (0x0206, input_packet(0x55000001, struct.pack('>h', 120))),
    'text': (0x0206, input_packet(0x17, '😀'.encode())),
    'controller': (0x0206, input_packet(12, struct.pack('<5H2B4h3H', 0x1a, 3, 9, 0x14, 0x1234, 50, 250, -32768, 32767, -3, 4, 0x9c, 0x55aa, 0x55))),
    'haptics': (0x0206, input_packet(13, struct.pack('<H', 1))),
}
content = '# name kind-hex payload-hex; independent Python struct fixtures\n'
content += ''.join(f'{name} {kind:04x} {body.hex()}\n' for name, (kind, body) in vectors.items())
path = pathlib.Path(__file__).with_name('vectors.txt')
if '--check' in sys.argv:
    assert path.read_text() == content, 'fixtures differ'
    print(f'{len(vectors)} primary-shaped fixtures verified')
else:
    path.write_text(content)
