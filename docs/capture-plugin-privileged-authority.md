# Capture plugin privileged authority

The Linux package authority path has source implementations for signed requests,
explicit administrator approval, protected deployment and broker-mediated worker
launch. It is separate from ordinary API routing grants. This document describes
the contract and the operator workflow; it does not claim a completed privileged
installation or a successful driver, GPU or client playback test.

## Signed requests and explicit approval

A release manifest may contain `privileged_requests`. Each request binds a
feature component to schema 1, the finite `vkms_frame_export_v1` operation, the
exact Linux package target, one DRM card number from 0 through 63, and the
filename, SHA-256 digest and size of a separately packaged helper artifact.
There is no arbitrary command, executable path, capability list or wildcard
resource request. Older manifests without requests retain their existing
behavior. Unknown operations, duplicate requests or artifacts, malformed
identities and unsigned privileged requests are rejected.

Package verification retains the verified publisher key fingerprint and produces
an exact request identity containing the manifest digest, component identity and
artifact digest, target, operation, resource scope and helper identity. The
approval record additionally binds the worker executable digest and its non-root
UID and GID. The approval ID hashes that complete typed record. A publisher key
or a signed request alone does not grant privileged access.

`capture-authority-review` verifies the package and displays its requested
identity, approval ID and concrete access before approval. The operation can
export the approved virtual display's pixels; the administrator must review that
access as well as the publisher and helper. Approval requires an explicit OS
administrative invocation and the exact ID from review. Recomputing a different
identity refuses that confirmation.

Approval copies the worker, trust policy, selected package and helper into a
root-owned deployment, then re-verifies the installed bytes before storing the
protected decision. The copied bootstrap is sealed as a signed production
policy (`development=false`, `allow_unsigned=false`); the source development
root and its signing key are unchanged. No development signing key is copied.
The helper is readable for package verification but executable only by root
(mode 0544), with no file capability. Ordinary users cannot execute that
privileged copy directly. Approval never starts a service or changes the running
session.

## Enforcement and lifecycle

`amaterasu-capture-authority` is a root supervisor for an approved installation.
It verifies protected ownership, permissions, package signatures and artifact
hashes before launching the pinned worker as the approved UID and GID. The
worker receives a private inherited channel after its supplementary groups and
privileges have been dropped. The launcher clears the inherited environment and
supplies a restricted set of process and desktop values. `XDG_RUNTIME_DIR` and
the D-Bus session address are derived from the approved UID. A root service may
explicitly provide `WAYLAND_DISPLAY`; the broker accepts only a bounded
`wayland-*` basename resolving to a socket owned by that UID under
`/run/user/UID`, and forwards it after clearing other inherited values. There is
no public socket accepting a caller-supplied package or component identity.

The broker also requires Linux Yama `ptrace_scope` 1, 2 or 3 to protect the
interval around worker execution. At scope 1, it rejects a launch with an
approved-UID ancestor: use the root system service manager, rather than running
the broker directly beneath a user shell through `sudo`. Missing or weaker
kernel policy is an explicit refusal; these commands do not change it.

The host validates that inherited channel and injects its descriptor into the
approved component's settings. A numeric descriptor in ordinary package policy
cannot confer authority. The capture provider requests restricted helper
sessions through this channel; the broker constrains requests to the approved
DRM card and rejects malformed requests and broader operations. It rechecks the
protected approval while supervising the worker and serving sessions. Withdrawal
refuses new work and closes affected broker sessions. A changed worker,
manifest, signer, component, helper, target or resource scope requires a new
approval and deployment. A restarted broker must read the protected decision
again.

Native modules within the worker share one address space and remain trusted
code. This authenticates and scopes authority available to that worker; it does
not isolate mutually untrusted plugins. Ordinary runtime grants continue to
control cooperative service routing. Neither an API hook nor a routing grant
implicitly grants OS elevation.

The older `--frame-helper` mode remains an explicit compatibility option. Its
separately configured capability-bearing helper is independent of package
approval, and its same-UID peer check is not per-package enforcement. The new
package authority mode does not fall back to that helper when approval or its
inherited channel is unavailable.

## Prepare, review and deploy for a later test

Build the normal Linux session components and the authority-enabled capture
provider. The capture snapshot feature requires its EGL/GBM development
prerequisites and an accessible render node at runtime. Set an absolute
`CARGO_TARGET_DIR` shared by both builds and prefetch the provider SDK and its
other dependencies before building offline:

```sh
cargo build --offline --locked \
  -p amaterasu-capture-authority -p amaterasu-release -p amaterasu
git submodule update --init modules/capture-vkms
cargo build --offline --locked --manifest-path modules/capture-vkms/helper-wire/Cargo.toml
cargo build --offline --locked --manifest-path modules/capture-vkms/capture/Cargo.toml \
  --features linux-kwin-geometry,linux-gpu-snapshot
```

The following names are placeholders for explicit absolute paths. `ROOT` is a
new local publisher root, `ARTIFACTS` is the built component directory, and
`PACKAGE` is a new output directory. Select `CARD` for the intended
`vibeshine_drm` card; do not approve a different card to work around a failure.

```sh
amaterasu-release init-linux-session ROOT \
  --enable-capture --enable-virtual-display --enable-virtual-screen-capture \
  --capture-authority-helper /absolute/path/to/amaterasu-vkms-frame-helper \
  --capture-authority-card CARD
amaterasu-release pack-linux-session ROOT ARTIFACTS PACKAGE 1
amaterasu-release capture-authority-review ROOT/bootstrap.json PACKAGE /absolute/path/to/amaterasu UID GID
```

Review displays an approval ID for each verified request. `UID` and `GID` must
name the non-root account that will own the stream worker. The helper path is
publisher input: `init` writes a local build specification, and `pack` copies
and signs those helper bytes. Neither command installs a privileged executable.

Only after checking the review output, use OS administrator authorization for
the deployment command. Administrative examples assume `amaterasu-release` is
installed as an administrator-controlled executable; do not run a writable
checkout executable as root. `INSTALL_ROOT` must be under protected root-owned
ancestors, for example `/var/lib/amaterasu/capture-authority`; it cannot be in a
user-writable checkout or `/tmp`.

```sh
sudo amaterasu-release capture-authority-approve \
  ROOT/bootstrap.json PACKAGE /absolute/path/to/amaterasu UID GID \
  INSTALL_ROOT CONFIRMED_APPROVAL_ID
```

The command reports the protected package root and approval store. Deploy the
broker executable through an administrator-controlled installation as well; do
not run a writable checkout executable as root. Before a later hardware test,
create a separate mode-0700 state directory as the approved user. Use its
absolute path as `STATE_ROOT`; the broker refuses a root-owned or public worker
state directory. Read `/proc/sys/kernel/yama/ptrace_scope` before the test; the
broker requires 1, 2 or 3 and performs its own check. At scope 1, a direct
`sudo broker run ...` launch has a user ancestor and is refused.

Ask the root system manager to start the protected broker as a separate transient
service. Choose a new unit name and absolute executable and resource paths.
For KWin output geometry, read `WAYLAND_DISPLAY` in the approved user's desktop
session and configure that actual socket basename explicitly. In the example,
replace `wayland-N` with that name; omit `--setenv` only when intentionally using
the Wayland client's default selection. The broker checks the configured
socket's type and ownership and rejects paths or arbitrary environment values.

```sh
sudo systemd-run --system --unit=amaterasu-capture-authority-test --collect \
  --property=Type=exec --property=User=root --property=KillMode=control-group \
  --setenv=WAYLAND_DISPLAY=wayland-N \
  /protected/path/to/amaterasu-capture-authority run \
  INSTALL_ROOT/approvals CONFIRMED_APPROVAL_ID \
  INSTALL_ROOT/packages/CONFIRMED_APPROVAL_ID \
  INSTALL_ROOT/packages/CONFIRMED_APPROVAL_ID/amaterasu STATE_ROOT
journalctl --unit=amaterasu-capture-authority-test.service --follow
```

The system manager, rather than the invoking user's shell, owns the broker's
ancestry. This is a new worker launch, not a modification of an existing user
service. Port conflicts and desktop resources must be resolved before the
operator starts it. Stop this test unit explicitly when finished:

```sh
sudo systemctl stop amaterasu-capture-authority-test.service
```

Revoke the decision with an explicit administrative command:

```sh
sudo amaterasu-release capture-authority-revoke INSTALL_ROOT CONFIRMED_APPROVAL_ID
```

Revocation retains the deployment snapshot. Rebuilding or replacing any approved
identity requires a fresh review and approval; retaining the old files does not
keep revoked authority usable.

## Qualification limits

Keep these states distinct: a signed manifest **requests** access; a protected
record **approves** it; a verified protected snapshot is **deployed**; an absent
channel, missing feature, inaccessible device or rejected driver operation is
**unavailable**; removing the record **revokes** it. None of those states proves
that frames reached an encoder or a client.

Required target-host checks include protected installation and restart,
unauthorized and same-UID direct invocation, malformed and out-of-scope requests,
file replacement, concurrent revocation, real frame export, bounded cleanup,
immutable GPU snapshots, hardware encoding and client playback. Unit tests and
simulated protocol tests cannot establish those driver or hardware behaviors.
No live installation or running-service change is implied by source preparation.
