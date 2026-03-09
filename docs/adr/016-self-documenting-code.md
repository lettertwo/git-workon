# 016 — Self-Documenting Code as Implementation Status Source

## Context

A growing codebase needs a way to communicate what is implemented, what is planned, and what is deferred — both to contributors and to AI assistants helping with the code. Options included a separate roadmap document, issue tracker labels, or embedding status directly in the code. A separate document drifts out of sync; code comments stay co-located with the implementation.

## Decision

Code is the primary source of truth for implementation status. Four conventions are used:

1. **Module-level `//!` doc comments**: Each module begins with a doc comment explaining its purpose, design rationale, and the status of its features. These are browsable via `cargo doc --open`.
2. **`TODO` comments**: Mark deferred features and future enhancements. Discoverable via `rg "TODO"`.
3. **`FIXME` comments**: Mark known issues and areas needing improvement. Discoverable via `rg "FIXME"`.
4. **`unimplemented!()` macro**: Functions that are planned but not yet started use this macro. Discoverable via `rg "unimplemented!"`. These are never removed until the feature is actually implemented.

Before implementing any feature, contributors are expected to read the module-level doc of the relevant file and search for related TODOs and FIXMEs. This prevents redundant work and ensures new implementations follow the existing design intent.

## Consequences

- Status information is always co-located with the code it describes, reducing drift.
- `cargo doc` generates browsable documentation from `//!` comments automatically.
- The convention requires discipline: `unimplemented!()` methods must not be implemented opportunistically unless they are part of the current task.
- AI assistants working with the codebase can discover planned work without reading a separate tracking document.

## References

- `CLAUDE.md` — "Finding Implementation Status" section
- `docs/adr/018-implementation-principles-guide.md`
