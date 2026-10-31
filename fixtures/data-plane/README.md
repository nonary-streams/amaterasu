# Independent binary transfer fixture

This cdylib registers `example.binary/1` as a control and native service. It
exercises the generic `TransferApi` through real runtime principals; payloads
are synthetic repeated bytes, not decodable images or video.

Run `python scripts/test_binary_transfer_api.py` from the repository root. The
script builds the fixture and explicitly runs the otherwise ignored dynamic
library test in `amaterasu-runtime`. The test covers a 985,696-byte asset,
32 MiB + 17-byte source record, repeated retained-input frames, authorization,
backpressure, ownership on rejection and teardown before unloading.

Control actions are `open_source { bytes }`, `open_sink {}`, `consume { channel }`
and `close { channel }`. Unknown fields are rejected. The authenticated control
caller owns each channel; clients cannot supply an owner or route. The fixture
reserves at most 128 MiB across four channels, with at most 40 MiB per channel.
It has no automatic expiry and is deliberately not a production asset service.

See [the API and migration contract](../../docs/binary-transfer-api.md).
