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
or incompatible. The provider's current published descriptor may still omit
the action; source and hardware qualification remain separate.
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
persistence, then removes its temporary root.

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
effects disabled and its action list empty. This profile requests registry-backed
settings snapshots for sessions; saved-settings propagation and host startup
with the final integrated binaries still need end-to-end qualification. Run the
signed integration fixture and validate the relevant operating-system and hardware scenario before relying
on those behaviors.
