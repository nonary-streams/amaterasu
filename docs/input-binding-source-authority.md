# Optional input binding source, version 1

This additive profile conveys an opaque lifecycle-issued source identity to the
selected input consumer. It does not identify a paired-client settings scope,
provide a device ID, or make the source handle a bearer credential.

The trusted NVHTTP component sends `source_attestation` only on a session prepare
for a `PeerIdentity` established through verified TLS and the committed pairing
lookup:

```json
{"source_attestation":{"version":1,"kind":"paired_tls"}}
```

The lifecycle service accepts that field only when both broker caller and
initiator are the direct `amaterasu.nvhttp` principal. A missing field leaves a
legacy session unattested. A different shape or caller fails closed. This is an
assertion by trusted native protocol code, not a credential or client-supplied
identity. The existing prepare grant and session ownership checks remain in
force; installing this code does not grant a principal anything. The journal
persists the attestation with the exact owned session; a coordinator restart
clears it and fences old bindings.

The selected input consumer may add `"source_version":1` to its existing
`validate_input_binding` operation. Unsupported versions fail. When the exact
binding is valid, the stream is active, the session remains available, and the
session has the attestation above, its response includes:

```json
{"source":{"version":1,"id":"<32 lowercase hexadecimal characters>","generation":1}}
```

Otherwise the negotiated response contains `"source":null`. Without
`source_version`, the response retains its original shape. The ID is generated
separately for each binding, is not its token, and must be treated as opaque. The
generation is that binding's stream generation; consumers must validate the
full returned stream target `(host_epoch, session_id, stream_id, generation)`
with the source, input-provider boot epoch and device incarnation. Source IDs
and generation are labels, never authorization. `paired_client` remains null.

A parented source validation must retain the session owner's broker initiator;
the selected input consumer may also revalidate its own binding as a direct
caller after activation. Both still require the selected input principal, exact
binding token, current host and stream generation, unrevoked binding and matching
input boot epoch. An input consumer that validates while the stream is ready but
not active gets `source:null` and must revalidate after activation. Pause,
revocation, retirement, failed cleanup and host epoch changes fence admission;
existing owned work must still be drained through the existing retirement path.
An input bridge must recheck these conditions at its own final effect boundary.

This profile does not authenticate arbitrary payloads from clients, change
`paired_client` settings resolution, or establish real hardware or transport
support. Native modules remain trusted OS code; grants are not a sandbox.
