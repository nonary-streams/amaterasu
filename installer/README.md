# Windows installer

## Optional VHF keyboard/mouse package

The VHF keyboard/mouse source is a separate root device and privileged
companion. It is never installed by the ordinary Amaterasu MSI or by approving
an update repository. `prepare-vhf-input.ps1 -OutputDirectory <new-directory>`
builds the x64 UMDF driver and user-mode companion, checks the INF, and runs
Inf2Cat. The resulting catalog and companion are **unsigned and not
installable**. `-WithoutPdb` permits a local diagnostic build when the installed
VS PDB components are inconsistent; it also emits a linker map but does not
replace release PDBs. The build needs VS2022, the WDK 26100 tool package,
InfVerif, Inf2Cat, and Cargo. No signing key is accessed by this script.
The optional installer also requires PowerShell 7 in its protected Program
Files location and an explicitly supplied WDK SignTool and DevCon.
Both executable tools must be Microsoft-signed and located under protected
Program Files directories. A DevCon binary in a user-writable NuGet cache is
rejected; an administrator will need a protected copy before a later approved
installation. The installer does not move tools or change their ACLs.

After a separately authorized signer signs the catalog and companion,
`amaterasu-release seal-vhf-package PACKAGE CONFIRMED_SHA1_FINGERPRINT SIGNTOOL`
checks that Windows trusts both signatures, that the exact independently
confirmed signer matches both files, and that the catalog covers the INF and
DLL. It records SHA-256 hashes in `manifest.json`. The same checks run again
before any install mutation, and after copying into a protected stage. A
repository URL or package-provided fingerprint is not approval. Missing
SignTool, missing signatures, untrusted signatures, changed hashes, or missing
catalog members fail closed. No certificate is silently enrolled.

The separately invoked `install-vhf-package PACKAGE INSTALL_ROOT RUNTIME_ROOT
CONFIRMED_SHA1_FINGERPRINT SIGNTOOL DEVCON` requires an elevated administrator,
the standard Program Files Amaterasu install folder, and a protected runtime
root. It stages only the four fixed artifacts, creates a demand-start
`AmaterasuInputCompanion` LocalSystem service with its service SID, and uses the
specified WDK DevCon to install only `ROOT\AMATERASUVHFINPUT`. It does not start
the service or enable input. A durable journal records exact files, service,
device instance, and published `oemNN.inf`. Device discovery checks the
hardware-ID property and separately records the actual `ROOT\SYSTEM\NNNN`
instance ID. If an OS operation's outcome is uncertain,
`rollback-vhf-package INSTALL_ROOT RUNTIME_ROOT DEVCON` reconciles only that
hardware ID and matching INF, including a root device without a bound driver;
mismatches stop recovery. Rollback
requires an explicit grant revocation and stopped companion service first.
These installer commands are code paths for later approved deployment and have
not been run against a machine.

`grant-vhf-input INSTALL_ROOT RUNTIME_ROOT ACTIVE_SET_SHA256 1|0 1|0` is a
separate administrator action. It requires the currently selected component
set, a completed package installation, matching service/device identities, and
a stopped Amaterasu host; it writes a protected, exact-digest keyboard/mouse
policy. `revoke-vhf-input INSTALL_ROOT RUNTIME_ROOT` removes only that policy.
The new backend has no SendInput fallback. Neither signing nor this policy
turns native code into a sandbox. The installed companion, UMDF ACL and HID
behavior still need controlled end-to-end validation before enabling it.

The installed Ambidex lifecycle-test Amaterasu service currently runs from a
ProgramData test tree with ordinary-user Modify rights. The optional installer
must reject that root; a protected installation is a prerequisite for a
privileged companion test. This finding does not alter or remove that test
service.

`build-installer.ps1` produces a per-machine x64 MSI and local Burn Setup
bundle from a release workspace build. Their definitions are tracked here;
generated WiX fragments and artifacts remain under `target/installer`.

The MSI has a stable Amaterasu upgrade code and performs transactional major
upgrades of earlier Amaterasu packages. It finds Sunshine, Apollo, Vibeshine,
and Vibepollo configuration below the selected Program Files root, runs a
preflight before copying anything, and imports a private, byte-preserving copy
into `%ProgramData%\Amaterasu\legacy-imports`. Supported applications and
settings are materialized into runtime state; source configuration contents are
never written to the MSI log. The mutable runtime root and migration backups
are deliberately not MSI-owned, so uninstall removes the service/program
payload while preserving recovery data.

The signed installer seed is a production policy, not a development root. It
contains a separately signed static management bundle and enables the
management API/UI on `127.0.0.1:47990`. It also enables the NVHTTP control and
pairing listeners on loopback (`127.0.0.1:47989` and `127.0.0.1:47984`) so they
can consume the migrated catalog, authentication verifier, and pairing
identity without exposing a LAN listener by default. The seed deliberately
does not enable capture, encode, audio, RTSP, stream-session, media/control
transport, virtual display, or input injection: those require a separately
signed, machine-qualified media policy and operational stream validation.
The bootstrap does not contain a development signing key or a build-machine
path.

The host must accept this production bootstrap before an installer may treat
the service as ready. Until that host hook lands, the current host rejects
non-development roots and this seed must not be used to retire a predecessor.

An Amaterasu self-upgrade may transition only the earlier installer-generated
development seed to this production policy. Before it writes anything, the
release helper verifies the active installed package, the incoming package,
and the incoming web selection against the exact same single
`installer-release` public key. It refuses unsigned state, a local-development
key, key rotation, a missing active set, or any unsupported schema. The helper
copies the exact prior `bootstrap.json` and `selection.json` to a bounded
transition backup and records any newly staged immutable set. A failed MSI
transaction restores those exact bytes and removes only the attempt-created
set; mutable runtime state is never a transition target. Successful
finalization removes the short-lived backup. Program-files web metadata is
owned by MSI and therefore restored by Windows Installer's own file rollback;
the release helper only verifies it before the policy switch.

Provisioning records an authenticated per-attempt journal. If a later install
action fails, MSI invokes the rollback helper while its release tool is still
installed; it removes only unchanged artifacts created by that attempt and
never uses shell directory deletion. Existing runtime data and backups are not
rollback targets.

The journal is finalized only after a product-owned readiness and fixed-family
legacy-host handoff checkpoint. Windows service state alone is not treated as
streaming health. Handoff records and stops only a validated predecessor
service; it never uninstalls a predecessor package or removes its files. MSI
rollback first stops Amaterasu, restores the recorded predecessor state, and
then reverses journaled provision artifacts.

Direct MSI installation supports a fresh Amaterasu install or an Amaterasu
self-upgrade. It performs the installed helper's fixed-family registration
check before provisioning; a detected Apollo, Sunshine, Vibeshine, or
Vibepollo installation exits with `predecessor_bundle_required`. Use the
generated `Amaterasu-<version>-Setup.exe` bundle for that case. The bundle's
public MSI property is sequencing data, not authorization: only its
SYSTEM-private transaction marker may authorize predecessor handoff.

The tracked Burn bundle marks its chained MSI as coordinated, retains handoff
recovery state until its last vital package has verified a legacy MSI
uninstall, and is the only installation path that may retire a predecessor
package. That package consumes only captured fixed-family MSI registrations;
it never runs an arbitrary uninstall command and restores the prior service
state before failing. A final non-vital, permanent package clears the
Amaterasu provision journal only after retirement. If that cleanup fails, the
bundle does not attempt an impossible post-retirement rollback: the strict,
harmless journal remains for later cleanup and the completed replacement stays
running.

The installed host registers an SCM service and starts its native worker in the
active user session rather than Session 0. It still deliberately does **not**
stop, uninstall, or remove a working Sunshine, Apollo, Vibeshine, or Vibepollo
host. Legacy pairing, listener, authentication, and stream handoff must all
pass their dedicated preflight and health checks before a later package may
perform that destructive cutover. A failed or unsupported migration leaves the
predecessor untouched.

Build it from an elevated Developer PowerShell when the local Windows Installer
service requires elevation:

```powershell
.\installer\build-installer.ps1 -Version 0.1.0
```

For a predecessor replacement, launch the generated setup bundle from an
elevated command prompt:

```powershell
.\target\installer\Amaterasu-0.1.0-Setup.exe
```

The bundle uses one persisted, command-line-overridable data-root variable. Its
default is `C:\ProgramData\Amaterasu`; an administrator using a relocated
ProgramData volume must pass the same root to the whole chain:

```powershell
.\target\installer\Amaterasu-0.1.0-Setup.exe AmaterasuDataRoot=D:\ProgramData\Amaterasu
```

The MSI, predecessor retirement, and post-retirement journal finalizer all
receive that single value.

Validate a clean install using an isolated directory rather than a live legacy
host (the public `LEGACY_PROGRAM_FILES` override is only for this isolated
test):

```powershell
msiexec /i .\target\installer\Amaterasu-0.1.0-x64.msi INSTALLFOLDER=C:\temp\amaterasu-msi-test /l*v C:\temp\amaterasu-msi-test.log
```
