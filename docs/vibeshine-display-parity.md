# Vibeshine display behavior comparison

Reviewed on 2026-11-11 against local Vibeshine Canary
`43cf27252a205221a295126a5fcd9ad87ba9c914`. The Amaterasu starting revision was
`abcac347c1f74762608004a66801ab56c927f602`; the corrections are recorded in
the subsequent display commits and the pinned display-provider submodule.
Unrelated local Vibeshine changes were preserved.

**Complete behavioral parity is not established.** This comparison covers
display policy, physical and virtual display providers, configuration admission,
launch/resume coordination, and restoration. Passing source tests establishes
the exercised transitions, not compositor, driver, client, or hardware behavior.
The running installation was not updated and real display topology was not
changed for this review.

## Corrected differences

| Behavior | Previous Amaterasu behavior | Correction |
| --- | --- | --- |
| Default `verify_only` | Returned before applying resolution, refresh, remapping, or HDR. | Requires an active target, then applies requested presentation settings while preserving other displays. |
| Resolution automation | Required Optimize game settings for automatic/manual resolution and remapping. | Follows the current reference without that obsolete gate. Explicit client display mode retains priority. |
| Disabled properties | Launch populated client mode values even when the corresponding property was disabled. | Preserves the baseline resolution or refresh independently. |
| FPS remapping | Matched against the selected physical refresh, which may have been promoted. | Carries original stream FPS separately through launch, contracts, and provider resolution. |
| Highest refresh | Could preserve a nearby current rate instead of the highest advertised rate. | Selects the actual highest available rate for the resolution. |
| Scoped configuration | Launch retained only an older subset of display keys and rejected newer settings. | Preserves the complete scoped policy, validates global-only keys separately, and attests the same frozen settings through launch. |
| HDR request override | Stored `auto`/`force_on`/`force_off` without applying them consistently. | Resolves client, application, then global policy into negotiated video, encoder selection, and physical/virtual display intent. Raw client intent and processor source color remain separate. |
| Linux HDR activation | Combined modeset and HDR enable, followed by immediate readback. | Settles mode/geometry first, then enables/rearms HDR and verifies stable state. Independent display-settings consumers use the same ordering. |
| Linux command errors | A zero exit status could hide a KScreen-reported failure. | Rejects reported command failures and retains uncertain recovery obligations. |
| Linux physical displays | General physical topology mutations were unsupported despite stored policy. | Implements journaled active/primary/only-display transactions, mode/HDR composition, exact monitor binding, observed survivor checks, and restoration for supported physical inventories. |
| Linux restoration | Rejected flipped rotations, required current mode data for disabled outputs, and compared some floating values exactly. | Preserves disabled outputs and flipped rotations, compares equivalent timings/scales appropriately, and uses complete fractional desktop bounds. |
| Linux layouts | Native virtual display provider supported only extended/exclusive. | Implements extended, extended-primary, isolated, primary-isolated, and exclusive placement, including peer ownership and primary restoration. |
| Windows HDR state | Could mistake wide color gamut for enabled HDR. | Uses modern HDR state/control with the compatible legacy fallback. |
| Windows inactive SDR target | Could reject activation because the output did not support HDR. | Requires HDR support only when enabling HDR; native query failures remain errors. |
| Windows indirect outputs | Rejected compatible wired indirect displays. | Admits exact bound routes while retaining unsupported transport and modern topology guards. |
| Windows virtual-driver compatibility | Rejected compatible additive protocol minor versions such as 3.8. | Checks required major/minor compatibility without discarding additive driver versions. |
| Windows CCD enumeration | Used active-topology-sized bounds for the larger all-path catalog. | Uses separate bounded candidate limits while retaining exact route checks. |
| Windows permanent displays | Persisted configured count without enacting it. | Reconciles an explicitly configured startup count with the selected driver and verifies independent readback; temporary creation remains available after an honest failure. |
| Private resolution catalogs | Paired preference, ownership propagation, and driver flag were absent. | Preserves the certificate-bound preference through admission, recovery, and exact driver state; requires protocol 3.8, prevents incompatible sharing, and rejects unsupported backends before allocation. |
| Virtual capture readiness | Could accept target metadata without independent active-output proof. | Requires active readiness and preserves cleanup ownership on rejection. |
| Helper completion | A descendant retaining command output could prevent bounded completion. | Bounds output collection and retires the owned command process group on uncertainty. |
| Shared host tick | Native display queries, mutations, and virtual driver operations executed synchronously in the host tick. | Both broker providers use the SDK serialized service worker, retaining transaction ownership and joined retirement. |

The global `dd_remove_virtual_display_after_restore` setting is also preserved
through product migration, the management editor, and signed RTSP admission.
That corrects configuration loss; retain-versus-remove lifecycle effects remain
part of the outstanding restoration policy below.

## Existing behavior retained

- Provider-owned allocation identities, references, native borrows, and recovery
  journals remain authoritative. A lost reply never proves an allocation or
  removal did not happen.
- Linux saves its layout before virtual hotplug, authenticates connector
  ownership through the broker, resolves an exact advertised mode, and retains
  parked recovery when safe removal cannot be established.
- Teardown requires another independently observed active output before
  retiring the last managed scanout. A released reference is distinct from
  confirmed native removal.
- Client display overrides retain precedence over ordinary resolution/refresh
  automation and remapping. Settings are frozen at admission rather than read
  opportunistically during a later module tick.
- RTX HDR's SDR source and HDR encoded output remain separate requirements.
  Request overrides must not relabel SDR pixels as HDR or force the source
  display to HDR against the selected processor's source contract.
- Removed reference HDR-toggle workarounds remain compatibility no-ops.
  Native provider API grants remain authorization for trusted native code.

## Remaining implementation differences

These require implementation work; access to display hardware alone would not
close them.

| Area | Remaining behavior |
| --- | --- |
| Pause/disconnect restoration | `dd_config_revert_delay` (reference default 3000 ms), `dd_config_revert_on_disconnect` (false), and paused virtual-display timeout (7200 seconds) are stored and snapshotted without equivalent retained-source suspend/restore timers. Ending the existing lifecycle would also terminate the retained application and unrelated resources, so it is not a substitute. |
| Retain versus remove | Reference default retains stable virtual identity after restoration. Full retained-source restore and optional remove-after-restore behavior still need a provider-neutral transaction. |
| Retained private catalogs | Changing private preference or resolution on a paused source is rejected while preserving the application. Reference-equivalent idle-source retirement and recreation are not implemented; a retained reconfigure cannot change the driver's private allocation catalog. Linux reports private catalog capability as unsupported. |
| Baseline management | Golden/current/previous selection, golden health and fallback, configured snapshot exclusions, helper-engine selection, and snapshot recovery hotkeys do not have complete equivalent runtime implementations. Saved physical baselines must also exclude managed virtual state using ownership evidence and preserve independently owned outputs. |
| Windows scaling and topology | Nonzero virtual-display scaling is unsupported. Modern virtual-aware clone groups, desktop-image modes, dynamic refresh, and HDR activation on an initially inactive target still have explicit unsupported paths. |
| Windows driver lifecycle | Driver adapter restart/recovery, pre-24H2 refresh restrictions, separate descriptor versus promoted refresh, and reference-equivalent HDR descriptor luminance remain incomplete. |
| Linux recovery integration | Safe-removal guard observes Wayland publication rather than the selected capture provider. Monitor power event handling, manual unplug-all recovery, greeter snapshot adoption, and remembered scale across new connector allocations are not equivalent. |
| Linux permanent displays | The Linux provider explicitly reports permanent-monitor runtime support as false. |
| Linux physical topology limits | Physical clone layouts, new headless physical admission, and mixed physical/managed virtual inventories are safely rejected. An already-at-origin secondary cannot be made primary when the actual native primary is elsewhere until explicit primary intent is represented. |
| Display Settings provider portability | The independent adjustment provider has a Linux implementation; its Windows backend remains unavailable. |
| HDR profiles and workarounds | Client color-profile resolution remains unfinished. Dummy-plug HDR10 behavior, Vulkan HDR exposure, and Wayland HDR compatibility settings lack complete native consumers. |
| Adapter selection | Adapter name/PnP selectors are transported in settings but do not fully determine the capture/encoder adapter. |
| Output selectors | The public display API uses opaque provider identities. Raw imported reference display/friendly names need explicit resolution; treating them as equivalent opaque identities is incorrect. |
| Capacity policy | The reference virtual client limit defaults to 4 with a 1..8 range. Remote-session's separate default/range and ownership model do not establish the same global shared display limit. |

The source reference does not establish a macOS physical-display mutation
implementation equivalent to its Windows/Linux helpers. Absence of such an
Amaterasu backend alone is therefore not recorded as a confirmed divergence.
macOS capture and playback remain separate qualification work.

## Comparison sources

Reference source, rather than older option descriptions, determined behavior:

- `Vibeshine/src/display_device_policy.cpp` and `src/display_device.cpp`:
  preparation, resolution/refresh/remapping, client override, and HDR precedence.
- `Vibeshine/src/config.cpp`, `src/config.h`, and
  `behaviors/display_engine_2/scenarios/`: defaults, configuration scope,
  baselines, orchestration, recovery, and acceptance scenarios.
- `Vibeshine/src/platform/linux/private_display.cpp` and its mode, HDR,
  snapshot, restore, cleanup, and handoff policy headers.
- `Vibeshine/src/platform/windows/display_helper_request_policy.cpp`,
  `display_helper_v2/`, `virtual_display*.{h,cpp}`, and
  `third-party/libdisplaydevice/`: Windows policy and native transitions.
- `Vibeshine/src/stream.cpp`, `src/nvhttp.cpp`, and
  `src/platform/windows/virtual_display_catalog_policy.h`: stream ownership,
  disconnect behavior, and private mode catalogs.

Amaterasu equivalents are `modules/display`, `modules/display-settings`,
`modules/virtual-display`, the display/virtual-display SDK and contracts,
`modules/launch`, `modules/nvhttp`, `modules/stream-session`, pairing, RTSP,
and the management frontend. Provider replacement and durable effect ownership
must remain intact when implementing the remaining behavior.

## Validation and acceptance

| Check | Result |
| --- | --- |
| Integrated Linux contracts, SDK, providers, launch, NVHTTP, RTSP, pairing and lifecycle suites with `linux-libvirtualdisplay` | 2,747 passed; 56 intentional/opt-in checks ignored; the one pre-existing pairing case below explicitly filtered. |
| Default virtual-provider suite | 64 passed. |
| Frontend tests and TypeScript check | 502 passed. |
| Combined Windows display fixtures under Wine | 243 passed; official Windows cross-build and strict Clippy passed. |
| Changed Rust formatting and strict Clippy for contracts, SDK, display providers, launch, stream-session and RTSP | Passed. |
| NVHTTP and pairing Clippy | Passed with the pre-existing unfinished-stage/migration warnings. |
| Touched-path ownership and writability | Verified; all 73 checked paths remain owned and writable by `chasep`. |

Focused provider, contract, launch, stream-session, settings, migration, and
frontend tests accompany the changes. Rust formatting and Clippy are checked
for the changed components. Windows native changes are cross-built and their
platform fixtures run in an isolated Wine environment; those fixtures do not
exercise a real Windows display driver.

The existing unfinished `ResolveHdrProfile` stage still produces a dead-code
warning. Its unused import of an absent contract prevented the starting NVHTTP
revision from compiling; the import was removed while retaining the unfinished
stage.

The independent native check queried the attached HDMI output's KScreen/DRM
identity without changing it. No display mutation or client playback check was
performed.

Native acceptance still requires actual compositor and driver readback, physical
and virtual mode/HDR changes, repeated pause/resume and quit cycles, independent
clients, hotplug/power loss, interrupted restoration, and visible client
playback. Source tests and successful local commits do not establish installed
package activation or those runtime outcomes.

The pairing suite also has a pre-existing failure in
`pipeline::quiescence::tests::uncertain_marker_delete_is_neither_reissued_nor_retired_on_shutdown`.
It was reproduced in a clean checkout of the starting revision. The display
preference tests pass; the integrated pairing run explicitly excludes that one
case. Missing result-retirement acknowledgments in the existing pairing test
harness were corrected without changing production marker-delete behavior.
