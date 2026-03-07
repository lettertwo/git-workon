# 018 — Minimal-Change, Boundary-Only Validation, No Premature Abstraction

## Context

A codebase that accretes "improvements" beyond what is requested becomes harder to review, harder to revert, and harder to understand. Common failure modes include: adding configurability when none was asked for, validating internal function arguments that are always controlled by trusted code, creating utility modules for logic used only once, and leaving dead code with backwards-compatibility shims instead of deleting it.

## Decision

Three principles govern all changes to this codebase:

1. **Minimal change**: Implement exactly what was requested. A bug fix does not clean up surrounding code. A simple feature does not add extra flags. No docstrings, comments, or type annotations are added to unchanged code.

2. **Boundary-only validation**: Validate only at system boundaries — user input, external API responses, file I/O. Internal functions called from trusted code are not defensively validated. The compiler and type system are trusted to enforce invariants within the codebase.

3. **No premature abstraction**: A helper or utility is created only when the same pattern appears three or more times. Two similar occurrences stay as inline code. Unused code is deleted completely — no `_var` renaming, no re-exports, no `// removed` tombstones.

## Consequences

- Diffs are small and focused, making code review straightforward.
- Reverting a change does not require untangling unrelated cleanup.
- The rule "three occurrences before abstracting" is a heuristic, not a hard line — judgment is required.
- Deleting code aggressively means there are no backwards-compatibility shims; callers must be updated at the same time.

## References

- `docs/adr/018-implementation-principles-guide.md` — annotated examples and per-category rules
- `docs/adr/016-self-documenting-code.md` — related convention for how planned work is expressed
