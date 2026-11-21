# Publishing the Amaterasu repository

Hosts take updates from a publisher's signed repository
([approved repositories](approved-repositories.md)). This is how the project's
own repository is produced. It replaces the single-purpose
[RTX HDR procedure](rtx-hdr-repository-publishing.md) for new publications; the
RTX HDR repository stays readable by the hosts that enrolled it.

## What is published

Amaterasu's own modules ship together as **Amaterasu Core**. One catalog per
platform lists the Core set, and a host moves from one set to the next as a
whole: the host verifies a complete set under one signature, tries it, and
returns to the previous set if it does not start. An update can change any
module of the set and can add one. The same catalog can also list independently
signed native plugins that an administrator can add from **Modules → Available**.
A module from another publisher is that publisher's to list in its own repository.

| | |
| --- | --- |
| Repository ID | `amaterasu` |
| Location | `https://raw.githubusercontent.com/nonary-streams/amaterasu-repository/catalog/<target>` |
| Targets | `linux-x86_64`, `windows-x86_64` |
| Slot | `amaterasu.runtime`: a set is listed at the runtime's version, which is the workspace version in the root `Cargo.toml` |
| Implementation | `amaterasu-core` |
| Publisher key | Ed25519, fingerprint `e89ff0faa32cd120fa58642edff3c0f3e118c9d1099e302b1234dd33870c706c` |

The public key is
`6c399560a1dbe4b646d3be60344fa114d05cdcd7b567a3648958aa1818805b00`. Its
fingerprint is the SHA-256 of those 32 bytes. An administrator can compare
the fingerprint above with the one shown in the console.

Each module keeps its own version. In-tree modules share the workspace version;
a module mounted from its own repository carries the version in its
`Cargo.toml`. `scripts/component_versions.py` reads them from the tree a set
was built from, and the publisher writes them into the manifest it signs, with
every dependency following the component it names. Raise the workspace version
for each publication: the catalog files a set under
`native/<implementation>/<runtime version>/`, and two different sets cannot
share that path.

## The publisher key

The private half exists in two places: a `0600` file kept by the maintainer
outside any checkout (`~/.config/amaterasu/publisher/amaterasu-publisher.key`
on the machine that generated it, to be copied to offline storage), and the
`AMATERASU_PUBLISHER_KEY` secret of the `publish` environment in
`nonary-streams/amaterasu-repository`. The release tool reads it only from the
`AMATERASU_PUBLISHER_KEY` environment variable, never from an argument or a
file path, so it does not land in shell history or a process listing.

A repository ID cannot be enrolled again after it is revoked, and an enrolled
ID is bound to one key. Replacing this key therefore means a new repository ID
and a manual step on every host. Treat its loss or exposure accordingly.

## Publishing a set

Build and pack the set as for a local update
([local package updates](local-package-updates.md)); the source is a complete
package signed with a development key that `BOOTSTRAP` trusts. Then, on the
target platform:

```sh
python3 scripts/component_versions.py . PACKAGE/manifest.json \
  --extra-manifest modules/capture-vkms/capture/Cargo.toml > versions.json

now=$(date +%s)
AMATERASU_PUBLISHER_KEY="$(cat ~/.config/amaterasu/publisher/amaterasu-publisher.key)" \
amaterasu-release publish-repository BOOTSTRAP OUTPUT/linux-x86_64 amaterasu \
  https://raw.githubusercontent.com/nonary-streams/amaterasu-repository/catalog/linux-x86_64 \
  "$now" "$((now + 180 * 24 * 3600))" \
  --versions versions.json \
  --set amaterasu.runtime amaterasu-core PACKAGE
```

Repeat `--set` to list more than one set in a catalog. The command verifies
each source package, signs its manifest and the catalog with the publisher
key, and then checks the finished tree exactly as an enrolled host will. It
refuses an existing output directory, a catalog lifetime over one year, a
changed artifact and a slot the set does not contain. It uploads nothing.

To add an independent plugin, [pack its signed manifest and artifacts](plugin-driver-installation.md#package-and-sdk-contracts)
and include `--plugin PLUGIN_ID IMPLEMENTATION PACKAGE` alongside the Core
`--set` argument. Keep previously published plugin choices in later catalogs
as long as installations still use them. The host checks the plugin's exact
dependencies and grants against the active composition before setup.

The catalog sequence must rise with every publication; the Unix time does
that. A host refuses a catalog older than one it has accepted, and a set whose
manifest sequence is below the one it runs.

The generated `OUTPUT/<target>` includes `publisher.pub` alongside
`catalog.json` and `catalog.sig`. Serve the public key file at the catalog
address to enable enrollment by URL. Existing repositories can add the
public key file without changing their signed catalogs.

Publication is the commit that places `OUTPUT/<target>` at `<target>/` on the
`catalog` branch of `nonary-streams/amaterasu-repository`. Review the catalog
and the manifest's component list before pushing. Keep the previous set's
directory on the branch while hosts may still be downloading it.

## Enrolling a host

In the console, open **Modules → Repositories → Add repository** and paste the
address for the host's platform. Choose **Check repository**, review the ID
and signing key fingerprint, then choose **Trust and add** to list its offers.

A Core update also needs the publisher authorized by the installed host.
For an existing Linux user service, wait for streams to end, snapshot the
installation's policy and recovery state, then stop the service and run
`amaterasu-release provision-core-repository ROOT ACTIVE_DIGEST CONFIRMED_FINGERPRINT`
as the service user. This checks the current signed set, enrolls the official
repository, and authorizes its publisher while preserving the current grants
and trusted keys. It leaves the active selection in place. Import and activate
a compatible Core set through the [signed local activation flow](local-package-updates.md)
to move the running package's provenance to the Core feed.

## What an update can and cannot change

The host accepts a new set only when the bootstrap policy it needs is one the
host's protected allowlist already permits. A set that changes module code,
or adds a module whose grants the allowlist names, installs from the console.
A set that adds a module the allowlist has never heard of is refused at
activation, and today the only way to extend the allowlist is the
provisioning command on the host. Adding a core module to hosts already in
the field therefore still needs that step; removing it is host work that has
not been done: the set has to carry the policy it proposes, and the console
has to show an administrator what a new module asks for before the host
records the approval.

Independent native plugins use [the plugin installation workflow](plugin-driver-installation.md).
The host preserves each plugin's publisher signature and checks it alongside
the Core set. The host needs the plugin installation coordinator and must
already authorize the plugin's requested grants. Changes requiring a driver
need separate administrator approval for the signed driver payload.

## What is not automated yet

- No workflow builds the product set. The Linux editions need the encoder
  features and their vendor headers on the build machine, so a set is built
  and packed by the maintainer and published with the command above.
- Each core module's own repository builds, tests and attests its libraries in
  continuous integration and on a version tag. Those libraries are evidence
  that the module builds from its repository alone; the published set is
  built from the pinned submodules in this workspace.
- The Windows installer still signs its seed with a key generated on the
  build machine. Moving it to this publisher is a separate change.
