# Independent version 2 library

This standalone plugin implements the optional
[library interoperability profile](../../../../docs/application-library-interoperability.md)
using its own immutable startup snapshot. It has no Catalog, store, network or
packet-delivery dependency. It supports bounded search/pagination, rich public
reads and permission-separated opaque execution resolution. It does not scan,
edit or persist a live library. Cursor state is in memory; at most 128 cursors
are retained before the oldest expire.

Startup settings supply a selected `/2` service, exact broker caller/initiator
policies, collection revision, public entries and private execution targets:

```json
{
  "service":"example.independent-library/2",
  "callers":{
    "example.consumer":{"initiators":["example.consumer"],"read":true},
    "amaterasu.launch":{"initiators":["example.consumer"],"resolve_launch":true}
  },
  "revision":"snapshot-v1",
  "entries":[{
    "id":"library/game","revision":"entry-v7","name":"Game",
    "description":null,"tags":[],"source":null,"portable_ids":[],
    "installed":"unknown","readiness":"ready","unavailable_reason":null
  }],
  "targets":[{
    "id":"library/game","revision":"entry-v7","executor":"runner",
    "application_id":"executor-specific-id"
  }]
}
```

The runtime must independently approve registration/call grants. Target keys are
fixed in Launch startup settings; no client can name a broker service or command.
This example's permitted readers share one snapshot. It does not claim to
authenticate humans or isolate device/seat catalogs. Native plugins are trusted.

Build/test from the repository root:

```sh
CARGO_TARGET_DIR="$PWD/target/library-v2-standalone" cargo test --manifest-path crates/module-sdk/examples/application-library-v2/Cargo.toml --offline
CARGO_TARGET_DIR="$PWD/target/library-v2-standalone" cargo build --manifest-path crates/module-sdk/examples/application-library-v2/Cargo.toml --offline
AMATERASU_LIBRARY_V2_ARTIFACT="$PWD/target/library-v2-standalone/debug/libexample_application_library_v2.so" \
  CARGO_TARGET_DIR="$PWD/target/library-provider-interop" cargo test \
  --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml \
  --offline separately_built_library_artifact -- --ignored
```

The artifact commands above use Linux filenames. The separate build directory
keeps the exported-ABI artifact apart from composition tests' `embedded` feature,
which suppresses that symbol for static test linking. The artifact test uses the real
broker and Launch implementation with synthetic session and executor effects;
it does not start a real game or install anything.
