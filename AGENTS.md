# Working on Amaterasu

## Initiative and follow-through

- Infer the user's intent and scope from the request and conversation. Treat
  requests to change, build, or fix something as instructions to do the work;
  make reasonable decisions using the available evidence and continue through
  implementation and appropriate verification.
- Do not ask routine or non-blocking clarifying questions. State material
  assumptions when useful, and ask only when missing information could
  materially change the outcome or when authorization is required. Continue
  independent, authorized work while a necessary answer is pending.
- Before requesting a decision or approval, complete the authorized work
  needed to make the proposed action concrete and reviewable. Read-only work,
  reviews, in-scope fixes, and reversible actions do not need a separate
  confirmation when already authorized or implied by the request.
- Honor explicit user instructions and the specific authorization boundaries
  elsewhere in this file, including protection of unrelated work and active
  streams. Do not invent approval requirements from hypothetical risks or
  interpret routine implementation choices as requiring permission.

## Local worktrees

- On this Linux host, create new Git worktrees at
  `/run/media/chasep/ArchivedGames/worktrees/amaterasu/<worktree-name>`.
  Verify ArchivedGames is mounted from UUID
  `44b0381a-7a54-4be2-a13d-631a0026ab2b` before creating or building a
  worktree; stop if it is unavailable rather than writing to the mountpoint.
- Keep worktree build output on that volume, preferably inside the worktree;
  set `CARGO_TARGET_DIR` there when needed. Preserve existing worktrees and
  use the tests appropriate to the task.

- Write readable Rust with descriptive domain names and normally formatted statements.
- Keep functions focused on one operation. Extract validation, authorization, and
  persistence steps when a function starts mixing responsibilities.
- Separate subsystems into cohesive types and files. Do not move a giant function
  into several files without giving the pieces clear ownership.
- Keep the shell generic. Product behavior belongs in independently built libraries.
- Keep unsafe code at documented ABI or platform boundaries. Explain pointer
  ownership and lifetime requirements with safety comments.
- Run rustfmt, meaningful tests, and Clippy before handing off a change.
- Use Vibeshine as a behavioral reference. Do not copy its source or translate it
  line by line. Record unimplemented parity requirements honestly.
- Keep dependencies small and justified; use vetted cryptographic implementations.
- Native modules are trusted code. Never describe API grants as a native-code sandbox.

## Builds for feature changes

- Prefer building and testing the affected module or modules for a feature or
  fix. Do not rebuild the entire application by default. If a shared library,
  contract, ABI or build feature changes, identify and rebuild every affected
  consumer; use a full build when compatibility or provenance cannot otherwise
  be established.
- For a local component update, verify the replacement is compatible with the
  installed host and other components. Build against the installed source and
  dependency versions when needed, and retain the exact installed bytes of
  unaffected components. Record accurate provenance for the replacement and
  verify the package delta. A complete signed package may contain unchanged
  installed components; a complete package does not require rebuilding them.
- Keep the existing signed import, activation, service-pin and stream-activity
  checks. Run the tests and checks appropriate to the changed modules and report
  installation separately from real client playback.

## Stream failures: warnings and fallbacks first

- **Keep the stream viable. A compatible fallback with a warning is the
  default.** Apply this from the first launch, resume, or RTSP request through
  playback and termination, including paths before a stream record exists.
  A failure is critical only when a specific required condition still prevents
  a valid, authorized stream from starting or continuing after safe fallbacks
  are considered. Name that condition and give the reason at the decision.
- Route every decision to fail the whole stream through the centralized
  Amaterasu lifecycle owner. The owner records an explicit critical disposition
  and concrete reason through the [lifecycle SDK](docs/stream-lifecycle-store.md).
  Lower layers return what failed and why, distinguishing no effect, confirmed
  effect, and unknown outcome. They may contain or retire their own resources
  within their authority. The observing Amaterasu layer reports dependency
  failures when needed, such as video reporting a capture failure. A provider
  error, local failure state, or HTTP/RTSP error status alone cannot decide the
  whole stream's fate. The store records the outcome. Keep diagnostic health
  and delivery off the decision path.
- Give optional probes, providers, hints, and diagnostics a bounded budget.
  On rejection, timeout, missing or invalid replies, use an approved alternate,
  keep the last valid configuration, or skip the optional feature as compatible
  with the request. Account for any uncertain effect under its original operation
  during fallback. Preserve authorization, explicit policy, and agreed media
  and encryption. For an optional codec such as PyroWave, prove the provider
  chain before offering it and verify the client can negotiate a compatible
  standard codec when it is unavailable. Keep HDR or SDR and required bit depth
  intact. The client and host must agree on the effective format before PLAY.
- Log every demotion in host diagnostics and report it through the lifecycle
  SDK to the exact launch or stream attempt. Attach it to stream diagnostics
  and history when available. Include a bounded code, stage, source, reason,
  and effective fallback. Include safe requested and effective values when
  they explain the decision. Report recovery only after the fallback succeeds.
  Use an advisory warning for an issue that needed no fallback. A trusted
  refusal before a stream exists belongs with the request. A rejected request
  against an existing stream leaves its state intact.
- Treat a requested stop or disconnect and its cleanup as separate from
  playback failure. For example, unconfirmed RTSP teardown after Quit is a
  warning, and the Quit response must still report incomplete cleanup. Retain
  the exact cleanup ownership and receipts. Keep ended generations out of new
  admission and fence late results to the original operation. If a required
  resource still cannot be safely acquired, report that specific blocker.

## Mandatory stream architecture review

- Read and apply the [stream failure policy](docs/stream-failure-policy.md),
  [lifecycle ownership and recovery](docs/stream-lifecycle-behavior.md),
  [admission and retirement rules](docs/lifecycle-admission-and-retirement.md),
  and [generation isolation design](docs/lifecycle-generation-isolation-design.md)
  when working on stream initialization, termination, or RTSP traffic. For RTSP
  also follow the [module contract](modules/rtsp/README.md) and the
  [wire negotiation contract](crates/gamestream/src/rtsp/negotiation/README.md).
- Every change that can affect those paths requires a distinct architecture
  review, including changes in a dependency, provider, configuration, or package.
  Trace the affected path from the client request through the responsible owners
  to media startup, refusal, or verified retirement. Trace every changed error
  return that could stop or reject the stream. Verify that each decision to fail
  the stream comes from the central owner and identifies the unmet requirement.
  For an optional failure, test both the warning and actual startup or
  continuation via the fallback. Cover retries, timeouts, late results, and
  cleanup where the change affects them. Verify the negotiated wire contract
  and exact effect ownership across module boundaries.
- Record the exact change reviewed, reviewer, affected paths, findings, and
  supporting evidence before merging, installing, or handing off the change.
  Apply the same review to the plan for a live repair before changing stream
  state. Resolve blocking findings and review the affected paths again after
  changes. Keep test, installed-package, and client-playback evidence distinct.

## Latency-sensitive paths

- Mark the stages of an input event or frame with `amaterasu_module_sdk::trace`
  and reuse the shared names in `trace::names`; see `docs/flow-tracing.md`. End
  every flow you start, including the paths that discard the item.
- Diagnose a stall from `trace.slow_flow`, `trace.slow_span` or the web UI's
  Performance panel before changing code. `runtime.tick_holds` can name the
  module ticked before the slow one; `host.tick` spans time each call directly.
- An operation that completes in microseconds belongs on a direct call
  (`ManagedModule::call_direct`, `docs/direct-calls.md`), not a queued request.
  Decline before any effect when the work needs a later tick, and keep the
  consumer working against a provider that serves nothing directly.
- Never block in `tick`: every module shares the host thread with input and
  video. Work that can take a millisecond belongs on a worker.

## Network tests

- Default tests that open sockets must bind only loopback addresses (`127.0.0.1`
  or `::1`). Prefer ephemeral ports where the test does not require a fixed port.
- Test wildcard and remote address selection without opening a listener when
  possible. Keep tests that must bind `0.0.0.0`, `[::]`, or another non-loopback
  address as separate, clearly named opt-in tests marked `#[ignore]` with a reason.
  Run a specific case with `cargo test -p <package> <test-name> -- --ignored`
  when remote binding needs validation.
- Do not disable Windows firewall notifications or add persistent firewall rules
  for generated test executables as a test workaround.

## Configuration parity execution

- For the active parity task, follow `docs/configuration-execution-plan.md`.
- Use six autonomous feature coordinators, each with up to three workers.
  Coordinators may delegate; workers may not. Assign complete behavior across
  layers and disjoint worker paths. The integrator only lands ready changes and
  resolves actual cross-team overlaps; team coordinators own review and tests.
- Workers use approved build-enabled worktrees and run focused tests, rustfmt,
  and Clippy before handoff. Fix validation failures in the owning worker.
- Commit coherent validated increments and integrate ready work promptly; do not
  wait for a batch barrier or create status-only commits to imply progress.

## Plugin substitutability

- Built-in consumers must use the same versioned contracts and SDK adapters
  offered to external plugin developers. Keep default implementations in
  provider components; a consumer must not silently fall back to one.
- Select providers through trusted startup composition, with explicit grants,
  dependencies and capability checks. Keep provider-specific identities, OS
  handles and allocation ownership out of consumer contracts.
- A reusable operation needs an API contract, SDK support and a replacement
  test that exercises an unchanged consumer against another implementation.
  Native table size and version alone do not establish interface identity.
- Production TCP/UDP/DNS socket operations belong to the network provider.
  HTTP, streaming protocols and library policy consume the Network SDK.
  Bootstrap repository retrieval before plugins can be loaded is an explicit
  exception; preserve its injected RepositoryTransport verification boundary.
- Preserve bounded storage, cancellation and worker retirement when replacing
  providers. Local admission is not evidence of remote delivery.

## Installed stream recovery and package updates

- A request to install locally authorizes preparing, signing with the existing
  local publisher key, importing and activating that local candidate. Do not
  require a shared repository publication or a separate signing approval for
  routine local updates. Shared publication remains a separate action.
- For a production local update, prepare the signed repository layout on disk
  and run `amaterasu-release import-local-native ROOT REPOSITORY BUNDLE MANIFEST_DIGEST`.
  This uses the existing publisher approval, verifies the complete package and
  signed catalog, and advances the normal replay checkpoint without uploading.
  Never enroll a new key or weaken trust just to install locally. Follow with
  the installed host's authenticated activation or `activate-package-offline`
  when the managed service is stopped. Retain policy and service-pin checks.
  See `docs/local-package-updates.md` for the complete procedure.
- Component-only updates may have byte-identical bootstrap policy. Do not
  invent a policy change to satisfy activation: fix the transaction guard if
  it rejects this case. Selection and the service package pin must still advance.
- Preserve the original request through installation and verification. Preparing
  a candidate is not completion. An idle service restart needed for installation
  is authorized by the install request; check stream activity first and request
  fresh permission before interrupting an active stream.

- Diagnose the running service, selected manifest digest and exact launch/cleanup
  error before changing source or runtime state. Preserve a private snapshot of
  recovery journals before an authorized repair; preserve unrelated work.
- Fix the owning provider/coordinator and reproduce the failed transition in a
  regression test. Independent version 1 providers must not require version 2
  composition outputs merely because more than one provider is selected.
- Do not edit lifecycle phases, retirement flags, receipts or cleanup journals to
  simulate progress or successful cleanup. A restart does not prove retirement.
  Use authenticated recovery operations and verify native resource retirement.
- Inspect the installed bootstrap's production/development mode before staging.
  For production, generic `stage` plus a service restart is not activation.
  Use the approved signed publisher (local bundle import is supported) and
  authenticated host activation flow;
  retain signature, publisher, sequence, policy and service-pin checks. Do not
  switch to development mode or rewrite the active selection to bypass them.
- Use a release tool compatible with the installed bootstrap schema. Match the
  replacement to the installed host ABI and preserve other component digests.
  Review the actual package and catalog delta before publishing to a shared
  repository; publishing is separate from preparing a local candidate.
- Updating an already enabled optional provider must preserve its exact
  approved destination policy. An approval's old source values must not make
  subsequent package updates fail; reject values matching neither endpoint.
- If broken cleanup prevents the activation barrier from opening, inspect the
  host's `reconcile-package-activation` path. It verifies repository/catalog,
  publisher, package, selection revision and protected policy before loading a
  candidate with the service stopped. Snapshot state first: the trial can run
  real cleanup against the installation's state. Verify its actual outcome;
  the word "probe" does not mean it is a read-only command.
- Before calling recovery complete, verify the intended digest is loaded, the
  old lifecycle stream is retired, the stream-session cleanup journal is empty,
  and the host reports ready. Then verify a real client launch when available.
  Report source tests, package activation, cleanup and client playback separately.
- Continue the authorized repair through activation and verification. Do not
  end a turn at an intermediate failure when another supported path remains.
