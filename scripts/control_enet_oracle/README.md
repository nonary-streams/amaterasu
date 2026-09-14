# Primary ENet client oracle

This development-only client links the original Moonlight ENet C implementation
at commit `aca87840b57f045a1f7f9299e4b1b9b8e2a5e2f1`. The source is supplied by
the developer and is neither vendored nor shipped with Amaterasu. The wrapper
uses public ENet APIs; it contains no cryptography or copied protocol machinery.

```sh
PATH=/private/tmp/amaterasu-cross/build-tools/bin:$PATH \
  scripts/control_enet_oracle/build.sh \
  /path/to/moonlight-common-c/enet
/private/tmp/amaterasu-enet-client-oracle 47999 12345 /tmp/control-ciphertext.bin
```

The build requires Git, CMake, a native C compiler, and the exact clean tracked
checkout. An optional second build argument changes the binary destination.
`CMAKE` and `CC` select tool executables. Upstream CMake performs platform probes
and builds its static library in a temporary directory that is removed afterward.
The wrapper compiles with `-Wall -Wextra -Werror`. No Cargo build is involved.

The client connects only to IPv4 loopback with 48 channels and the supplied
unsigned 32-bit decimal connect data. It reads a regular file containing
1–65,539 encrypted bytes, sends one reliable packet on channel 2, and expects
one reply on channel 2. Successful output is:

```text
CONNECTED
RECEIVED <lowercase hexadecimal ciphertext>
```

Both lines are delivered immediately through bounded writes. Connection,
exchange, and stdout delivery share a five-second monotonic deadline. Invalid
arguments/files, oversized replies, unexpected channels, disconnects, and
timeouts produce a nonzero exit code. Receive allocation and queued receive data
are bounded through ENet host limits. A stalled stdout consumer cannot retain
the client indefinitely. Diagnostic stderr should be drained by the harness.

The optional final argument `--interactive` keeps the same connection available
for post-activation input tests. After the initial file packet, stdin accepts at
most 64 additional packets, each framed by a four-byte big-endian length and
1–65,539 ciphertext bytes. Partial pipe reads are retained. `SENT` means the
packet was queued and flushed through ENet, not acknowledged by the application.
Replies remain `RECEIVED` lines. This mode has a fixed 30-second deadline and
ends on clean stdin EOF; a truncated frame fails. The harness must verify input
effects through the independently loaded fixture before closing stdin.

The integration harness owns keys, independently seals and authenticates the
messages, and compares the managed server's reply. This utility only transports
ciphertext. A successful loopback exchange establishes interoperability with the
primary ENet implementation for the exercised reliable channel, fragmentation,
and message size. It does not qualify packet-loss recovery, network migration,
all channels, or a complete Moonlight streaming session. This POSIX test wrapper
is intended for a native development host, not an installed transport service.
