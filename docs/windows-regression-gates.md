# Windows regression gates

These gates were derived from the Windows history at the pinned Vibeshine
reference `aa292c4cb097f22beab19d6ebf0307367f9bf449`. They are not claims that
Amaterasu already passes the scenarios. A compile check cannot close a gate that
requires an interactive desktop, hardware, a driver, or an actual client.

| Reference commit | Required scenario | Owning components | Evidence still required |
| --- | --- | --- | --- |
| `e213ec83c`, `c7d5b7ba0` | Absolute mouse mapping follows the captured output after topology/layout changes | Capture, display, input | Multi-monitor session with live layout change |
| `a37015764` | Explicit WGC target remains selected while temporarily unavailable | WGC capture, session | Disconnect/reconnect target without accidental fallback |
| `7d05d95b6`, `8e8b81b78`, `c0764da40` | Accepted desktop restoration stops recovery and preserves authoritative physical topology | Display recovery | Crash/restart and final-owner teardown with exact restoration |
| `b75ee92a9` | Virtual display scaling is applied before its DPI state is saved | Virtual display, display policy | Create/recreate virtual monitor at non-default scale |
| `b2408d317` | Capture eligibility checks preserve Mouse Keys accessibility settings | Input, capture | Accessibility settings unchanged before/after streaming |
| `f64ee5187`, `0c2f1c9cb` | Partial controller feedback retains trigger releases and DualSense adaptive feedback | Controller input, VHF | Packet interruption and release sequences on supported controller |
| `d2e391e57`, `fd4b4db60` | Nintendo and explicit VHF profiles use the correct fallback | Controller input | Profile negotiation/fallback matrix |
| `9bdac04de` | Audio can capture without changing system default outputs | Audio | Capture selected endpoint while defaults remain unchanged |
| `b94ca73f3` | Queued Playnite game-stop validation is serialized | Application lifecycle, Playnite | Launch/stop/relaunch race with delayed provider responses |
| `b6e755d16`, `ae0fc7897` | Streaming VSYNC preference is separate, and released RTSS versions are recognized | Performance integration | Toggle/preference restoration with supported RTSS |
| `40106ae8c` | Diagnostics and settings remain available in replacement UI | Frontend, management, diagnostics | Route/control inventory and browser acceptance tests |

The captured-output mapping gate remains open. The signed synthetic input test
now verifies negative-origin mapping and held-input cleanup when video reports
geometry failure. That proves session behavior across loaded modules. It does
not prove native coordinate accuracy or continuity through a display change.
Stopping a stream when geometry changes is a safety foundation; following a
changed layout and resuming capture still needs implementation and qualification.
The native acceptance run must include mixed DPI, negative origins, a secondary
adapter, and replacement of a monitor that reuses the same Windows source name.
Sequential topology checks cannot make display enumeration and later input
delivery atomic against concurrent hotplug.

The sibling checkout may move independently. Additional commits inspected at
`d9722f8e6c75e1b84908d2ddf6997684787dce82` include delayed WGC HDR transitions,
unmanaged virtual-display HDR preservation, scaling/dimension retention, and
automatic Vibeshine gamepad-driver fallback. These are follow-up regression
inputs, not a silent change to the pinned parity baseline.

`scripts/inventory_reference.py` extracts 115 route declarations from the pinned
management and Moonlight-facing server files into `reference-routes.json`.
Declarations include conditional and duplicate HTTP/HTTPS entries. The artifact
does not substitute for payload, authentication, status-code, streaming, or
legacy-adapter behavior tests. Selected legacy compatibility still needs an
explicit supported-route contract; no declared route is implicitly complete.
