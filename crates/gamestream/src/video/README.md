# GameStream H.264 and HEVC packetization

`VideoPacketizer` creates complete UDP payloads from H.264 or single-layer HEVC
Annex B access units, with optional systematic Reed–Solomon parity. The HEVC
path is intended for 8-bit 4:2:0 output; this packetizer does not inspect SPS
profile, bit depth, color range, or transfer characteristics. It has no sockets,
encryption keys, encoder, pacing loop, or streaming session state. This library
alone does not make Moonlight streaming available.

## Configuration and wire format

`VideoConfig.packet_size` means the negotiated GameStream packet size: the 16-byte
NV header plus fragment data. A UDP datagram adds 16 RTP/extension bytes. The
accepted range is 29..=65491, keeping IPv4 UDP payloads at or below 65507 bytes.
Callers should negotiate a path-MTU-appropriate size; a legal UDP size does not
promise fragmentation-free network delivery.

Set `fec_percentage` to the host's repair policy, normally 20, and
`min_parity_shards` to the negotiated minimum, normally 2. An omitted client repair
percentage is not an explicit zero; the coordinator must choose its host policy.
Zero percentage plus zero minimum explicitly disables FEC. A nonzero minimum with
zero percentage is rejected. `encrypted:true` remains unsupported and fails.

`max_fec_blocks` is mandatory and accepts 1..=4. Use 1 for a legacy version profile;
values above 1 require confirmed multi-block support (Moonlight server version
>=7.1.431). There is no implicit multi-block default. The current HTTP adapter's
7.1.0.0 advertisement does not authorize multi-block video. The short frame header
also requires a compatible version: it is eight bytes for 5.x through 7.1.319 and
>=7.1.350. The historical 7.1.320..7.1.349 twelve-byte variant and long frame headers
are not implemented; the coordinator must reject those profiles.

The packetizer adds one eight-byte short frame header, then fragments the original
Annex B bytes without RFC 6184 FU-A conversion or NAL rewriting. RTP sequence and
timestamp fields are big-endian; NV stream/frame indices and FEC metadata are
little-endian. The 24-bit stream packet counter and 16-bit RTP sequence wrap,
counting parity packets too. Frame indices and 90 kHz presentation timestamps come
from the caller, normally starting at frame 1 and timestamp 0. Successful calls
consume all packet indices even if delivery later fails; rejected calls consume
none. `reservation(access_unit_len)` returns exact datagram and byte counts using
the same FEC block planner without parsing or encoding. The transport reserves
that space before packetization; encrypted video additionally needs its prefix
bytes per datagram. Reservation does not validate the bitstream contents. SOF/EOF flags mark each block, while block indices distinguish actual frame
boundaries. Only the first block contains the short frame header.

For H.264 the frame header reports IDR for NAL type 5. For HEVC it reports IDR
for NAL types 19 and 20. Both paths leave unknown processing latency at zero.
Validation checks Annex B framing, codec-specific NAL headers, and the presence
of a picture; the HEVC path rejects nonzero layer IDs. It does not decode or prove
that parameter sets, slice contents, profile, bit depth, or reference dependencies
are valid.

## FEC and bounds

The original Rust implementation uses the receiver's systematic Cauchy matrix
over GF(256), polynomial 0x11d. Parity coefficients are the inverse of the XOR of
row and column coordinates; no third-party source or generated arithmetic tables
are embedded. A shared 64 KiB multiplication table is generated once at runtime.
The NV stream index/flags and picture bytes are encoded; transport-derived fields
are filled afterward as expected by the receiver's recovery path.

Each FEC block has at most 255 total shards. Data is balanced across at most the
configured block count. The effective percentage is the lowest representable
value at least the requested percentage that supplies the minimum parity count.
The generated count is always `ceil(data_shards * wire_percentage / 100)`, exactly
matching the receiver; no rounded-down count mismatch is allowed. A minimum that
cannot be represented for a particular frame returns `InvalidFec`. Every shard
has the negotiated size, with zeros padding the final data shard; the short frame
header records the actual final payload length. Without FEC, one block of at most
1023 data packets is supported and its final packet is short.

Input is bounded to 8 MiB; FEC output to four blocks of at most 255 UDP datagrams.
Scalar parity work is additionally capped at 268,435,456 bytewise field
multiply/XOR operations per call. `TooManyPackets`, `AccessUnitTooLarge`, or `FecWorkLimit`
reports excess explicitly, with no partial output, consumed indices, disabled
parity fallback, or silent truncation. At packet_size 1392 and 20% parity, four
blocks allow 848 data shards. A local optimized smoke measurement, after table
initialization, averaged about 0.7/4.9/11.7 ms for 64 KiB/256 KiB/1 MiB access units
(ten calls each). This is not a cross-platform throughput or latency guarantee;
the managed transport must account for CPU work and pacing.

## Independent evidence and reproduction

Receiver reference inspected at moonlight-common-c commit
`874ac9548f1bd6f095ef2b435c42cdde460e7821`:

- [Video.h](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/Video.h): field layouts and flags.
- [RtpVideoQueue.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/RtpVideoQueue.c): shard counts, recovery fields and zero-parity completion.
- [VideoDepacketizer.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/VideoDepacketizer.c): frame boundaries, header variants and 24-bit indices.
- [nanors rs.c](https://github.com/sleepybishop/nanors/blob/b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a/rs.c): actual client encoder/decoder and Cauchy matrix.

The pinned sibling was inspected as a behavioral reference, without copying or
translating source. The independent oracle links the actual upstream C codec from
an external checkout. Only an original small test bridge is kept here; upstream
code is not a runtime dependency or included in production Rust.

To reproduce with an existing nanors checkout (no network or automatic downloads):

```sh
crates/gamestream/src/video/fec/build-oracle.sh /path/to/nanors /tmp/amaterasu-nanors-oracle
AMATERASU_NANORS_ORACLE=/tmp/amaterasu-nanors-oracle cargo test -p amaterasu-gamestream -- --include-ignored
```

The build helper verifies commit `b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a` and a
clean tracked tree. Recorded SHA-256 checksums:

- `rs.c`: `e650cec353c2d3430e48c3a171d86622495d9ec1023bb7793d17bb5f05ba50c9`
- `deps/obl/gf2_8_tables.h`: `ed4480ba7cb72217724b24ae2cb51c58c74a10456bb0d7c8317f9a5e613a31db`

Both explicit oracle tests passed. They compare parity byte-for-byte across six
shard shapes, and reconstruct emitted packets after all two-erasure combinations
for a 2+2 block and multiple losses up to the parity budget in 1..4-block frames,
including final padding and counter wrap. Regular tests retain a C-generated
fixture, independent wire reassembly, minimum/count round trips, block/version
limits and error atomicity. Oracle tests are ignored by default because they
require the separately compiled pinned C executable; they do not silently pass
when it is missing.

Actual Moonlight application streaming, the complete C queue/depacketizer,
encrypted video, HEVC decoding/playback, AV1, long frame headers, and
reference-invalidation signalling remain unqualified or unimplemented. Successful
FEC recovery does not qualify the remaining control, transport, audio, timing, or
hardware pipeline.
