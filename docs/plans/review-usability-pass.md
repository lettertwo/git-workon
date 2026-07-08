# Plan — Review TUI Everyday-Usability Pass (M6.5)

Design locked 2026-07-07. Decisions live in **[ADR-034](../adr/034-review-git-native-config-schema.md)**
(config schema + keymap) and **[ADR-035](../adr/035-review-theming-base16-hybrid.md)**
(theming). This doc is the *execution* plan: what lands, in what order, how each unit is
verified. Read both ADRs before implementing — this plan does not restate their rationale.

Comments (M7) are deprioritized behind this pass. Goal: make the review TUI usable for
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

Two independent tracks fan out from the shared config reader (CS1), plus view-config off CS1.
Each unit is land-alone (green + valuable on `main` by itself) and standalone-review.

```
main
 └─ uc-review-config           CS1  ── shared git-config reader
     ├─ uc-keymap              CS2  ── configurable per-view keymaps (keybinding track)
     │   └─ uc-help            CS3  ── footer + ? overlay
     ├─ uc-theme-base16        CS4  ── Theme primitive + render-time resolution (dark only, no visible change)
     │   └─ uc-theme-light     CS5  ── curated light + theme=dark|light
     │       └─ uc-theme-auto  CS6  ── terminal-derivation probe + theme=auto default
     └─ uc-view-config         CS7  ── outline.width/mode, diff.layout/zoom
```

Order of landing: CS1 → (CS2 → CS3) and (CS4 → CS5 → CS6) and CS7. The keymap and theming
subtrees are independent after CS1; land in either interleaving. Main-thread diff-read each
before the next lands (per the working style).

### CS1 — `ReviewConfig` reader
- **Decision:** the review binary reads git config for the first time. Mirror
  `git-workon-lib/src/config.rs`'s `WorkonConfig` pattern: read via `repo.config()` (the
  `App` already owns a `Repository` — see `app.rs`). New module `git-workon-review/src/config.rs`.
- Provide typed getters for the keys this pass introduces (bindings, theme, view settings).
  Reuse git2 `Config::get_string`/`get_bool`/`get_i64`/`multivar` as `WorkonConfig` does.
- **No behavior change yet** — just the reader + tests against a fixture repo config.
- Verify: unit tests reading `workon.review.*` from a `FixtureBuilder` repo (both a set and
  an unset/default case). Load `/docs testing` first; use FixtureBuilder + predicates.

### CS2 — Action registry + configurable keymaps
- **Decision:** ADR-034. Replace the hardcoded `map_key` match (`tui.rs`) with a
  registry-driven dispatch.
- Build the **action registry**: one table `action → (default keys, human description, view ∈
  {global, diff, outline})`. This is the single source of truth for defaults, validation,
  help text. The existing `Action` enum is the action set; extend, don't fork it.
- **Token-grammar parser** (ADR-034): reserved symbolic names (incl. `space`, `tab`, `enter`,
  `esc`, arrows, `backtab`, `f1`–`f12`), modifier prefixes (`ctrl-`/`alt-`/`shift-`), literal
  chars, chords (`]f`). Reserved-word-wins disambiguation.
- **Load + invert:** read every `workon.review.*.bind.*` var (via CS1), split values into key
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

### CS3 — Help surface
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

### CS4 — base16 `Theme` primitive + render-time resolution
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

### CS5 — Curated light scheme + `theme = dark|light`
- Add the **light** base16 instance (paste a published base16 light scheme's 16 hexes — do
  NOT hand-invent; ADR-035). Wire `workon.review.theme` (via CS1) to select dark/light;
  derived tints recompute for light automatically.
- Verify: `theme=light` selects the light instance; tints derive; a render test at light.

### CS6 — `theme = auto` terminal-derivation probe
- **Decision:** ADR-035. The single most terminal-fragile unit — isolated on purpose.
- OSC probe on the controlling `/dev/tty` (TUI already renders there — `tui.rs`) at startup,
  raw mode, short timeout: `OSC 4;n;?` (n=0–15) + `OSC 10/11`. Populate slots from real RGB.
- **Synthesize the 6 slots ANSI lacks** (base01/02/04/06/09/0F) per ADR-035's rules.
- **Fallback chain:** per-slot fallback to curated; total failure → curated by bg luminance
  (if `OSC 11` answered) else `dark`. **Never hang** — timeout is the backstop. Make `auto`
  the default `theme` value.
- Verify: probe parses a synthetic OSC reply into slots; timeout path falls back to curated
  (no hang) — drive with a fake tty/reader, do not depend on the test terminal answering.

### CS7 — View-config settings
- Read `workon.review.outline.width|mode` and `workon.review.diff.layout|zoom` (via CS1),
  current hardcoded values as defaults. `outline.width` also addresses M5's deferred
  narrow-terminal papercut.
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
- Post-subcommand completion delegation (M6 note), git-inference stack model, ref-range
  sources (M5), and all of M7 comments onward.
