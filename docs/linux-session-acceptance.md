# Linux session acceptance runbook

This runbook accompanies [the Linux session execution plan](linux-session-execution-plan.md).
It records, per plan milestone, what evidence exists and what is still owed, and it
defines the host-safety protocol and the exact commands every native check uses.

**No milestone is claimed passed by this document.** A build, a fake backend, encoded
metadata, device enumeration, or a clean read-only comparison never establishes a
client-visible milestone. Anything that needs a real Moonlight client, a real game, or
a privileged operator step is `pending` until the user runs it and records the result.

## 1. Evidence kinds

Each milestone is tracked in five separate columns so that one kind of evidence is
never mistaken for another.

| Column | Meaning |
| --- | --- |
| Implementation | The behavior exists on `main`, exists only on an unmerged branch, or does not exist. |
| Focused tests | Unit or loopback tests of that behavior pass. They do not prove the whole path. |
| Integration | The composed host (signed requests, session, modules) passes against fixtures or fake backends. |
| Native | A bounded check ran on this host against the real kernel, compositor or audio server, following section 3. |
| Real client | A real Moonlight client (and, where stated, a real game) observed the visible pass condition. |

Cell values: `pending` (nothing recorded), `partial`, `branch` (exists only on an
unmerged branch, not on `main`), `blocked: <reason>`, or a dated result with a pointer
to its evidence. A cell is changed only together with the commit or evidence path that
justifies it.

## 2. Milestone status

Recorded 2026-09-30 from the round briefing and the read-only tool in this change.
Implementation facts come from the briefing and were not re-derived by this task; the
integrator updates a cell when work lands. Every Real client cell is `pending`: no
Moonlight session has been run.

| # | Milestone (visible pass condition in the plan) | Implementation | Focused tests | Integration | Native | Real client |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Pair Moonlight: a fresh client pairs, sees the game, stays trusted after a host restart | partial: nvhttp and pairing exist; `ServerCodecModeSupport` is hardcoded to 0, so moonlight-common-c aborts `LiStartConnection` (Connection.c:228-233) and no client can stream; Unpair is administrator-only | pending | pending | pending | pending (R1, R2) |
| 2 | Own the session: rejection or cancellation leaves the desktop unchanged; retries allocate nothing twice | `branch`: session ledger (journal, obligations) is in the lifecycle task; the read-only baseline and comparator exist (this change) | 84 unit tests for the baseline tool only | pending | read-only baseline and compare ran clean on this host, 2026-09-30 (section 6); this is tool evidence, not milestone completion | pending |
| 3 | Create the screen: KDE sees the intended screen; releasing it cannot affect another session's output | `branch`: Linux VKMS lease provider (a8a1441) | pending | pending | blocked: mutating owned-output checks need operator decision E2 (section 5) | pending (R3) |
| 4 | Configure the screen: readback shows the requested compositor state and monitor policy | not started: KScreen apply and restore is the next display-capture task | pending | pending | pending | pending (R3) |
| 5 | Capture real frames: moving pixels from the exact owned output, SDR and 10-bit HDR verified separately | partial: capture-kms enumerates; `vibeshine_drm` frame producer and managed routing are `branch` | pending | pending | blocked: `vibeshine_drm` ioctls need `CAP_SYS_ADMIN`, operator decision E3 | pending (R4) |
| 6 | Deliver video: Moonlight shows the desktop, HDR renders, keyframe requests recover | partial: software x264 works; Linux NVENC H.264 is `branch`; HEVC, Main10 and HDR signaling do not exist | pending | pending | pending | pending (R4, R9) |
| 7 | Deliver audio: intended audio is heard with acceptable sync and no lasting routing change | partial: Linux capture exists; `main` has a Pulse callback use-after-return hazard; owned-sink recovery is not landed | pending | pending | pending | pending (R5) |
| 8 | Launch the game: a real game starts on the owned screen, existing Steam and games intact | not started for Linux: Desktop launch kind is in the lifecycle task; Steam handoff tracking is not this round | pending | pending | pending | pending (R6) |
| 9 | Make it playable: actions and controller feedback work in game; disconnect leaves nothing held | partial: native input modules exist; injection into the live session is disabled (E4) | pending | pending | blocked: E4 (pointer containment is an open design decision) | pending (R7) |
| 10 | Quit and restore: client quit and natural exit restore the saved usable state with no orphans | not started | pending | pending | pending | pending (R8) |
| 11 | Recover interruptions: each interruption restores the desktop or exposes a concrete recovery action | not started | pending | pending | pending | pending (R8) |
| 12 | Sustain gameplay: video, audio, input, latency and restoration hold under heavy GPU load | not started | pending | pending | pending | pending (R9) |

## 3. Host-safety protocol (mandatory)

This machine is the user's live KDE desktop. The qualification flow never changes the
following, and any check that might touch them stops instead:

- `Virtual-1`, the active desktop screen. It is off-limits in every operator flow: it is
  never acquired, reconfigured, restored, released or named `--owned`. The tool refuses
  `--owned Virtual-1` with exit status 2.
- `HDMI-A-1` (connected, currently disabled) and every other real output.
- The running Vibeshine host, its session broker and helpers, its outputs, and its sinks
  (`sink-sunshine-*`), plus the Arctis and iec958 sinks and the default sink and source.
- The user's Steam, any running game, and existing Moonlight client trust.
- Firewall, UPnP, router and device ownership, ACLs, groups and kernel modules. No sudo,
  setcap or service restart is part of any check.

Rules for every native check:

1. **Read-only first.** Run `preflight` and prefer read-only discovery. The tool in
   `scripts/linux_host_snapshot.py` runs only allowlisted read-only commands and never
   opens a device node (`stat`, `getfacl` and `os.access` only).
2. **Build outside the lock.** Run `cargo test --no-run` or a release build first, then
   run only the built artifact under the lock.
3. **Serialize host-global checks.** Run under
   `flock -w 900 /home/chasep/sources/amaterasu-targets/native.lock timeout -k 10 <seconds> <command>`.
   The lock file is created by the integrator; do not delete or bypass it.
4. **Fresh baseline before every mutation**, saved to a new private file, and a
   `compare` against that exact file afterwards. A baseline is never reused across runs.
5. **Act only on resources the run owns.** Owned names are `Virtual-2` to `Virtual-4`
   (only after the owned-output provider proves ownership), `amaterasu_qual_<pid>` audio
   sinks and the PIDs the run spawned. Stop spawned PIDs with TERM, then KILL after a
   bounded wait.
6. **Bounded deadlines for restore.** Teardown has a deadline; the final `compare` uses
   `--wait-restored` with a stated number of seconds. A run that cannot restore stops and
   reports; it does not try a different backend or change host-wide configuration.
7. **Loopback sockets only** with ephemeral ports. The live host owns the default
   Moonlight ports (47984, 47989, 47990, 48010 and UDP 47998 to 48003). Remote-bind tests
   are separate `#[ignore]`d cases with a stated reason. Never add firewall rules.
8. **Disk.** Stop and tell the integrator if `/home` has under 12 GB free. `preflight`
   fails below that.
9. **Input injection into the live session stays disabled (E4).** A relative uinput
   pointer can leave an owned output and move the real cursor.

If any step reports drift on a protected resource, stop. Do not repair `Virtual-1`,
`HDMI-A-1`, the default sink or the Vibeshine host from the test; report the difference
and the baseline path.

## 4. Commands

All commands run from the repository root. `scripts/linux_host_snapshot.py` uses only the
Python standard library. The examples are POSIX `sh`; this login shell may be fish, so run
multi-line sequences through `sh -c '...'`.

### 4.1 Preflight (read-only)

```sh
python3 scripts/linux_host_snapshot.py preflight
python3 scripts/linux_host_snapshot.py preflight --json > "$XDG_RUNTIME_DIR/amaterasu-preflight.json"
```

Reports the display state (including `Virtual-1` HDR and scale and which `Virtual-N`
connectors are free), audio defaults and stale `amaterasu_*` sinks, device access for
`/dev/dri/*`, `/dev/uinput`, `/dev/uhid`, `/dev/vibeshine-ds5` and the helper socket,
kernel modules, Vibeshine host PIDs, busy default ports, free space, the native lock and
GPU tools. It marks operator decisions E2 and E3 `pending` instead of failing.
Exit 0: no failing check. Exit 1: a check failed (low disk, stale Amaterasu sink).
Exit 2: required evidence (displays, audio, devices, processes) is unavailable.

### 4.2 Baseline snapshot

```sh
BASELINE_DIR="$XDG_RUNTIME_DIR/amaterasu-qualification"
BASELINE="$BASELINE_DIR/baseline-$(date +%Y%m%dT%H%M%S).json"
python3 scripts/linux_host_snapshot.py snapshot --out "$BASELINE"
```

Writes a JSON file stamped with time and host. The directory is created `0700` and the
file `0600`; an existing directory that is not private, or an existing file, is refused.
The snapshot contains, for each output, connected, enabled, mode, refresh, position,
priority, scale, rotation, HDR and WCG (disabled connected monitors included); the PulseAudio
default sink and source, sinks, sources, modules and playback streams with the sink each is
on; type, mode, owner, group, ACL and access for the device nodes; the kernel modules;
D-Bus `NameHasOwner` for PipeWire reservation and portal names only; NVIDIA encoder
session counts and VAAPI profiles when the tools exist; protected process identities
(PID, start time and program name, never arguments); listeners on the Moonlight port
range; and free space. Missing tools are recorded as `unavailable` with a reason. The tool
refuses to write a baseline when required evidence is unavailable (exit 2).

### 4.3 Compare after the run

```sh
python3 scripts/linux_host_snapshot.py compare --baseline "$BASELINE"
python3 scripts/linux_host_snapshot.py compare --baseline "$BASELINE" \
    --owned Virtual-2 amaterasu_qual_12345 --wait-restored 30
```

`--owned` names resources the run created; only `Virtual-2` or higher, `amaterasu_<name>`
sinks and PIDs are accepted, so the live desktop and the default audio cannot be excused.
`--wait-restored SECONDS` polls until the host matches the baseline and exits 1 at the
deadline. `--poll-interval`, `--json`, `-v` (list restored items) and
`--allow-owned-present` (for a mid-session check that expects the owned resource to exist)
are optional.

| Exit | Meaning |
| --- | --- |
| 0 | Clean: every compared item matches, or is an owned resource that is gone. |
| 1 | Drift or not restored (including an owned resource that still exists). |
| 2 | Evidence unavailable, malformed or from another host, or a usage error. |

| Class | Meaning |
| --- | --- |
| `restored` | The item in the baseline is present and identical now. |
| `owned-and-gone` | A resource named `--owned` was in the baseline and is now absent. Allowed. |
| `owned-present` | A named owned resource exists and differs from the baseline. Fails unless `--allow-owned-present`. |
| `drift` | Any other difference: a changed, added or removed output, sink, device ACL, module, stream move, protected process or listener. |
| `unverified` | A reading was unavailable. Two unavailable readings never count as restored: a required section fails with exit 2, an advisory section that is unavailable on both sides is listed but not counted. |
| `info` | Reported only: session-helper churn, playback streams starting or ending, free space, a reboot. |

Displays are compared by connector name and meaningful state; KScreen mode-id numbers and
output order are ignored. `Virtual-1`, `HDMI-A-1`, the default sink and source, and the
PIDs (with start times) of the Vibeshine host, Steam and games must be unchanged.
Session-broker and helper processes are short-lived by design and are only counted.

### 4.4 Wrapping a native check

```sh
sh -c '
  BASELINE="$XDG_RUNTIME_DIR/amaterasu-qualification/baseline-$(date +%Y%m%dT%H%M%S).json"
  flock -w 900 /home/chasep/sources/amaterasu-targets/native.lock timeout -k 10 600 sh -c "
    python3 scripts/linux_host_snapshot.py preflight &&
    python3 scripts/linux_host_snapshot.py snapshot --out $BASELINE &&
    <built native artifact> ;
    ARTIFACT_STATUS=\$? ;
    python3 scripts/linux_host_snapshot.py compare --baseline $BASELINE \
        --owned <names the artifact reported creating> --wait-restored 30 ;
    COMPARE_STATUS=\$? ;
    exit \$(( ARTIFACT_STATUS != 0 ? ARTIFACT_STATUS : COMPARE_STATUS ))
  "
'
```

Name only what the artifact's own log says it created. If it crashed and leaked a
resource, run `compare` once without `--owned` to see the leak as drift, then report it;
removing a leaked output or sink is a separate, owner-approved recovery action, not part
of the check.

## 5. Pending operator decisions

These are the user's to make; nothing in the repository or the workers performs them.

- **E2, helper socket.** `/run/vibeshine/vkms-control.sock` is `root:vibeshine-vkms` mode
  0660 and the current user is not in that group, so a mutating owned-output check cannot
  connect. The user either adds the account to the group (then logs in again) or approves a
  different account for the helper. Read-only checks do not need it.
- **E3, `CAP_SYS_ADMIN`.** `vibeshine_drm` ioctls return `EACCES` without it. The user
  approves granting `cap_sys_admin+p` to a copy of the native test or host binary. Never
  grant it to the user's installed host binary.
- **E4, pointer containment.** Input injection into the live KDE session stays disabled
  until a design keeps a relative pointer on an owned output.
- **`Virtual-1` is never an option.** It is the active desktop; no run may take it. If no
  separate owned output is available, stop before display mutation.

## 6. Recorded native read-only evidence

This is evidence that the tool itself is read-only and self-consistent on this host. It is
not completion of any milestone.

- 2026-09-30 21:19 local, host `AmbidexL`, `scripts/linux_host_snapshot.py` at commit
  `0c12e8d` on `linux-session/qualification-preflight-baseline` (read-only, so it did not
  need the lock): `preflight` exited 0, `snapshot` exited 0 and wrote
  a `0600` file inside a `0700` directory, and `compare` against it immediately exited 0
  (84 items restored, 1 advisory `unverified`: `vainfo` is not installed, 3 informational).
- Observed live state at that time: `Virtual-1` enabled, connected, 3840x2160@120, scale
  2.5, HDR and WCG on; `HDMI-A-1` connected, disabled, scale 2; default sink
  `sink-sunshine-stereo`; Vibeshine host PIDs 1838, 5202 and 5237; `Virtual-2` to
  `Virtual-4` disconnected; `/dev/uinput`, `/dev/uhid` and `/dev/vibeshine-ds5` read/write
  for the user through ACLs; the helper socket not writable (E2); `CAP_SYS_ADMIN` absent (E3).
  The plan's discovery snapshot read `Virtual-1` as 3024x1890@90, so that mode is not
  stable: always take a fresh baseline instead of trusting numbers written in a document.
- The unit tests (`python3 scripts/test_linux_session_preflight.py`) cover the parsers
  against real kscreen-doctor and pactl output, identical-snapshot comparison, each drift
  class, owned-resource rules, unavailable and malformed evidence, private permissions,
  `--wait-restored` expiry, and a guard that every external command is allowlisted and
  read-only.

## 7. Real-client and privileged checks (all pending)

These checks need the user: a real Moonlight client, a real game, or a privileged step.
Wrap anything that mutates the host in section 4.4. Record, for each, the date, the
baseline path, the compare result, the session or operation identifiers and the client
evidence (a screenshot or capture, client statistics, what was seen and heard).

### Client preparation (R0)

1. Obtain a private moonlight-qt build. The recipe from the previous qualification run:
   copy the moonlight-qt checkout to a private directory, then build out of tree with
   qmake (`mkdir build && cd build && qmake6 ../<copy>/moonlight-qt.pro CONFIG+=release && make -j2`).
   Never build in or modify the user's checkout. This recipe was recovered from a lost run;
   re-verify it before relying on it.
2. Isolate the client profile: create an empty directory, put an empty file named
   `portable.dat` in it, and run the client with that directory as its working directory.
   The host list and client identity then live there, never in the user's normal profile.
   The client's CLI verbs are `pair`, `list`, `stream` and `quit`.
3. Run a private Amaterasu host built from `pack-linux-session` (pending: network task)
   with its own state directory and loopback or separate ports. Never attach the test client
   to the live Vibeshine host or reuse its trust. The exact start command is defined with the
   real-client harness in `docs/linux-real-client-checks.md` (pending; not yet written).

### Checks

- **R1, pair (milestone 1).** Pair the fresh client with the private host using its PIN.
  Pass: the client lists the host and the application, and `Desktop` appears.
- **R2, trust survives restart (milestone 1).** Restart the private host with the same
  state directory; `list` from the same profile still succeeds without re-pairing. Pass: no
  pairing prompt. moonlight-qt calls plaintext `/unpair` only to clean up failed pairings.
- **R3, owned screen (milestones 3, 4).** Needs E2. Start a `Desktop` session. Pass: KDE
  shows a new `Virtual-2` to `Virtual-4` screen with the requested mode, scale and HDR
  state; `compare --owned <that output> --allow-owned-present` is clean mid-session while
  `Virtual-1` and `HDMI-A-1` are unchanged. Expect KWin to apply remembered per-EDID
  settings; if HDR or scale is stale the stream must fail closed
  (`unsupported_pixel_format` or a dimension mismatch).
- **R4, moving SDR desktop (milestones 5, 6).** Needs E2 and E3 for the real producer. In
  the client, drag a window and watch a live clock on the owned screen. Pass: decoded and
  presented frame counters increase, the pixels come from the owned output and not
  `Virtual-1`, and a keyframe request (stop and resume the stream) recovers decoding. Host
  metadata or frame counters alone do not pass.
- **R5, audio (milestone 7).** Play a short bounded tone through the stream. Pass: it is
  heard in the client with acceptable synchronization; afterwards the default sink, sinks
  and streams match the baseline (`compare` clean).
- **R6, real game (milestone 8).** Launch one explicitly chosen installed game through the
  session. Pass: it appears on the owned screen, video and audio stay live, and the user's
  existing Steam and other games are unaffected.
- **R7, input (milestone 9).** Needs the E4 design. In a benign window, send a key and
  pointer motion, and one controller input; try feedback (rumble) if available. Pass: the
  remote application sees each event; after disconnect nothing stays held.
- **R8, quit and interruptions (milestones 10, 11).** Quit from the client, then end a
  second game naturally, then drop the client connection mid-stream and kill the host. Pass
  in each case: `compare --wait-restored` is clean, no owned output, sink or process
  remains, and no allocation is duplicated on retry.
- **R9, HDR and sustained load (milestones 6, 12).** After SDR passes, enable HDR on a real
  HDR game and sustain heavy GPU load for the agreed interval, then repeat once including
  an interrupted connection. Pass: 10-bit rendering with correct color, and video, audio,
  input, latency and restoration stay correct. An H.264 SDR fallback does not satisfy an
  HDR request.

R1 and R2 can only run once the wire contract returns a nonzero codec mask; R3 to R9 depend
on the display, capture, encode and lifecycle work that is still landing. Until a user
records the evidence above, every Real client cell in section 2 stays `pending`.
