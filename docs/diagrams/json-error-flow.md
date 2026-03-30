# JSON Error Flow

Decision tree in `main()` when `cmd.run()` returns an error. The key branching point is whether `--json` mode is active.

```mermaid
flowchart TD
    RUN["cmd.run()"] --> ERR{returns Err?}

    ERR -->|no| SUCCESS["Ok(worktree)\n→ normal output path"]

    ERR -->|yes| JSON_CHECK{"--json\nmode?"}

    JSON_CHECK -->|yes| EXTRACT["extract from miette diagnostic:\n• code = diagnostic_code().map(to_string)\n• message = error.to_string()"]

    EXTRACT --> EMIT["emit to stdout:\n{\"error\": {\"code\": \"...\", \"message\": \"...\"}}"]

    EMIT --> EXIT1["process::exit(1)"]

    JSON_CHECK -->|no| MIETTE["miette renders\nformatted error to stderr\n(labels, help, source span)"]

    MIETTE --> EXIT2["propagate Err\n→ miette hook prints\n→ exit(1)"]
```

## Notes

- JSON errors go to **stdout** (not stderr) — consistent with all `--json` output, so callers read one stream.
- `code` maps from `#[diagnostic(code(...))]` on the error variant; `null` if no code is set.
- `message` is `error.to_string()` — short, no Miette decoration.
- Non-JSON path is completely unchanged: Miette renders to stderr as before.
- Implementation: replace `cmd.run()?` at `main.rs:65` with an explicit `match`.

## Key files

- `git-workon/src/main.rs` — implementation point
- `git-workon-lib/src/error.rs` — `#[diagnostic(code(...))]` attributes
- `docs/adr/021-structured-json-error-protocol.md` — design rationale
