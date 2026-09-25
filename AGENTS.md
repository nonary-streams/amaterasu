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

## Configuration parity execution

- For the active parity task, follow `docs/configuration-execution-plan.md`.
- Use one coordinator and no more than three direct feature workers; no domain
  lead hierarchy or nested delegation. Assign complete behavior across layers.
- Workers use approved build-enabled worktrees and run focused tests, rustfmt,
  and Clippy before handoff. Fix validation failures in the owning worker.
- Commit coherent validated increments and integrate ready work promptly; do not
  wait for a batch barrier or create status-only commits to imply progress.
