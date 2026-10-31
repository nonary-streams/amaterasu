# Rubylight capture parity map

This is a source-level comparison, not a claim of runtime parity. Reference:
[Rubylight](https://github.com/RamazanKara/Rubylight) by RamazanKara,
revision `dde04ab28bd9da405e4b70f77765921e474ffa89` (GPL-3.0-only), compared
with Amaterasu revision `9aa36a13` and the working tree on 2026-10-29. Rubylight
was studied as a behavioral reference; no Rubylight code was copied. Recheck both
revisions and working trees before implementing or updating this map.

Scope is Windows video capture through delivery of an owned frame to the video
pipeline, including closely coupled selection, timing, and recovery. Encoding,
networking, general display management and audio are outside scope except where
they constrain capture. Status means source inspection only, not a passed
Windows device test. `Partial` means some related behavior exists, not identical
semantics. `Unknown` means the specific behavior was not established by this
review.

| Feature | Rubylight reference behavior | Amaterasu assessment and parity work |
| --- | --- | --- |
| Provider composition | Direct driver push for owned virtual displays; WGC and Desktop Duplication (DDX) for Windows displays. | **Partial.** Separate WGC and DXGI native modules exist; no equivalent Windows driver-push capture provider is wired into video. Linux `capture-vkms` is a different platform and protocol. |
| Automatic selection | Auto prefers driver push for a display it owns; otherwise WGC, with DDX fallback. | **Partial.** Video has bounded WGC/DXGI startup fallback, based on the bootstrap provider and clean failure categories. It does not implement Rubylight's owned-Windows-display push priority. |
| Explicit selection | WGC and DDX can be selected explicitly; `idd-push` requests driver push with documented WGC fallback. | **Partial.** `ddx`, `wgc`, and `wgcc` can be adapted by video; direct push is absent. Full settings persistence and launch projection must be verified end to end. |
| Display identity | Push only targets a compatible Rubylight-owned virtual display; physical outputs use WGC/DDX. | **Partial.** Windows providers enumerate and reopen an exact output. Add an authoritative owned-display identity and adapter match for a push provider; do not infer ownership from a name. |
| Shared capture | One driver ring per monitor; compatible streams may share capture, while incompatible settings are reported. | **Gap.** Matching Windows provider/ring sharing and conflict policy has not been established. Define ownership, admission, release and multi-client behavior before implementation. |
| Push registration and driver compatibility | Host registers textures and synchronization with its own driver; compatibility and failure trigger a fallback. | **Gap.** Amaterasu's bundled libvirtualdisplay has a separate, unfinished frame-transport design; its broker and IOCTL contract is not Rubylight's. Implement and validate its own negotiated contract instead of assuming compatibility. |
| Push producer | Driver drains the monitor swap chain, copies each composed frame into an available shared slot and drops rather than blocks when all slots are held. | **Gap.** Requires a Windows driver producer and bounded lifecycle, including replacement swap chains and output removal. |
| Push newest frame / static screen | Consumer takes the newest publish; driver retains a last picture for a newly registered ring, including a static desktop. Sequence survives swap-chain replacement. | **Gap.** Specify and test initial image, ordering, sequence, replacement and no-consumer behavior. |
| Push synchronization | Default compatible AMD path uses shared textures and GPU written/released fences; keyed mutex is the fallback. A rejected fence transport remains rejected across same-registration reopen. | **Gap.** Define handle ownership, GPU readiness, reader lifetime, timeout and safe retirement, fallback and reset scope. Windows capture ABI currently has no equivalent D3D shared-texture/fence contract. |
| Push failure policy | Rings that fail before their first frame are retried once before WGC is used; failed transport and display recreation have distinct retry scopes. | **Gap.** Implement and exercise the exact failure policy and observable reason; distinguish clean rejection from uncertain registration or teardown. |
| Windows GPU frame handoff | WGC/DDX frames are held until safe GPU copy into an owned texture; supported push frames can be consumed in place. | **Gap.** Amaterasu's WGC and DXGI paths stage and return tight BGRA8 CPU frames. Add a versioned Windows GPU surface interface with adapter identity, synchronization and explicit lifetimes, plus consumer support. Do not label CPU readback zero-copy. |
| Bounded frames under pressure | Snapshot pool is bounded at eight; WGC retains the newest pending frame, while DDX waits to acquire until space is available. | **Partial.** WGC has bounded capacity-one channels and a pending staging readback; DDX also owns pending staging. Equivalent latest-frame and static-desktop pressure behavior requires direct comparison and tests. |
| WGC service session | Service starts a signed-in-user worker, exchanges GPU frames through three shared textures, and authenticates the local pipe participants. | **Gap.** Amaterasu WGC uses an in-process worker thread with CPU frame handoff; no equivalent service-to-user-session GPU bridge was established. Add authenticated worker lifecycle and resource cleanup if service operation requires it. |
| Lock, UAC and recovery | WGC can fall back to DDX for startup, lock or UAC and retry WGC after returning to the normal desktop. | **Partial.** Amaterasu's video startup fallback is limited to clean admission failures. Provider device/access loss closes the stream; equivalent midstream desktop-transition recovery is not established. |
| Mode, topology and device changes | Capture reopens when relevant source size or color changes; push ring survives swap-chain replacement subject to its protocol. | **Partial.** Amaterasu validates geometry and closes a Windows stream on target, geometry or device changes; transparent equivalent recovery is not established. Test changed target identities and preserve explicit selection. |
| Timing and cadence | WGC uses source timestamps and guarded source-phase pacing; push publishes driver presentation timing. | **Partial.** WGC exposes a separate capture timestamp and freshness with checked fallback; DXGI reports readback-submission time rather than presentation time. Compare pacing policies and add exact clock-domain and wrap/fallback tests. |
| Constant-rate repeats | Rubylight's pacing and latest-picture publication should be compared separately from source freshness. | **Partial.** WGC can repeat cached pixels on caller-scheduled reads and reports freshness; end-to-end `wgcc` routing is documented as pending. Do not count a repeat as a fresh source frame. |
| Cursor | WGC and DDX capture behavior, separate pointer metadata and composition need backend-by-backend comparison. | **Partial / unknown.** Amaterasu WGC has a cursor option; DXGI composes color, masked-color and monochrome pointer shapes in CPU pixels. Validate exact Rubylight behavior, visibility, rotation and cursor-only updates on hardware before claiming parity. |
| HDR source precision | WGC/DDX can capture FP16 scRGB for active HDR, with downstream linear-light conversion; push supports HDR formats. | **Gap.** Amaterasu's Windows capture providers document BGRA8 and no HDR preservation. Add precise format/color metadata and GPU/encoder path; check genuine HDR pixels end to end. |
| Adapter selection | WGC can capture across GPUs; DDX is tied to the display's adapter. Push checks its render/capture adapter. | **Partial.** DXGI selects the output adapter; WGC reports system-selected adapter. Cross-adapter ownership, GPU selection, push identity and user-visible effective adapter require qualification. |
| Errors and visibility | Logs identify actual opened backend and fallback reason; transport fallback warns. | **Partial.** Video reports actual provider and errors, including some fallback reasons. Map all push, service, lock, transport and recovery reasons to observable states. |
| Trust and third-party integration | Rubylight uses its own integrated driver and capture stack. | **Partial.** Amaterasu has an independently built native capture ABI and a controlled substitute, but video provider names are a fixed enum. A project-owned capture implementation needs explicit registration/selection, packaging and grants; native code is trusted, not sandboxed. |

## Correction: project-owned capture integration

The table above compares internal behavior and is useful as background, but it is
not a requirement that Amaterasu reproduce Rubylight's capture internals. For
the intended integration, Rubylight owns acquisition, driver communication,
selection and its internal recovery; Amaterasu needs a supported way to select
the project provider, manage its stream lifecycle, and consume its frames. The
features in the table become requirements at this boundary only when the two
sides need to exchange information or coordinate ownership.

The current capture API can carry synchronous borrowed CPU frames and supports
versioned geometry and freshness extensions. A Rubylight-owned adapter could
produce a compatible CPU frame and implement the expected open/close control
contract, subject to a custom signed composition using an existing service
identity. The video consumer currently accepts only hard-coded provider names,
so a separately named project provider is not yet a general configuration-only
extension. This integration has not been built or tested.

A CPU-frame adapter can require Rubylight to read GPU pixels back to the CPU;
it is a functional integration option, not evidence of equivalent latency,
HDR precision or zero-copy operation. If those properties are required, the
boundary must also represent the relevant Windows GPU resource, synchronization,
lifetime, color and timing metadata, and video/encoder consumers must support
it. Those requirements do not prescribe Rubylight's internal acquisition method.
An adapter must also map source identity and geometry where the host needs
capture selection or input mapping, propagate useful failure and teardown
signals, and honor asynchronous work and frame ownership across calls.

The appropriate next proof is an independently built Rubylight-owned provider
and a real Amaterasu consumer: select it under a project-owned service identity,
open a source, deliver known CPU frames, verify content and stream cleanup, and
exercise failure and multiple-stream behavior. Add and qualify a GPU contract
separately if the integration requires the GPU-resident path.

## Evidence and boundaries

- Rubylight reference: [architecture and capture ownership](https://github.com/RamazanKara/Rubylight/blob/dde04ab28bd9da405e4b70f77765921e474ffa89/docs/architecture.md), [capture settings](https://github.com/RamazanKara/Rubylight/blob/dde04ab28bd9da405e4b70f77765921e474ffa89/docs/configuration.md), [push implementation](https://github.com/RamazanKara/Rubylight/blob/dde04ab28bd9da405e4b70f77765921e474ffa89/rust/windows/src/capture/idd_push.rs), [hardware qualification](https://github.com/RamazanKara/Rubylight/blob/dde04ab28bd9da405e4b70f77765921e474ffa89/docs/hardware-matrix.md).
- Amaterasu: `modules/capture-dxgi/README.md`, `modules/capture-wgc/README.md`, `modules/video/README.md`, `modules/video/src/model.rs`, `crates/media/src/lib.rs`, and `third-party/libvirtualdisplay/docs/frame-transport.md`. The DXGI README documents no physical desktop capture run; WGC source timestamp and hardware behavior also need attached-Windows qualification. Existing Linux DMA-BUF support does not establish Windows GPU sharing.
- Rubylight's reference reports its own hardware results and limits, including unqualified physical HDR and unresolved AFMF generated-frame capture. Those results are not Amaterasu results.

## Broader implementation and qualification inventory

The items below describe full internal feature parity and are not prerequisites
for a basic project-owned capture adapter.

1. Freeze an exact behavior and compatibility contract, including real provider selection, owner-bound control, CPU and Windows GPU frame descriptors, color, timing, fences, and shutdown. Build a separately packaged test provider rather than reserve a built-in service name. Test selection, authorization, ABI negotiation, frame lifetime, backpressure and teardown through the real video consumer.
2. Qualify WGC and DDX capture on Windows first: exact display and cursor, static/moving content, timestamps, mode changes, service and user session, lock/UAC, device loss, and effective-provider reporting. Add recovery only with explicit failure and ownership tests.
3. Design and implement the project's own negotiated libvirtualdisplay push protocol and provider with bounded slot/fence ownership, static initial frame, retry/fallback and crash/teardown recovery. Keep WGC/DDX usable. The existing frame-transport document lists driver security and validation gates.
4. Integrate Windows GPU surfaces and native HDR into preprocessing and encoding; test CPU fallback, adapters, synchronization, frame content and colors independently of latency.
5. Run signed installed end-to-end Windows sessions on supported AMD, NVIDIA and Intel devices as applicable, at 60/120/240 Hz and SDR/HDR, including concurrent clients, pause/resume, adapter/mode changes, producer/consumer failure and restoration. Compare decoded pixels and fresh-frame cadence, then benchmark the same workloads and report measurement boundaries. No hardware or performance parity is established until those checks pass.
