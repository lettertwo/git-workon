# Plan — Review TUI Everyday-Usability Pass

Design locked 2026-07-07. Decisions live in **[ADR-034](../adr/034-review-git-native-config-schema.md)**
(config schema + keymap) and **[ADR-035](../adr/035-review-theming-base16-hybrid.md)**
(theming). This doc is the *execution* plan: what lands, in what order, how each unit is
verified. Read both ADRs before implementing — this plan does not restate their rationale.

Comments (now part of the agent-loop work) are deprioritized behind this pass. Goal: make the review TUI usable for
everyday review work — configurable keybindings, discoverable help, real theming.

## Scope (four tracks)

1. **Keybindings** — action registry (action → default keys, description, view); git-config
   loading of `workon.review.<view>.bind.<action>`; token-grammar parser; per-view
   resolution with validation + collision detection; **defaults unchanged** (decided:
   configurability + discoverability is the fix, not a keymap redesign).
2. **Help surface** — persistent curated per-view footer + `?` overlay (focused view + global
   bindings), new `toggle-help` action.
3. **Theming** — base16 `Theme` primitive; render-time color resolution (`FgSpan` carries a
   capture index, not a `Color`); hybrid boundary (on-tint = base16 truecolor, chrome =
   ANSI-named); derived diff tints; `workon.review.theme = auto|dark|light`;
   terminal-derivation OSC probe for `auto` with curated fallback; curated dark + light.
4. **View-config** — `workon.review.outline.width|mode`, `workon.review.diff.layout|zoom`
   read from config with current values as defaults.

## Changeset partition (Graphite stack)

Two independent tracks fan out from the shared config reader (the git-config reader), plus
view-config off it.
Each unit is land-alone (green + valuable on `main` by itself) and standalone-review.

```
main
 └─ uc-review-config           ── shared git-config reader
     ├─ uc-keymap              ── configurable per-view keymaps (keybinding track)
     │   └─ uc-help            ── footer + ? overlay
     ├─ uc-theme-base16        ── Theme primitive + render-time resolution (dark only, no visible change)
     │   └─ uc-theme-light     ── curated light + theme=dark|light
     │       └─ uc-theme-auto  ── terminal-derivation probe + theme=auto default
     └─ uc-view-config         ── outline.width/mode, diff.layout/zoom
```

Order of landing: the git-config reader → (configurable per-view keymaps → the help footer and
`?` overlay) and (the base16 palette primitive → the curated light scheme → the
terminal-derivation probe for `theme=auto`) and the view-config settings. The keymap and
theming subtrees are independent after the git-config reader; land in either interleaving.
Main-thread diff-read each
before the next lands (per the working style).

### The git-config reader (`ReviewConfig`)
- **Decision:** the review binary reads git config for the first time. Mirror
  `git-workon-lib/src/config.rs`'s `WorkonConfig` pattern: read via `repo.config()` (the
  `App` already owns a `Repository` — see `app.rs`). New module `git-workon-review/src/config.rs`.
- Provide typed getters for the keys this pass introduces (bindings, theme, view settings).
  Reuse git2 `Config::get_string`/`get_bool`/`get_i64`/`multivar` as `WorkonConfig` does.
- **No behavior change yet** — just the reader + tests against a fixture repo config.
- Verify: unit tests reading `workon.review.*` from a `FixtureBuilder` repo (both a set and
  an unset/default case). Load `/docs testing` first; use FixtureBuilder + predicates.

### Configurable per-view keymaps (action registry)
- **Decision:** ADR-034. Replace the hardcoded `map_key` match (`tui.rs`) with a
  registry-driven dispatch.
- Build the **action registry**: one table `action → (default keys, human description, view ∈
  {global, diff, outline})`. This is the single source of truth for defaults, validation,
  help text. The existing `Action` enum is the action set; extend, don't fork it.
- **Token-grammar parser** (ADR-034): reserved symbolic names (incl. `space`, `tab`, `enter`,
  `esc`, arrows, `backtab`, `f1`–`f12`), modifier prefixes (`ctrl-`/`alt-`/`shift-`), literal
  chars, chords (`]f`). Reserved-word-wins disambiguation.
- **Load + invert:** read every `workon.review.*.bind.*` var (via the git-config reader), split values into key
  tokens, build per-view `key → action` maps. A git entry overrides that action's default
  (native single-value precedence — no custom layering). Empty value = unbind.
- **Validation + collisions:** unknown `bind.<action>` → footer warning (action set is
  enumerable); a key claimed by two actions in one view → footer warning + deterministic
  winner. Defaults never collide.
- **Not rebindable, keep hardcoded:** confirm modal (`y`/`n`/`Esc`) and the whole `Esc`
  precedence cascade (`tui.rs` `update`). Do not route these through the registry.
- Verify: parser unit tests (each token class incl. `space`, a chord, an unbind, an unknown
  action, a collision); a dispatch test asserting a rebind takes effect. `map_key`'s existing
  behavior tests must still pass (defaults unchanged).

### The help footer and `?` overlay
- **Decision:** persistent curated per-view footer + `?` overlay targeting the focused view.
- **Footer:** always-visible one line of ~5–7 **hand-curated** keys for the focused
  context (diff vs outline), rendered from the resolved map + registry descriptions. Updates
  on focus/mode change. A transient notice **temporarily replaces** it (notices already clear
  on next keypress — `tui.rs` `update`), so no second line.
- **`?` overlay:** new global action `toggle-help` bound to `?`. Centered modal listing the
  **focused view's** bindings **+ global** bindings (what's live right now), grouped, from the
  resolved registry. Renders the *active* map so user rebinds show.
- Curation: pick the footer key set per view deliberately (this is the "feels learnable"
  lever). Diff: nav + stage/discard + outline + help. Outline: nav + open + mode + back.
- Verify: overlay renders resolved (rebound) keys; footer swaps with a notice and returns;
  instrument via a log-file + expect harness, NOT ratatui frame grepping (see the TUI-dogfood
  memory).

### The base16 palette primitive (`Theme` + render-time resolution)
- **Decision:** ADR-035. Largest mechanical unit; **behavior-preserving** (dark stays
  pixel-identical), so land-alone with no user-visible change.
- Introduce `struct Base16 { base00..base0F }` / `Theme`. Re-express the current `render.rs`
  `const` palette + `highlight.rs` accents as the **dark** base16 instance (the existing
  values ARE base08–0E + a ramp — see ADR-035). Derive diff tints via the existing
  `tint_toward` from base08/base0B toward base00.
- **Hybrid boundary:** on-tint colors resolve from `Theme` slots; chrome stays ANSI-named
  (`Color::Gray`/`DarkGray`/…) — several already are.
- **Mechanism:** change `FgSpan` to carry the **capture index** (not a `Color`);
  `highlight.rs:283` records `idx`; render resolves `theme.slot[idx]` alongside tint/cursor.
  `HIGHLIGHT_NAMES` stays const (index space). Thread `&Theme` into render.
- Verify: existing render/highlight tests that assert concrete colors now resolve through a
  fixed test `Theme` (dark) — same asserted colors. Full workspace green. This is the
  regression gate that the refactor changed nothing.

### The curated light scheme (`theme = dark|light`)
- Add the **light** base16 instance (paste a published base16 light scheme's 16 hexes — do
  NOT hand-invent; ADR-035). Wire `workon.review.theme` (via the git-config reader) to select dark/light;
  derived tints recompute for light automatically.
- Verify: `theme=light` selects the light instance; tints derive; a render test at light.

### The terminal-derivation probe for `theme=auto`
- **Decision:** ADR-035. The single most terminal-fragile unit — isolated on purpose.
- OSC probe on the controlling `/dev/tty` (TUI already renders there — `tui.rs`) at startup,
  raw mode, short timeout: `OSC 4;n;?` (n=0–15) + `OSC 10/11`. Populate slots from real RGB.
- **Synthesize the 6 slots ANSI lacks** (base01/02/04/06/09/0F) per ADR-035's rules.
- **Fallback chain:** per-slot fallback to curated; total failure → curated by bg luminance
  (if `OSC 11` answered) else `dark`. **Never hang** — timeout is the backstop. Make `auto`
  the default `theme` value.
- Verify: probe parses a synthetic OSC reply into slots; timeout path falls back to curated
  (no hang) — drive with a fake tty/reader, do not depend on the test terminal answering.

### The view-config settings
- Read `workon.review.outline.width|mode` and `workon.review.diff.layout|zoom` (via the
  git-config reader), current hardcoded values as defaults. `outline.width` also addresses
  the stack-and-outline work's deferred narrow-terminal papercut.
- Verify: each setting overrides its default from a fixture config; unset = current default.

## Cross-cutting notes / gotchas

- **Testing:** load `/docs testing` before writing any test; FixtureBuilder + custom
  predicates, extend predicates before tests. Pin color off in output-asserting tests
  (`NO_COLOR`/`no_color()`) — the user env sets `FORCE_COLOR=3` (see memory).
- **TUI verification:** instrument via log-file + `expect`, never grep ratatui frames (memory).
- **Errors:** any new error types follow ADR-008 (concrete enums, `#[derive(Error, Diagnostic)]`);
  load `/docs errors` first.
- **Commit style:** Conventional Commits, single line, scope `review`. No body/footer.
- **Gate before landing each CS:** `cargo test --workspace` + clippy
  `-D warnings --all-targets --all-features`, then main-thread diff-read.

## Deferred (explicitly not this pass)
- `theme = <named>` / user-supplied base16 scheme files (the "user-configurable colors" tier).
  Additive later — the slot *source* is pluggable behind render-time resolution (ADR-035).
- Post-subcommand completion delegation (the CLI-integration work's note), git-inference
  stack model, ref-range sources (the stack-and-outline work), and all of the agent-loop
  work onward.
