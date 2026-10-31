# Working on Amaterasu

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
