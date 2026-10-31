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
and shortcuts are not authorized. The current provider descriptor declares no
actions. Add exact action IDs, scopes, and caller/initiator approvals only after
the provider advertises implemented actions and the authority path is qualified.
The command never stages,
activates, installs, starts, or restarts the host, and it does not change desktop
or network configuration. Both root and package paths must be new.

Bootstrap policy is trusted local operator configuration; it is not covered by
the component-set signature. The signed manifest binds each selected component,
its bytes, dependencies, and requested routing grants. The installed policy and
signed requests are intersected, so both must permit a service call. Native
modules are trusted in-process code; routing grants are not a sandbox.

This profile is an integration candidate. A verified signature and successful
startup do not prove end-to-end session lifecycle, action execution, live capture,
platform display changes, or recovery. The current display provider starts with
effects disabled and its action list empty. Lifecycle currently uses the pinned
bootstrap participant settings; it does not load settings saved through the
registry, so changing a setting in the UI does not change a subsequent session.
The profile has not yet been qualified for host startup with the final integrated
binaries. Run the signed integration fixture
and validate the relevant operating-system and hardware scenario before relying
on those behaviors.
