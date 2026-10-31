# Local session-extension composition

The Linux session-extension composition is an explicit development profile. It
includes the ordinary Linux session components plus the lifecycle, registry,
and actions modules. The display-settings library is built independently and
included by the operator in the same signed component set. The regular Linux
session profile continues to omit those three modules.

Build compatible Amaterasu libraries, including `amaterasu-session-lifecycle`,
`amaterasu-extension-registry`, `amaterasu-extension-actions`, the release tool,
and host, into one artifact directory. Build `nonary.display-settings` in its
own repository using compatible public SDK and extension API sources. The Linux
session package requires the x264-enabled encoder and other artifacts described
in [Linux session profile](linux-session-profile.md).

Prepare a new private development root and a new package directory:

```sh
python3 scripts/prepare_session_extension_profile.py \
  /absolute/path/to/amaterasu-release /absolute/path/to/new-root \
  /absolute/path/to/amaterasu-artifacts /absolute/path/to/new-package 1 \
  /absolute/path/to/amaterasu-display-settings \
  /absolute/path/to/libamaterasu_display_settings.so
```

Options for `init-linux-session`, if explicitly needed, follow `--` at the end.
The preparer uses the selected repository's `examples/component-linux.json`,
`examples/settings-descriptor.json`, and `examples/lifecycle-api-approvals.json`.
It creates an operator bootstrap policy with exact methods and callers, pins the
descriptor digest, configures lifecycle participants and consumer identities,
and signs then verifies the whole set. The plugin's default `enabled` setting
must be false. The action grant list and lifecycle input-action list start empty, so buttons
and shortcuts are not authorized. When the selected provider advertises the
exact supported `nonary.display-settings/remove-owned` action, passing
`--enable-display-actions` before the positional arguments authorizes that
action for session and stream targets through management and input. The
preparer rejects this flag if the matching descriptor and API methods are absent
or incompatible. Provider revisions without that descriptor remain unavailable for this opt-in;
source and hardware qualification remain separate.
The command never stages,
activates, installs, starts, or restarts the host, and it does not change desktop
or network configuration. Both root and package paths must be new.

To verify an assembled set in a disposable root, run:

```sh
python3 scripts/test_session_extension_profile.py \
  /absolute/path/to/amaterasu-release /absolute/path/to/amaterasu \
  /absolute/path/to/amaterasu-artifacts /absolute/path/to/amaterasu-display-settings \
  /absolute/path/to/libamaterasu_display_settings.so
```

This starts a temporary worker with loopback listeners and capture, input, and
display effects disabled. It checks registry discovery and host settings
persistence, then removes its temporary root. Add `--enable-display-actions`
before the positional arguments to test startup with the explicit action policy;
the test does not invoke the action.

To prepare a candidate that can request an owned virtual output and capture it,
pass `-- --enable-capture --enable-virtual-display` after the provider library
path. Add `--enable-input` only when input delivery is wanted. After reviewing
and staging the signed set in a development root, explicitly set the display
extension's `enabled` option to `true` through the extension settings UI or
registry API before starting a session. The default `enabled: false` setting
provides no virtual-display capture candidate. The current lifecycle binding
requires a KDE/KWin PipeWire SDR output; these options do not establish that a
particular desktop or GPU supports it.

To authorize remove-owned actions as well, add `--enable-display-actions` before
the preparer's positional arguments. Choose a keybinding in the extension UI
if shortcut invocation is desired; no shortcut is installed by this profile.
The action grant alone does not prove a live session can safely release its
output. Qualification of the exact host, provider, client, and recovery path is
still required before relying on it.

The new Linux root starts with no explicit legacy display policy or virtual
output list; those defaults resolve to disabled. If configuring product settings,
keep `dd_configuration_option` and `virtual_display_mode` set to `disabled` via
the supported product-settings UI/API. Application or client overrides can also
request legacy display behavior; the lifecycle path rejects conflicting policy.
The preparer does not edit the product state database directly.

Bootstrap policy is trusted local operator configuration; it is not covered by
the component-set signature. The signed manifest binds each selected component,
its bytes, dependencies, and requested routing grants. The installed policy and
signed requests are intersected, so both must permit a service call. Native
modules are trusted in-process code; routing grants are not a sandbox.

The implemented capture binding is limited to Linux KDE/KWin with PipeWire and
SDR. The preparer selects PipeWire explicitly. Windows, HDR, and other capture
providers are not supported by this lifecycle binding. Explicit KMS output or
virtual-screen capture options are rejected by the preparer.

This profile is an integration candidate. A verified signature and successful
startup do not prove end-to-end session lifecycle, action execution, live capture,
platform display changes, or recovery. The current display provider starts with
effects disabled and action authorization disabled by default. This profile requests registry-backed
settings snapshots for sessions; saved-settings propagation and host startup
with the final integrated binaries still need end-to-end qualification. Run the
signed integration fixture and validate the relevant operating-system and hardware scenario before relying
on those behaviors.

## Split-provider upgrade

The composition separates `nonary.virtual-screen/2`, built in the
[virtual-screen repository](https://github.com/nonary-streams/amaterasu-virtual-screen),
from `nonary.display-settings/2`, built in the
[display-settings repository](https://github.com/nonary-streams/amaterasu-display-settings).
Virtual-screen owns the session-retained producer and native leases;
display-settings applies stream configuration to its selected output. A resumed
stream receives a fresh configuration while its session's producer may remain
retained. Both descriptors default `enabled` to `false`; activating
either requires an explicit setting and a supported, approved composition. The
existing profile and commands above select the combined v1 provider; they do not
install or migrate this split composition. The
native `amaterasu.virtual-display/1` service remains a separate lower-level
interface. V1 method schemas remain unchanged; a v2 provider requires its own
approved methods and schemas.

The intended upgrade starts with a fresh isolated profile. Preserve the old
registry document or an export of it, and retire any old virtual resources
through their original owner before moving to the new composition. Native
ownership, lifecycle journals, and outstanding cleanup obligations cannot be
transferred by copying settings or a resource identifier. Existing combined
state or owned resources must block the new composition until resolved; an
unknown or failed cleanup is not proof of retirement.

`scripts/export_session_extension_split.py` prepares a non-mutating,
reviewable mapping from an exported v1 registry document and its descriptor to
the two candidate descriptors. It moves `retention`, `retention_seconds`,
`keepalive_while_paused`, and `enforce_while_paused` to virtual-screen, retains
the other settings with display-settings, and requires an explicit virtual-screen
`enabled` choice for each source scope and the host scope. It preserves the
source bytes and digest, pins descriptor digests, and validates the proposed
settings against the supplied schemas. Its ordered settings-update plans require
an empty destination and compare-and-set revisions starting at zero.

The tool does not contact the registry or apply its plans. It cannot migrate an
existing state root: display-settings retains its provider ID, so an existing v1
document needs an explicit migration and ordinary v2 updates reject it. The plans
require scope authorization, approved descriptors, provider validation, and cleanup
by the old owner before effects. The current preparer configures no registry
migration plans. The export has been tested with synthetic split descriptors;
the final descriptors, live import, and fail-closed upgrade checks still require
validation before this is an executable upgrade procedure. Preserve unrelated
global settings and validate both resulting documents before enabling effects.
