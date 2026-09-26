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
