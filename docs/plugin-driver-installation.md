# Plugin packages and driver setup

Modules can add independently signed native plugins without replacing the core
package or unrelated plugins. Setup verifies the offered package and complete
component graph, checks driver requirements without loading the new plugin,
requests interactive administrator approval for missing requirements, installs
through the optional machine service, verifies access as the runtime user, and
activates the complete composition behind the existing stream retirement barrier.
No automatic reboot or driver removal is performed.

## Package and SDK contracts

Independent packages use `amaterasu_package::plugins::PluginManifest`, schema `1`,
explicit host-context schema `1`, project `amaterasu`, a platform target, an
increasing publisher sequence and an expiry. Catalog entries use
`kind: "native_plugin"` and `plugins/IMPLEMENTATION/VERSION/`. Legacy complete
sets and packages with no driver declarations keep their existing behavior.
Hosts without the new format reject it; native ABI and exact component dependency
versions are checked before setup is offered.

A plugin contains components, optional driver requirements, its own component
settings and optional versioned API method declarations. It cannot replace a
core identity, add a runtime, overwrite another component's settings, widen an
existing principal's grants, or introduce an ambiguous provider. Versioned API
methods must belong to the plugin's provider; only its own components and
management can be declared callers. Native grants are routing permissions for
trusted native code, not a sandbox.

`amaterasu_module_sdk::drivers` defines the public `DriverRequirement`, immutable
helper/payload artifact identities, and schema `1` helper exchange. A helper
receives `--amaterasu-driver-helper-v1` and one bounded JSON request on stdin,
and returns one bounded JSON reply on stdout. Actions are `detect`, `install`,
`reconcile`, and `verify`. Replies distinguish missing, incompatible, access
denied, unsupported, installing, ready, reboot required, manual action and
failure. Publishers own these implementations; the coordinator uses this
contract for built-in and external helpers alike.

Requirements bind identity, name, platform, selected feature, reason,
compatibility contract, possible reboot, helper and payload filenames, SHA-256
and byte sizes. Shared identities are shown once with all dependent plugins.
Their feature, compatibility and exact installation artifacts must agree.
Conflicts are rejected before machine changes.

The runnable SDK example generates declarations from actual files:

```sh
cargo run -p amaterasu-module-sdk --example driver_requirement -- \
  org.example.virtual-display display display-v1 ./driver-helper ./driver-package
```

Include the result in the plugin manifest's `drivers` array. Review its name,
reason and compatibility contract before signing. Package with
`amaterasu-release pack-plugin MANIFEST ARTIFACTS OUTPUT` using the existing
`AMATERASU_PUBLISHER_KEY`. Hashes are verified rather than silently rewritten.
`publish-repository --plugin SLOT IMPLEMENTATION PACKAGE` accepts these packages
alongside complete sets. Neither command enrolls a key, uploads, or installs.

## Built-in driver adapters

Build `amaterasu-driver-helper` from `amaterasu-plugin-installation`. Its optional
publisher command reads a flat directory containing only reviewed driver build
inputs, including `driver-adapter.json`, and prints a requirement without running
detection or installation:

```sh
amaterasu-driver-helper --describe-requirement input.uinput ./driver-payloads
amaterasu-driver-helper --describe-requirement display.virtual ./driver-payloads
```

Copy the helper and declared payloads into the plugin's artifact directory before
`pack-plugin`. The descriptor is itself a signed payload. It has schema `1`,
`kind`, exact expected driver `version`, and optional `payload`, `tool`,
`windows_signer`, `catalog`, `client`, `output`, and `prerequisites` fields. References must name
files included in the signed requirement; browser requests cannot supply them.

| Adapter kind | Selected feature | Payloads and verification |
| --- | --- | --- |
| `linux_input` | `input.uinput` or `input.uhid` | Checks the selected device and effective read/write access. Arch/CachyOS installation loads the matching module and installs a per-user udev rule. |
| `linux_virtual_display` | `display.virtual` | `payload` names an Arch/CachyOS package with a matching `.sig`; `output` names the selected DRM connector. Checks loaded module version and a read-only libvirtualdisplay broker status query. |
| `windows_vigem` | `input.controller.x360` or `input.controller.ds4` | `payload` names the signed MSI; optional `client` names the vetted ViGEmClient DLL. Checks the exact bus version and a runtime-user ViGEm connection without creating a controller. |
| `windows_virtual_display` | `display.virtual` | `payload` names the INF, `catalog` its signed catalog, and `tool` the libvirtualdisplay CLI. Include its broker, driver members and DLL dependencies. Checks the exact driver version, protected broker prerequisites and a read-only device protocol query as the runtime user. |

For example, Linux input needs only this descriptor and the helper:

```json
{"schema":1,"kind":"linux_input","version":"kernel"}
```

Windows descriptors pin the expected Authenticode certificate thumbprint in
`windows_signer`. The virtual-display adapter verifies that catalog before the
libvirtualdisplay tool asks Windows to verify and install the INF/catalog
members. Broker executables are retained in a protected machine directory;
the service never points at a temporary helper staging directory. Windows virtual
display setup adds only the runtime user's device read/write ACE through the
supported [PnP security property](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/setting-device-object-registry-properties-after-installation), preserves existing device ACEs, and requests a
restart before verifying access. It does not weaken the broker's administrative
pipe or change the signed INF. ViGEm client
installation retains normal system read/execute access.

The Windows input declaration generator returns no mandatory requirement for
`input.keyboard`, `input.mouse`, or `input.controller.auto`. Explicit Xbox and DS4
backends share one ViGEm requirement. The unfinished VHF backend is unsupported
by this adapter and is never automatically required. Publishers must declare
only the selected feature; separate optional controller components when their
absence should leave keyboard/mouse functionality available.

Linux installation qualifies Arch/CachyOS prerequisites first. The virtual
display adapter checks the running kernel's headers, installs declared local
`prerequisites` packages with matching `.sig` payloads through the distribution
package manager, requires package signatures, configures persistent runtime-user broker access, and reports a
restart/module-signing requirement. It never unloads a live display module,
enrolls a signing key, or reboots. Prerequisite packages are fetched with the
plugin through the Network SDK; the privileged service does not download them.
Missing or mismatched prerequisite bundles produce an actionable manual step.
Other distributions report unsupported when
installation is needed. Existing usable devices can still pass detection.

## Machine service provisioning

Driver approval is disabled until an OS administrator provisions the privileged
boundary. The normal plugin worker remains unprivileged. The default coordinator
is included in management-enabled profiles and receives only its declared host,
network, lifecycle and installation API grants.

Linux user-service bundles include `provision-plugin-installer.py`. During an
explicitly authorized OS installation, run it as root with the installed runtime
root, a protected root-owned host executable, runtime account and approved driver
publisher fingerprints:

```sh
sudo python3 provision-plugin-installer.py --root /path/to/runtime \
  --host /usr/lib/amaterasu/amaterasu --user USER \
  --publisher APPROVED_ED25519_FINGERPRINT
```

It installs a protected policy and a root systemd service. The service accepts
only the pinned host image, expected user, runtime root and `service-user`
process role over authenticated Unix IPC. Root-owned approval/staging directories
and a protected socket parent are required. The root service never writes the
user's composition selection: the runtime user stages it after verification.
Plugins requiring no privileged changes do not need this Linux service.

The Windows SCM supervisor always provides authenticated composition staging for
its exact user worker. Adding a driver-independent plugin, or one whose drivers
are already usable, does not enable or require machine driver installation.
Without a protected driver policy, approval and installer execution are refused.

Windows installers include `tools/enable-plugin-installer.ps1`. Run it from an
elevated administrator session with `-RuntimeRoot`, `-HostExecutable`,
`-RuntimeSid`, and `-PublisherFingerprint`. The existing Amaterasu SCM service
starts the optional machine listener on its next authorized restart; ordinary
plugins remain in the interactive-user worker. Policy ownership and ACLs are
checked when read. The named pipe has explicit SYSTEM/Administrators rights and
interactive-user data rights, excludes remote clients, authenticates the server
against SCM, and accepts only the exact worker PID/creation identity authorized
by that service. See [Microsoft's named-pipe access-control guidance](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).

The policy pins the installed host hash and authorized publishers. A host binary
update must refresh that pin through the OS provisioning step. Web publisher
approval alone cannot expand machine installation authority. Machine requests
name prepared operations and signed requirements; there are no command, path or
argument fields. The service independently re-verifies approved publishers,
catalogs, signatures, exact requirements and current installation revision,
records approval privately, and copies/re-hashes payloads into protected staging
before executing a helper. A self-asserted approval digest is insufficient.

## Operations, activation and recovery

Management exposes `drivers/status`, `plugins/status`, and
`plugins/{prepare,approve,cancel,retry,activate}` through the selected provider's
`nonary.plugin-installation/1` SDK API. Mutations require an interactive web
administrator session; API tokens and viewer/operator sessions cannot approve
installation. Preparation names an approved repository, offered manifest and
installation revision. Subsequent mutations bind operation, string-encoded u64
revision and installation revision; approval also binds the exact review digest.
Stale requests require a new review.

Preparation/downloads run on workers through the selected Network SDK. Downloads
and read-only probes support cancellation; TCP waits are interrupted and joined
before provider retirement. “Not now” leaves approval pending. Cancellation uses
revision checks so it cannot race past the persisted installation intent.
Machine-changing helpers finish their non-cancellable boundary before another
attempt. Helpers run separately with process-tree containment, time/output
limits, private I/O and artifact verification.

The user operation record, machine approvals and bounded receipts survive page
refresh and process restarts. An uncertain install remains in flight and is
reconciled before another install. Reboot/manual-action states are rechecked on
coordinator startup and through “Check again”. Successful installer exit always
requires separate verification as the runtime user. No automatic driver removal
is attempted after failed setup, rollback, or plugin removal.

The coordinator holds the actual stream retirement barrier for machine changes
and activation. New stream admission remains blocked while recovery reconciles
an interrupted machine operation. Compositions retain original publisher package
references and per-publisher/plugin replay floors. Every source is reverified
under approval locks through native loading. Complete-set updates retain the
plugin composition and must still satisfy its dependencies/contracts. An
independent plugin update changes only that plugin, requires the same publisher
and a higher sequence, and uses the same driver review and complete graph trial.

The supervisor retires the old graph, trials the new composition, checks health,
and commits only after startup succeeds. Failed or interrupted trials retain the
previous selection. Readiness and module inventory use the host's loaded graph,
including plugin component declarations, rather than pretending a staged
selection is already running. Rust native images retain their code mappings
until process exit for thread-local destructors; instance/resource shutdown and
worker retirement still happen normally, and existing restart limits apply.

## Validation boundary

Automated coverage includes package integrity/trust/replay failures, exact
requirements and dependency/provider conflicts, shared-driver deduplication,
access denial, async install/reconciliation, stale approvals, cancellation,
reboot states, browser refresh/reconnection, stream barriers with an alternate
coordinator identity, and native composition trial/rollback/revocation.

The ignored host test
`plugin_setup_through_real_user_service_and_privileged_broker` uses real signed
packages from separate publishers, the real coordinator, native loader and Unix
privileged IPC. An alternate Python helper writes only a temporary fixture
marker as root and verifies it as the runtime user. It proves that installation,
verification and activation wiring works without installing a real driver.
It requires separately built runtime/network/coordinator/echo fixtures (echo
with `plugin-installation-qualification`), `AMATERASU_*_PLUGIN` paths,
`AMATERASU_QUALIFICATION_HOST`, and passwordless sudo for its isolated fixture.
It does not restart any installed service or open a stream.
`plugin_setup_reconciles_after_privileged_service_restart` additionally kills
only that fixture's broker during installation, starts a new broker with the same
protected policy and receipts, and requires reconciliation before retry and
activation. Both tests leave installed drivers and running product services alone.

Linux frontend checks, Rust tests, formatting and strict Clippy checks cover the
implementation. AllyTwo also compiled and linked the Windows host, management,
release tool and installer helpers, and ran focused package, installer and native
composition tests. The Windows device-security test exercises descriptor merging
only; it never opens or modifies a real device. Broader Windows host/release tests
still expose profile-ownership and Linux-path fixture assumptions, and are not a
fully passing Windows test suite.

**Production qualification remains separate.** Real signed virtual-display and
input installations, OS service provisioning, kernel/header/Secure Boot variants,
reboot readback, and real client streaming must be qualified on Windows and
Arch/CachyOS before this feature is called production-ready. Source builds and
fixture tests do not authorize deployment, installed driver changes or active
stream interruption.

## Driver inventory and version information

The Modules > Drivers inventory records the last runtime-user verification of the
successfully activated composition. Pending and failed setup appears separately;
cancelling setup does not change the installed inventory. Existing selections
without saved observations show no fabricated driver versions. These snapshots
are not continuous OS monitoring.

A signed requirement may include `offered_version`, for example
`{"version":"1.10.0","scheme":"numeric"}`. This identifies the driver bundled in
that plugin, not the newest upstream release. Publishers opt into numeric ordering
(one to eight decimal components, trailing zeros equivalent); invalid declarations
are rejected. Missing metadata, unknown installed versions and other version
formats report unknown update availability. Older installed versions report an
available update, equal versions match, and newer installed versions are shown
explicitly. Compatibility remains the helper's independent decision: a different
version need not be an upgrade. Installation always uses the existing reviewed,
signed setup flow and never downloads or runs an installer just to check updates.
Legacy declarations omit this optional field without changing their signed bytes.
