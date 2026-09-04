# 034 — Review TUI Config: Git-Native Per-View Namespaces

## Context

The review TUI (`git-workon-review`) grew its keybindings and colors as hardcoded
values during M3–M5: a `match` in `tui.rs` for keys, a block of `const … Color::Rgb(…)`
atop `render.rs` for theming. Making either user-configurable needs a config home, and
the review binary reads no config today (`struct Cli {}` is empty).

[ADR-006](006-git-native-config.md) already commits the tool to git-native config under
the `workon.*` namespace — no bespoke file format, git's layered precedence
(local → global → system), multivar for lists. The open question was whether a *keymap*
fits that model, since a keymap is many key→action entries. Three shapes were considered:

1. A dedicated `review.toml` (nested keymap syntax, in-tree shareable) — but a second
   config system, against ADR-006's one-config-system principle, needs a new loader and
   precedence layer.
2. Value-side multivar `workon.review.bind = "key=action"` — git-native, but multivar
   *accumulates* across layers, forcing us to reimplement override precedence and
   last-wins dedup by hand and invent an unbind sentinel.
3. Action-as-key, per-view namespaces (chosen).

The keymap is also context-dependent: the same key differs by view (`j` is cursor-down in
the diff pane, outline-move-down in the outline), and some bindings are two-key chords
(`]f`). Whitespace and special keys (Tab, Enter, Esc, arrows, **space**) have no safe
literal form.

## Decision

All review config lives under `workon.review.*` in git config, extending ADR-006. Config
is stored **action-as-key** in **per-view subsections**:

```
workon.review.theme                    = dark            ; global, non-view
workon.review.theme.<slot>             = #rrggbb          ; base00-base0f override (CS1)
workon.review.theme.<tint>             = #rrggbb          ; diff/cursor tint override (CS1)
workon.review.<view>.bind.<action>     = "<key tokens>"  ; a keymap entry
workon.review.<view>.<setting>         = <value>         ; view config
```

- **View** ∈ `diff`, `outline`; a bare `workon.review.bind.<action>` is the **global**
  keymap (active in every view). Git parses `workon.review.diff.bind.stage-hunk` as
  section `workon`, subsection `review.diff.bind`, name `stage-hunk` — dotted subsections
  are legal and case-sensitive (always lowercase here).
- **The action is the config variable; the keys are the value.** Each binding is therefore
  an ordinary *single-valued* variable, so git's native precedence does all override work:
  setting it replaces (local beats global beats system via `config.get_string()`), and an
  empty value unbinds. No custom layering, no sentinel. Defaults live in code; a git entry
  overrides that action's default. Action names qualify as git variable names (alphanumeric
  + `-`, alpha-initial): `stage-hunk`, `next-file`, `toggle-outline`, …
- **Value = space-separated key tokens** (an action may have several keys, e.g.
  `cursor-down = "j down"`). Replace, not append: setting a binding states exactly what
  triggers it. Token grammar:
  - **Reserved symbolic names (win over literals):** `space tab enter esc up down left
    right home end pageup pagedown backspace delete backtab f1`–`f12`.
  - **Modifier prefix:** `ctrl-`, `alt-`, `shift-` on any token (`ctrl-d`, `ctrl-space`).
  - **Literal:** otherwise printable chars — length 1 is one key (`s`, `=`), length >1 is a
    chord (`]f`). A token is matched against reserved words and the modifier grammar first,
    literal only if neither matches, so `space` is always the spacebar.
- **View config** (non-binding) shares the view namespace: `workon.review.outline.width`,
  `workon.review.outline.mode`, `workon.review.diff.layout`, `workon.review.diff.zoom`.
  The `.bind.` marker is what distinguishes a keymap entry from a view setting.
- **Theme overrides** (CS1, user-configurable colors tier — see
  [ADR-035](035-review-theming-base16-hybrid.md)'s CS1 revision) live in the `review.theme`
  subsection, distinct from the top-level `workon.review.theme` selection itself: `workon.review
  .theme.base00`–`workon.review.theme.base0f` (base16 slot overrides) and eleven kebab-case tint
  keys (`workon.review.theme.cursor-bg`, …). Same validation posture as an unknown bind
  action — an unrecognized key or malformed `#rrggbb` value is a startup warning, not an error.
- **Load-time inversion:** on startup, walk every `workon.review.*.bind.*` variable, split
  values into key tokens, and build the per-view key→action dispatch maps. This pass
  validates (unknown `bind.<action>` → warning; the action set is enumerable) and detects
  collisions (one key claimed by two actions in a view → footer warning + deterministic
  winner; defaults never collide, so this only fires on user config).
- **Not rebindable:** the confirm modal (`y`/`n`/`Esc`) and the whole `Esc` precedence
  cascade (confirm > outline-unfocus > selection-cancel > quit) stay hardcoded — they are
  conventional, safety-sensitive, and the Esc cascade's documented precedence would break
  if rebound.
- **`reload-config` (`R`, global view, rebindable like any other action):** re-reads the
  whole `workon.review.*` tree and swaps it in without restarting — this ADR's schema was
  originally "read once at startup"; live reload makes it "read once, re-readable on
  demand" instead, with no schema change (the same getters just run again). One exception:
  `theme = auto`'s terminal-derivation probe (ADR-035) never re-runs mid-session — it needs
  the tty, which the TUI owns once the alternate screen is live, and a second probe
  conversation there would corrupt input. Reload caches the startup probe result and reuses
  it whenever the resolved theme is `auto`, so switching `theme` to `dark`/`light` takes
  effect on reload, but switching back to `auto` reuses the cached base rather than
  re-probing.

## Consequences

- One config system across the whole tool; users already know `git config`. Global
  preferences in `~/.gitconfig`, per-repo in `.git/config`, standard layering — inherited
  from ADR-006 for free.
- Override and unbind require **no resolver logic** — they are native git-config semantics.
  This is the primary reason action-as-key beat value-side `key=action`.
- Key names and action names become a **compatibility surface**: once users write
  `workon.review.diff.bind.stage-hunk`, renaming that action or restructuring the namespace
  breaks their config. Action names are therefore part of the stable API, and the help
  overlay renders from the same enumerable action set.
- Like all git-native config (ADR-006), review config is **not checked into the repo**, so a
  team cannot ship a shared review keymap/theme in-tree. Accepted: this is a
  personal-productivity TUI.
- The per-view namespace gives previously-hardcoded view settings (outline width — M5
  deferred narrow-terminal handling — outline mode, diff layout/zoom defaults) a natural
  home without a second design pass.
- Adding a rebindable action = adding it to the enumerable action set (code default +
  dispatch + help entry); it is automatically configurable, validated, and documented.

## Revised (config validation completeness)

The validation posture above ("an unrecognized key … is a startup warning, not an error") turned
out to hold in only two of the four places it reads as a promise. `workon.review.theme.*` warns on
an unrecognized key, and the bind pass warns on an unknown action — but every *other* key under
`workon.review.*` is read by an explicit getter, so a name no getter asks for is never seen by
anything. A typo'd `workon.review.diff.laoyut` or `workon.review.outline.wdith` is silently
dropped: no warning, no effect, and nothing to distinguish it from a setting that simply had no
visible result. This bit in practice, twice in one session, on two different subsections.

**Unknown-key detection now covers the whole `workon.review.*` tree**, via a single validation pass
over `entries("workon.review.*")` driven by a central known-key registry: exact scalar names, plus
pattern arms for the two open-ended subspaces (`theme.<slot|tint>`, `<view>.bind.<action>`). Any
name no arm claims warns and is ignored, same non-fatal posture as everything else here.

Scope stops at `workon.review.*` deliberately. That subsection is this crate's exclusively;
`workon.*` at large belongs to `git-workon-lib`, and scanning wider would warn about
`workon.autocopy` and every other key this crate has no business knowing.

**The registry is a second source of truth, and that is the real cost.** A getter added without a
matching registry entry would make its key warn as unknown *while working correctly* — worse than
the silent-drop it replaces. The mitigation is a drift test that enumerates the getters' keys and
asserts each is claimed by the registry, so the failure lands in CI rather than in a user's footer.
The alternative — threading consumed-key tracking through every getter so the getters *are* the
registry — removes the drift class outright but reworks every reader's signature or call site; the
registry-plus-test was judged the better trade at this schema's size, and the choice is revisitable
if the schema grows a third open-ended subspace.

**Invalid-value warnings now carry the allowed set and the fallback being applied.** The existing
messages named the offending value but neither what was legal nor what the reader did instead —
`"workon.review.diff.text = 'edt' unrecognized; using default"` leaves a user to go read source or
docs for both halves. They now read `(valid: syntax, tint, edit); using default 'syntax'`, and the
range-checked and color-format cases get the same treatment. Theme keys keep saying `ignoring`
rather than naming a default, because an ignored override genuinely has no default to apply — the
underlying scheme's value stands.

**Unknown keys suggest a nearest match** by edit distance against the registry when one is close
enough, since the overwhelmingly common cause of an unknown key is a typo of a real one.

## References

- [ADR-006](006-git-native-config.md) — git-native config under `workon.*` this extends
- `docs/rfc/workon-review.md` — RFC; this is the everyday-usability pass inserted ahead of M7
- `git-workon-review/src/tui.rs` — current hardcoded keymap (`map_key`) being replaced
- `git-workon-review/src/render.rs` — current hardcoded palette (`const … Color::Rgb`) — see the theming decision
