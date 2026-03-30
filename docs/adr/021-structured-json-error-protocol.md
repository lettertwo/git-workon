# 021 — Structured JSON Error Protocol

## Context

When `git workon` is invoked by an AI coding agent or a shell script, callers need machine-readable error information. Currently errors always go to stderr as Miette-rendered text — useful for humans, unstructured for machines.

Scripts and agents detect failures via exit code today, but cannot reliably parse the error type or message from Miette's formatted output, which includes ANSI escapes, diagnostic labels, and multi-line help text.

## Decision

When `--json` is active and `cmd.run()` returns an error, `main()` intercepts the error before Miette renders it and emits a structured JSON object to **stdout**, then exits non-zero.

**Schema:**

```json
{
  "error": {
    "code": "workon::worktree::not_found",
    "message": "worktree 'foo' does not exist"
  }
}
```

**Rules:**

1. JSON errors go to stdout — consistent with all other `--json` output. Callers read a single stream.
2. `code` maps directly from the `#[diagnostic(code(...))]` attribute on the error variant. If no diagnostic code is present, `code` is `null`.
3. `message` is the `Display` rendering of the error (short, no Miette decoration).
4. Miette text rendering to stderr is **unchanged** in non-JSON mode — this ADR adds a path, not a replacement.
5. Implementation intercepts at `main.rs:65` (`cmd.run()?` → `cmd.run()` with manual match).

## Consequences

- Agents and scripts using `--json` get a consistent, parseable error envelope on the same stream as success output.
- Existing human-facing error messages are unaffected.
- Error codes are stable API surface; avoid changing `#[diagnostic(code(...))]` attributes on published error variants without a semver bump.
- The `worktree_to_json()` helper is not involved; error serialisation lives in `main()`.

## References

- `git-workon/src/main.rs` — implementation point (intercept before `?` propagates)
- `git-workon-lib/src/error.rs` — diagnostic code attributes
- `docs/diagrams/json-error-flow.md` — decision flowchart
- `docs/rfc/agent-integration.md` — motivation and context
