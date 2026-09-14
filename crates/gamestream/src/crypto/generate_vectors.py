"""Independent, reproducible public test vectors; requires Python cryptography.

Run with --check to compare the checked-in vectors instead of writing them.
Only fixed public fixture keys are used. No runtime credentials are read.
"""
from pathlib import Path
import sys
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives import padding

key = bytes(range(16))
rows = []
for counter in (0, 1):
    frame = 0x01020304
    plaintext = bytes(range(64))
    nonce = counter.to_bytes(8, "little") + b"\0\0\0V"
    sealed = AESGCM(key).encrypt(nonce, plaintext, None)
    packet = nonce + frame.to_bytes(4, "little") + sealed[-16:] + sealed[:-16]
    rows.append(f"video {counter} {frame} {key.hex()} {plaintext.hex()} {packet.hex()}")
for key_id, sequence, length in ((0x01020304, 0x1234, 31), (0xFFFFFFFE, 3, 32), (0, 65535, 1)):
    plaintext = bytes(range(length))
    iv = ((key_id + sequence) & 0xFFFFFFFF).to_bytes(4, "big") + bytes(12)
    padder = padding.PKCS7(128).padder()
    padded = padder.update(plaintext) + padder.finalize()
    encryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    ciphertext = encryptor.update(padded) + encryptor.finalize()
    rows.append(f"audio {key_id} {sequence} {key.hex()} {plaintext.hex()} {ciphertext.hex()}")
expected = "\n".join(rows) + "\n"
path = Path(__file__).with_name("vectors.txt")
if "--check" in sys.argv:
    assert path.read_text() == expected, "independent crypto vectors differ"
    print("Python cryptography verified all five media encryption vectors")
else:
    path.write_text(expected)

# Independent control and RTSP envelope vectors in both direction domains.
rows = []
for protocol, first in (("control", 0), ("rtsp", 1)):
    for direction in ("H", "C"):
        for sequence in (first, 0x01020304):
            domain = (direction + ("C" if protocol == "control" else "R")).encode()
            nonce = sequence.to_bytes(4, "little") + bytes(6) + domain
            if protocol == "control":
                payload = bytes(range(10))
                plaintext = (0x0305).to_bytes(2, "little") + len(payload).to_bytes(2, "little") + payload
            else:
                plaintext = b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n\r\n"
            sealed = AESGCM(key).encrypt(nonce, plaintext, None)
            if protocol == "control":
                prefix = b"\x01\x00" + (20 + len(plaintext)).to_bytes(2, "little") + sequence.to_bytes(4, "little")
            else:
                prefix = (0x80000000 | len(plaintext)).to_bytes(4, "big") + sequence.to_bytes(4, "big")
            packet = prefix + sealed[-16:] + sealed[:-16]
            rows.append(f"{protocol} {direction} {sequence} {key.hex()} {plaintext.hex()} {packet.hex()}")
expected = "\n".join(rows) + "\n"
path = Path(__file__).with_name("envelope_vectors.txt")
if "--check" in sys.argv:
    assert path.read_text() == expected, "independent envelope vectors differ"
    print("Python cryptography verified all eight control/RTSP vectors")
else:
    path.write_text(expected)
