# Configuring an application-library provider

This is a development-host example for a separately built, trusted component
that both imports catalog snapshots and implements the SDK provider service.
The bundled example provider is a synthetic fixture: it does not import a catalog
or launch real applications and cannot prove cleanup across a restart. Replace
it with an independently implemented and reviewed provider before using these
bindings for actual applications. Native components run as trusted code; routing
grants do not sandbox them.

The example assumes the Linux session profile, its default management-owned
catalog, and these operator-selected identities:

| Purpose | Identity |
| --- | --- |
| Provider component and catalog importer | `example.provider` |
| Provider service | `example.library.provider/1` |
| Catalog and launch provider key | `example` |
| Streaming initiator | `amaterasu.nvhttp` |
| Catalog owner | `amaterasu.management` |

All identities must match the selected component, its actual registration, and
its code. If using `--catalog-owner console`, use `amaterasu.console` consistently
as the catalog owner instead. Configuring a discovery route does not itself
import applications: the authenticated importer must publish valid snapshots.

## Component and signed set

For a compatible Linux library named `libexample_provider.so`, an external
`ComponentManifest` template can be:

```json
{
  "id": "example.provider",
  "version": "0.1.0",
  "file": "libexample_provider.so",
  "sha256": "",
  "size": 0,
  "abi_major": 1,
  "runtime": false,
  "requires": {"amaterasu.runtime": "0.1.0", "amaterasu.catalog": "0.1.0"},
  "requested": {
    "provide": ["example.library.provider/1"],
    "call": ["amaterasu.catalog/1", "amaterasu.state/1"]
  }
}
```

The template hash and size are placeholders: the publisher recalculates both
from the selected artifact. The exact binary must implement the stated component
identity and service, catalog importer, and provider lifecycle contract; the
template does not supply those capabilities. Static manifest and signature
verification cannot establish that the binary implements compatible discovery,
launch, recovery, or retirement semantics; qualify those behaviors against the
actual selected binary and host. The State call is needed when the
importer's parented catalog requests delegate to State. Add any further grants
only after reviewing the implementation and its complete delegation path.

Add the following **to**, not in place of, the development bootstrap's existing
`policy.principals` approvals. These are additive fragments; preserve every
existing grant required by the selected profile:

```json
{
  "example.provider": {
    "provide": ["example.library.provider/1"],
    "call": ["amaterasu.catalog/1", "amaterasu.state/1"]
  },
  "amaterasu.library": {"call": ["example.library.provider/1"]},
  "amaterasu.launch": {"call": ["example.library.provider/1"]},
  "amaterasu.nvhttp": {"call": ["example.library.provider/1"]}
}
```

The selected component's requests are intersected with bootstrap approvals.
The library and launch services call the provider under parented requests, so
the original NVHTTP caller must also have the service grant. Broker grants apply
to a whole service, not individual actions; the provider must check its
broker-attested immediate caller, initiator, and action rather than treating the
NVHTTP grant as authority to invoke every provider action directly. A provider
must allow `amaterasu.launch` as an initiator for launch's later parentless
monitoring and cleanup, as well as `amaterasu.nvhttp` for the parented start.
Provider-side initiator and capability checks are independent of broker grants.

The call additions for the selected profile go in `CALLS_JSON`:

```json
{
  "amaterasu.library": ["example.library.provider/1"],
  "amaterasu.launch": ["example.library.provider/1"],
  "amaterasu.nvhttp": ["example.library.provider/1"]
}
```

After separately approving the exact component and calls in the development
bootstrap, and building the selected profile artifacts and provider artifact,
a reviewable package can be created with:

```sh
amaterasu-release pack-linux-session ROOT ARTIFACTS OUTPUT SEQUENCE \
  --external-component TEMPLATE_JSON PROVIDER_ARTIFACT \
  --call-grants CALLS_JSON
amaterasu-release verify ROOT OUTPUT
```

Use real paths and a fresh output directory. The Linux session package also
requires the profile’s x264-enabled encoder artifact and its existing runtime
preflight; see [Linux session profile](linux-session-profile.md). The packer checks that each added
call targets a selected service and is separately approved for the named
profile component. It creates and signs one complete set with the development
root's key; packaging and verification do not activate it. The development
external-component path rejects duplicate profile component identities and
filenames. Ordinary inclusion keeps the bundled library; selecting an external
replacement is a separate explicit operation described below.

Independent mixing of native components signed by different publishers and
production repository activation are not supported by this path. Development
repository trials use a complete set from one publisher; installed production
trust management, an add-on installation UI, and hardened approvals remain
outside this development workflow. See [approved repositories](approved-repositories.md).

## Startup bindings

Merge these fields into the existing `policy.settings` objects in the development
bootstrap, retaining the profile's other settings and mappings:

```json
{
  "amaterasu.catalog": {
    "launch_principals": ["amaterasu.launch"],
    "export_principals": {"amaterasu.nvhttp": "amaterasu.management"},
    "import_principals": {"example.provider": "amaterasu.management"},
    "provider_launch_principals": {"example.provider": "example"}
  },
  "amaterasu.launch": {
    "provider_routes": {
      "example": {
        "importer": "example.provider",
        "service": "example.library.provider/1"
      }
    }
  },
  "amaterasu.library": {
    "discovery": {
      "providers": {
        "example": {
          "service": "example.library.provider/1",
          "allowed_initiators": ["amaterasu.nvhttp"]
        }
      }
    }
  },
  "example.provider": {
    "callers": {
      "amaterasu.library": {
        "initiators": ["amaterasu.nvhttp"],
        "discovery": true
      },
      "amaterasu.launch": {
        "initiators": ["amaterasu.nvhttp", "amaterasu.launch"],
        "lifecycle": true
      }
    }
  }
}
```

The `callers` object is the SDK adapter policy shape; an independent provider
must implement equivalent checks and may have its own settings schema. The
library discovery route is optional for a provider used only for catalog import
and launch. Generic discovery does not automatically enable generic catalog
artwork routing: the current library artwork path recognizes its supported
built-in provider types. Review all paths and grants separately.

Catalog accepts a provider launch target only from its authenticated mapped
importer, with `application_id` equal to the snapshot source ID. At resolution it
checks the current importer-to-key and importer-to-owner mappings and an attached
matching marker. Launch independently checks returned importer and source ID
against its route before starting a session. Changing importer, owner, or key can
make existing rows unavailable; do not rebind old rows to a new owner or provider
without a new authorized import.

Launch pins the provider incarnation before start and keeps ownership of the
session and preparation. A lost start is not replayed: launch requests a fenced
stop for the same operation. A changed incarnation, unknown outcome, or invalid
retirement assertion keeps cleanup unconfirmed and the lease retained. The
provider is responsible for durable effect reconciliation and fencing delayed
starts; its retirement assertion is trusted provider evidence, not a host-verified
proof of operating-system state. See the [launch contract](../modules/launch/README.md)
and [provider fixture](../crates/module-sdk/examples/application-library-provider/README.md)
for wire details and test limits.

## Selecting a library replacement

The development Linux session packer supports an explicit replacement at the
component-set boundary. For the read-only SDK demonstration replacement, an
example template for its separately built Linux artifact is:

```json
{
  "id": "example.library-replacement",
  "version": "0.1.0",
  "file": "libexample_library_replacement.so",
  "sha256": "",
  "size": 0,
  "abi_major": 1,
  "runtime": false,
  "requires": {"amaterasu.runtime": "0.1.0"},
  "requested": {"provide": ["amaterasu.library/1"]}
}
```

Before packaging, the bootstrap must approve that exact component's `provide`
grant, and the selected consumers must retain their own `amaterasu.library/1`
call approvals. The example's startup settings use its replacement component ID:

```json
{
  "example.library-replacement": {
    "allowed_callers": ["amaterasu.nvhttp"]
  },
  "amaterasu.catalog": {
    "library_principal": "example.library-replacement"
  }
}
```

These are fragments of `policy.principals` and `policy.settings`, respectively;
merge them with the existing approved policy. The replacement grants fragment is:

```json
{
  "example.library-replacement": {"provide": ["amaterasu.library/1"]}
}
```

Select the exact external component ID when constructing the complete set:

```sh
amaterasu-release pack-linux-session ROOT ARTIFACTS OUTPUT SEQUENCE \
  --external-component REPLACEMENT_TEMPLATE REPLACEMENT_ARTIFACT \
  --replace-library example.library-replacement
amaterasu-release verify ROOT OUTPUT
```

The selector also works with `pack-linux-session-extensions`, which requires
explicit `--call-grants CALLS_JSON`. The profile must contain the bundled
library, and exactly one included external component must provide
`amaterasu.library/1`; its ID must match the selector. The replacement must use
the host ABI major and require `amaterasu.runtime` version `0.1.0`, without
depending on itself or the bundled library. The bootstrap catalog settings must
explicitly bind `library_principal` to the selected component ID. If the
replacement calls `amaterasu.steam/1`, `amaterasu.lutris/1`, or
`amaterasu.playnite/1`, the corresponding component settings must bind its
`library_principal` to that same ID. For every called catalog, State, or artwork
provider service, the replacement must require its component at version
`0.1.0`. Reserved principals and overlap with a catalog launch principal are
rejected. Its identity and filename must not
collide with profile components. The packer removes the bundled library and
rewrites dependent component IDs and versions to the selected replacement in
the same signed set. It does not infer or add the replacement's own requested
calls; approve and declare those explicitly for the actual implementation.
`--call-grants` applies to selected profile components; a request for the omitted
`amaterasu.library` profile ID is rejected. Declare and approve the external
replacement's calls in its own manifest and bootstrap policy instead.

This is complete-set selection for a later worker start, not a live in-process
swap. Staging and verification do not change a running worker; activation and
trial happen at a worker lifecycle boundary. Provider replacement, rollback, and
revocation do not prove that effects from an old worker stopped. Follow the
host's existing lifecycle and recovery procedures and retain any unconfirmed
cleanup state.

The SDK replacement fixture supports a static `list` and `resolve` mode by
default. Its optional `catalog_backed` mode forwards those reads to Catalog as
parent-linked requests and projects only public ID and name fields. In both
modes it admits only configured callers whose broker-attested immediate caller
and initiator match; it rejects incoming delegated calls. It handles one pending
request and does not implement launch, discovery, artwork, or catalog import.
It is a demonstration, not a production-hardened or behaviorally complete
replacement.

For catalog-backed mode, the earlier minimal template and policy fragments are
not sufficient. Add `"amaterasu.catalog": "0.1.0"` and `"amaterasu.state":
"0.1.0"` to the replacement template's `requires`, and add
`"amaterasu.catalog/1"` and `"amaterasu.state/1"` to both its requested
`call` grants and its bootstrap principal approvals. Configure the fixture with
`"catalog_backed": true`; keep its `allowed_callers` explicit. Catalog's
`export_principals` must map each permitted initiating caller to the intended
catalog owner. The initiating caller also needs the catalog and State call
grants for the parented chain. These requirements are separate from the
mandatory catalog `library_principal` binding above.

A signed temporary-development-host test selected this fixture in one complete
set, verified and staged it, then used `amaterasu.console` to create a catalog
entry and directly list and resolve it through the replacement. The replacement
called real Catalog and State through the broker; the test also rejected a
forged payload initiator and a direct unauthorized catalog export. This qualifies
that local fixture path only. It does not test NVHTTP, artwork, actual game
launch, system installation, or end-to-end streaming. A production replacement
must implement and be qualified for every operation its selected consumers use,
including their actual delegation identities and failure paths.
