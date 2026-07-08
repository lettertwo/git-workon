# 028 — Review TUI Config: Git-Native Per-View Namespaces

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
- **Load-time inversion:** on startup, walk every `workon.review.*.bind.*` variable, split
  values into key tokens, and build the per-view key→action dispatch maps. This pass
  validates (unknown `bind.<action>` → warning; the action set is enumerable) and detects
  collisions (one key claimed by two actions in a view → footer warning + deterministic
  winner; defaults never collide, so this only fires on user config).
- **Not rebindable:** the confirm modal (`y`/`n`/`Esc`) and the whole `Esc` precedence
  cascade (confirm > outline-unfocus > selection-cancel > quit) stay hardcoded — they are
  conventional, safety-sensitive, and the Esc cascade's documented precedence would break
  if rebound.

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

## References

- [ADR-006](006-git-native-config.md) — git-native config under `workon.*` this extends
- `docs/rfc/workon-review.md` — RFC; this is the everyday-usability pass inserted ahead of M7
- `git-workon-review/src/tui.rs` — current hardcoded keymap (`map_key`) being replaced
- `git-workon-review/src/render.rs` — current hardcoded palette (`const … Color::Rgb`) — see the theming decision
