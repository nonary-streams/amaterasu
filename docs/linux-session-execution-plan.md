# Complete Linux streaming session

The target is one complete session: pair Moonlight, create an owned virtual
screen, configure HDR, capture and stream the screen, launch a game, play with
audio and input, quit, and restore the desktop. HDR and sustained gameplay under
high GPU utilization remain part of the final target.

This work replaces the active broad configuration-parity assignment. Preserve
the previous teams' source, commits, and evidence. Integrate relevant reviewed
prerequisites without reopening unrelated Windows, macOS, or localization work.
The earlier execution plan remains historical context for those handoffs.

## Delivery order and completion evidence

Deliver real SDR desktop streaming first, then playable SDR gameplay with
reliable restoration, then HDR and high-load qualification. These checkpoints
are intermediate results. A passing build, fake backend, encoded metadata, or
device enumeration cannot establish a client-visible milestone.

| Milestone | Required implementation | Visible pass condition |
| --- | --- | --- |
| 1. Pair Moonlight | Network services, persistent identity, PIN pairing, application list | A fresh real client pairs, sees the game, and remains trusted after a host restart. |
| 2. Own the session | Resolve settings, acquire leases, durably save display and audio baseline before effects | Rejection/cancellation leaves the desktop unchanged; retries allocate no duplicate resources. |
| 3. Create the screen | Adapter for the installed `vibeshine_drm` helper/driver, exact connector identity and ownership | KDE sees the intended screen; releasing it cannot affect another session's output. |
| 4. Configure the screen | KScreen activation, exact mode/refresh, layout, scaling, HDR, physical-monitor policy, rollback | Readback shows the requested compositor state and selected monitor policy. |
| 5. Capture real frames | Presentation wait, DMA-BUF export, synchronization, modifiers, buffer lifetime, cursor, color | Moving pixels come from the exact owned output; SDR and genuine 10-bit HDR are verified separately. |
| 6. Deliver video | Linux GPU encoding, GPU media boundary, codec/depth/color negotiation, HDR signaling | Moonlight displays the real desktop, HDR content renders correctly, and keyframe requests recover decoding. |
| 7. Deliver audio | Linux capture, optional owned sink/routing, Opus, transport, timing, restoration | The client hears intended game audio with acceptable synchronization and no lasting routing change. |
| 8. Launch the game | Preparation after display readiness, Steam/Proton environment, actual-game handoff tracking | A real game starts on the owned screen; existing Steam and unrelated games remain intact. |
| 9. Make it playable | Authenticated native keyboard/mouse/controllers, mappings, permissions, feedback, release | Actions and chosen controller feedback work in-game; disconnect leaves no held inputs. |
| 10. Quit and restore | Graceful game exit, input release, media shutdown, reverse preparation, display/audio restore, output release | Client quit and natural game exit restore the saved usable state with no orphan resources. |
| 11. Recover interruptions | Startup/teardown faults, client loss, capture/encoder failure, restart, retained recovery records | Each interruption restores the desktop or exposes a concrete recovery action without duplicate allocation. |
| 12. Sustain gameplay | High GPU load, repeated cycles, mode/HDR changes, relevant display loss | Video, audio, input, latency, and restoration remain correct under intended gaming load. |

Record implementation, focused tests, integration, native qualification, and
real client/game acceptance separately. Pending runtime checks stay pending.
When client hardware is inaccessible, finish reachable implementation and
provide reproducible checks for the user; do not convert that limitation into
an implementation completion claim.

## Session ownership and ordering

1. Authenticate the peer and freeze client/application settings and negotiated
   media intent. Validate the complete request before resource mutation.
2. Acquire durable transaction ownership. Save the display baseline, including
   disabled connected monitors, exact modes, positions, priorities, scaling,
   rotation, HDR/WCG, and the relevant audio routing baseline.
3. Persist the intended owned output operation before creation. Allocate an
   available output, resolve its exact connector/CRTC, and await KDE readiness.
4. Apply the selected display policy. Verify the actual mode and color state
   before preparation, capture, or game launch can claim readiness.
5. Start the real media paths and launch preparation/game through the owned
   transaction. Track the actual game independently of a short-lived Steam
   launcher. Preserve unrelated processes and pre-existing environment choices.
6. On quit, natural exit, cancellation, or failure, release held input, stop
   media, undo preparation, restore audio/display, verify restoration, and
   release the exact owned virtual output. Keep recovery records until each
   obligation has confirmed completion.

Interrupted requests must remain inspectable through stable operation IDs.
Lost replies never authorize replaying an ambiguous native allocation. A
provider unable to prove ownership exposes an exact recovery action instead
of removing a connector based only on its name or caller UID.

## Native media boundary

The existing CPU frame and native table prefixes retain their layout. Extend
them through explicitly versioned optional interfaces for GPU frames. Define
DRM fourcc, modifier, bounded planes and dimensions, offsets/strides, per-plane
acquire fences, timestamp/sequence, and explicit color/HDR metadata. Metadata
must describe the actual pixels and negotiated encoder output.

A synchronous callback borrows descriptors only until it returns. Consumers
must import or duplicate resources while valid. GPU work must not continue
reading a compositor-owned source after its allowed lifetime. Keeping a
DMA-BUF descriptor open pins allocation lifetime; it does not establish that
the producer cannot rewrite its pixels. Establish source synchronization and
safe copying into owned GPU storage, reject ambiguous presentations, and
verify both coherence and progress under load. Busy accepts nothing and
advances no frame or transport state.

HDR requires an actual 10-bit capture/conversion/encoder path, supported codec
negotiation, correct primaries/transfer/matrix/range, appropriate mastering/CLL
metadata, transport signaling, and real client playback. An H.264 SDR fallback
cannot satisfy an HDR request.

## Parallel execution and ownership

Use six Luna coordinators with workers at Medium through Max reasoning according
to complexity. Native ABI, resource fencing, and interrupted restoration use
Max. Coordinators may delegate to at most three workers; workers do not
delegate. Stay within native capacity, currently nineteen concurrent slots
including the integrator. Reuse completed threads where possible.

| Coordinator | Complete behavior and initial ownership |
| --- | --- |
| Network | Pairing, persistent identity/trust, application-list behavior, Linux product profile and network configuration |
| Lifecycle | Immutable launch settings, session leases/order, baseline coordination, Steam/Proton actual-game tracking, cleanup and recovery |
| Display/capture | Owned Linux virtual outputs, KScreen baseline/apply/restore, exact connector readiness, driver presentation/DMA-BUF producer |
| GPU media | Shared C/Rust GPU/color ABI, GPU frame consumers, native GPU encoder, codec/HDR negotiation and media transport |
| Input/audio | Native audio capture/routing/Opus, keyboard/mouse/controller input and feedback, device release and routing restoration |
| Qualification | Private client profile, native/session acceptance scripts, operator runbook, repeated/fault/high-load checks |

Agree shared interfaces before edits. Lifecycle owns `stream-session` consumer
coordination; GPU media owns the shared media ABI; network owns product-profile
composition. Transfer an overlapping path explicitly rather than duplicating
the same implementation in different teams.

Each worker has a separate approved build-enabled checkout and Cargo target,
two build jobs, incremental compilation disabled, and dev/test debug info
disabled. Run meaningful tests, rustfmt, and strict all-target Clippy before
handoff. The integrator alone changes main and lands reviewed coherent
increments promptly. Preserve source and evidence when retiring idle generated
build caches.

## Host qualification boundaries

Save a fresh baseline for every native mutation. Preserve the running Vibeshine
host, its outputs and sinks, existing Steam, unrelated games, and client trust.
Use separate ports, private persistent host state, and a fresh client profile.
Default socket tests stay on loopback/ephemeral ports; external binding checks
are explicit opt-in tests. Serialize host-global device/GPU/display checks,
identify exact owned fixture PIDs, and enforce bounded test/cleanup deadlines.

On this host, initial readback found `Virtual-1` active at 3024x1890@90 Hz with
HDR/WCG and scale 2.5; `HDMI-A-1` was connected but disabled with scale 2.
That existing virtual screen belongs to the current desktop. Preserve it during
qualification. `Virtual-2` through `Virtual-4` were disconnected at discovery;
recheck availability and ownership at admission. This discovery snapshot is
not permission to reuse a connector after its state changes.

The final acceptance run is: pair a fresh real Moonlight client, launch a real
HDR game on the owned virtual screen, play with audio and controller input under
heavy GPU load, quit from the client, verify restoration, then repeat including
an interrupted connection.
