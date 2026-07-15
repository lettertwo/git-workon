# 029 — Review TUI Theming: Hybrid base16, Render-Time Resolution, Terminal-Derived `auto`

## Context

The review TUI's colors were hardcoded during M3–M5: a `const … Color::Rgb(…)` block
atop `render.rs` (dark-only) and a parallel `HIGHLIGHT_NAMES`/`HIGHLIGHT_COLORS` pair in
`highlight.rs`. The everyday-usability pass (ahead of M7, see [ADR-028](028-review-git-native-config-schema.md))
adds built-in light/dark theming and terminal adaptivity. Four things had to be resolved:
the color *philosophy* (respect the terminal's 16 ANSI colors vs. ship tuned truecolor),
the theme *primitive*, the *mechanism* by which a theme reaches syntax highlighting, and
what "adapt to the terminal" concretely means.

Key constraint: diff readability depends on a **truecolor gradient** — `BG_*_SUBTLE` vs
`BG_*_STRONG` and their staged variants sit a few RGB shades apart, and that gradient is
how word-level emphasis and staged-vs-unstaged attribution read at a glance. The 16-color
ANSI palette has no equivalent, so pure "inherit the terminal's ANSI colors" (which would
self-adapt for free) was rejected — it regresses the readability that is the tool's point.

A second discovery shaped the primitive: `highlight.rs` is *already* a base16 template.
Its comment says the palette is "in the same family as base16-eighties.dark," and the
`C_RED/ORANGE/YELLOW/GREEN/CYAN/BLUE/PURPLE` consts are base08–base0E, mapped to captures
per the base16 spec's role conventions (`keyword → base0E`, `string → base0B`,
`function → base0D`, `comment → base03`, …). The capture→slot template already exists and
is spec-conformant.

## Decision

**Philosophy — hybrid, split on "does this color sit on a tinted background?"**
- **On a tint → base16 truecolor (theme-controlled):** diff add/del subtle/strong + staged
  variants, cursor, selection, and **syntax**. Contrast is guaranteed because foreground and
  background come from the *same* scheme.
- **Chrome (default text, dim labels, gutter/dividers) + the canvas background →
  base16-ramp-controlled (revised post-CS6):** originally these were ANSI-named
  (`Color::Gray`/`DarkGray`) and the canvas was never painted, on the theory that inheriting
  the terminal's own bg/fg would self-adapt for free. In practice this broke explicit
  `light`/`dark` selections outright — the terminal's own (often dark) bg/fg bled straight
  through a "light" theme, since nothing ever painted over it. Fixed: `Palette::background`
  (base00)/`foreground` (base05)/`dim` (base03)/`gutter` (base04) are now real palette
  fields, and `render()` paints the whole frame with `background` first when
  `Palette::paint_canvas` is set. `dark()`/`light()` set `paint_canvas: true` — a curated
  theme now fully controls the look, canvas included. `from_terminal` (`auto`) still derives
  these four straight from the probed terminal colors — so it matches the terminal exactly,
  as before — but sets `paint_canvas: false`, since `auto`'s base00 *is* the terminal's own
  background; painting over it would flatten terminal transparency/background images for no
  gain. The probe-failure fallback (`dark()`/`light()`) paints normally. Chrome that is
  never a theme knob (error/warn/current-marker) stays ANSI/const in `render.rs`, unchanged.

**Primitive — the theme is a base16 scheme.** A `Palette` holds the 16 slots
(base00–07 mono ramp + base08–0F accents). Syntax uses the accents via the existing
capture→slot template.

Diff-bg tints ideally come from base08 (red / spec "Diff Deleted") and base0B (green / spec
"Diff Inserted") and the scheme background, so syntax and tints stay coordinated. **But the
derivation is luminance-dependent, not a single "blend toward base00" (corrected in CS4):**
- **Dark (base00 dark):** the shipped M3–M5 tints are more saturated/darker than *any* convex
  blend of an accent toward a dark base00 can produce (their green/blue channels sit *below*
  base00's). A blend toward a dark base00 also yields muddy mid-tones, not punchy washes. So
  the **dark tints are held explicit** in `Palette::dark()` (byte-identical to M3–M5, per the
  pixel-identity gate). Deriving them would require scaling the accent toward *black* plus a
  desaturation step, not a base00 blend — not worth reverse-engineering the hand-tuned values.
- **Light (base00 light) and terminal-derived:** blending an accent toward a *light* base00
  gives the correct pale tint, so the `tint_toward` derivation applies there (CS5/CS6). A
  terminal-derived theme on a *dark* background hits the same problem as dark and needs the
  toward-black+desaturate construction — a CS6 concern.

Net: the scheme-coordinated derivation is real but must branch on background luminance; dark
stays authored.

**Mechanism — resolve color at render time, not in the highlight phase.**
- `HIGHLIGHT_NAMES` stays global/const: it defines the capture *index space* bound by
  `config.configure()` and is theme-invariant.
- `FgSpan` carries the **capture index** (semantic role), not a resolved `Color`. The
  highlight phase (`highlight.rs:283`) records the index instead of looking up a color.
- Render resolves `index → Color` against the active `Palette` (`palette.slot[idx]`), in the
  same place it resolves diff tints and cursor/selection. One theme-application site;
  syntax and background contrast are reasoned about together.
- Consequence: the expensive tree-sitter pass is theme-free and cacheable — a theme switch
  recolors by re-rendering, without re-parsing.

**Selection — `workon.review.theme = auto | dark | light`** (git config, per ADR-028;
`auto` is the default).
- **`auto` = terminal-derived.** Probe the terminal for its palette (`OSC 4;n;?` for
  n=0–15, `OSC 10/11` for fg/bg), populate the 16 slots from the real RGB, and derive tints
  from the probed base00/08/0B. `auto` *means* terminal-derivation and nothing else — it is
  not a placeholder for a curated pick (an earlier `COLORFGBG`-picks-curated design was
  rejected precisely because it would change `auto`'s meaning once the probe landed).
- **`dark` / `light` = curated base16 schemes** — explicit overrides and the probe-failure
  fallback. `dark` is the current eighties.dark values; `light` is a published base16 light
  scheme's 16 hexes (pasted, not hand-invented).

**Terminal derivation specifics.**
- ANSI-16 cannot fill 6 base16 slots (base01, base02, base04, base06, base09, base0F), so
  those are **synthesized**: ramp intermediates by interpolation (base01/02 from base00→03,
  base04/06 from base03→05→07), base09 (orange) by blending base08+base0A, base0F from
  base09/base08. The diff-critical slots (base00/08/0B) are always real, so tint quality is
  preserved; the loss is secondary accents.
- The probe runs at startup on the controlling `/dev/tty` (the TUI already renders there —
  see `tui.rs`), in raw mode, reading replies with a short timeout. **Failure degrades
  gracefully:** per-slot fallback to the curated scheme's slot; total failure falls back to
  the curated scheme chosen by background luminance if `OSC 11` answered, else `dark`.
  tmux/screen/ssh non-response is handled by the timeout, never a hang.

**CS6 refinement — the diff-bg tints stay curated, only the scheme is derived.** In
implementation, `auto` derives the base16 **scheme** (the 16 slots → syntax + monochrome ramp)
from the terminal, but the **diff/cursor/selection tints stay curated by luminance** rather than
derived from the probed accents (`Palette::from_terminal`: syntax = `SYNTAX_SLOTS` over the probed
`Base16`; tints = `Palette::dark()`'s or `Palette::light()`'s tint fields, chosen by the luminance
of the probed `base00`). Two reasons the earlier "derive tints from `base08`/`base0B`" plan was
narrowed: (1) dark-tint derivation is unsolved (see the corrected Primitive section — a convex
blend toward a dark `base00` can't reproduce the hand-tuned washes, and a probed *dark* terminal
hits exactly that), and (2) deriving washes from an arbitrary terminal's accent is unpredictable
across the range of real terminal palettes. The value of `auto` — **code colors matching the
terminal** — is fully delivered by the probed syntax slots, which curated tints don't compromise;
the diff washes were already hand-tuned per luminance, so borrowing them loses nothing. The six
ANSI-less slots are still synthesized as above; `parse` → `build_base16` → `from_terminal` → the
`palette_for_auto` fallback decision are all pure and unit-tested, with only the timed `/dev/tty`
read left untested (see `terminal_query.rs`).

## Consequences

- Light/dark ships as curated base16 schemes now; **terminal-derivation is first-class from
  the start**, not deferred. `auto` never has to change meaning later.
- Because color resolves late as `palette.slot[idx]`, the slot *source* is pluggable — a future
  user-supplied base16 scheme (`theme = <name>` / a scheme file, the deferred
  "user-configurable colors" tier) is additive, no renderer change.
- The OSC probe is the single most terminal-fragile component; its blast radius is contained
  by the timeout + curated fallback, so a hostile terminal yields a correct curated theme,
  never a hang or a broken palette.
- Adding a syntax capture = adding it to `HIGHLIGHT_NAMES` + the capture→slot template; it is
  automatically themed by every scheme.
- `render.rs` and `highlight.rs` both change: the `const` palette becomes a `Palette` threaded
  to render; `FgSpan` loses its `Color` field in favor of a capture index. Existing render
  tests that assert concrete colors must resolve through a fixed test `Palette`.

## Revised (CS2, visual-polish pass)

The "chrome that is never a theme knob (error/warn/current-marker) stays ANSI/const in
`render.rs`" clause above is superseded. Those three colors are now `Palette` fields
(`error_fg`/`warn_fg`/`current_fg`, mapped to base08/base0A/base0B) rather than module
consts — the user explicitly approved revisiting this boundary during the icons/semantic-fg
polish pass. `dark()` keeps the shipped RGB values verbatim (the same pixel-identity
precedent the diff/cursor tints follow); `light()` takes `ONE_LIGHT`'s base08/base0A/base0B;
`from_terminal()` takes the probed scheme's base08/base0A/base0B directly, same reasoning as
the syntax slots (matching the terminal, not curated-tint-borrowing). No other part of the
hybrid boundary changes: this only moves three named colors from `const` to palette fields.

## Revised (CS1, user-configurable colors tier)

The "user-supplied base16 scheme … the deferred 'user-configurable colors' tier" noted in
Consequences above lands, narrower than originally sketched: **per-slot and per-tint git-config
override keys**, not named bundled schemes. `workon.review.theme.*` (a subsection distinct from
`workon.review.theme` itself — both coexist, since git parses `[workon "review"] theme = …` and
`[workon "review.theme"] base00 = …` as different subsections) accepts:

| Key | Meaning | Palette field(s) rewritten |
| --- | --- | --- |
| `base00`–`base0f` (lowercase) | base16 slot override | role-mapped field(s) below, plus every `syntax` entry whose capture→slot template maps to that slot |
| `base00` | canvas background | `background` (and sets `paint_canvas: true`) |
| `base03` | dim/comment ramp step | `dim` |
| `base04` | gutter/divider ramp step | `gutter` |
| `base05` | default text | `foreground` |
| `base08` | red accent | `error_fg` |
| `base09` | orange accent | `modified_fg` |
| `base0a` | yellow accent | `warn_fg` |
| `base0b` | green accent | `current_fg` |
| `base0c` | cyan accent | `heading_fg` |
| `del-subtle`, `del-strong`, `add-subtle`, `add-strong`, `del-staged-subtle`, `del-staged-strong`, `add-staged-subtle`, `add-staged-strong`, `cursor-bg`, `selection-bg`, `outline-cursor-unfocused-bg` | diff/cursor tint override (kebab-case, mirroring the `Palette` field names) | the matching field, verbatim |

Values are `#rrggbb` or bare `rrggbb` (six hex digits only — no 3-digit shorthand). Applied via
`Palette::apply_overrides`, on top of whichever base (`dark`/`light`/`auto`'s probe) was already
resolved — the mechanism is base-agnostic, so an override key works identically regardless of
`workon.review.theme`'s selection. **Uniform slot rule:** a slot override rewrites its
role-mapped field(s) even when the current base hand-authored that field explicitly (e.g.
`base08` under `theme = dark` replaces `dark()`'s hand-tuned `error_fg`) — the alternative
(silently ignoring slot overrides for authored fields) is a UX trap: a user who sets `base08`
expects red to change. Slot overrides do NOT re-derive the diff/cursor tints; that stays the 11
tint keys' job, applied last and verbatim, so a slot override can't reshape a hand-tuned wash it
wasn't asked to touch. An invalid value or an unrecognized key under `workon.review.theme.*` is
ignored with a startup warning (the same posture as ADR-028's keybinding validation) — not a
hard error.

**Named bundled schemes explicitly deferred.** `theme = <scheme-name>` selecting a whole
vendored base16 scheme (e.g. from tinted-theming/schemes, MIT-licensed and so licensing-clean
to vendor) was considered and set aside — the override-key tier covers the immediate need, and
named schemes slot in additively later (a `Theme::Named` variant + a `schemes.rs` of vendored
constants) without touching this work if demand appears.

**NO_COLOR (CS2).** The other extra this tier's Context section named — `NO_COLOR`, no CLI flag,
no color-depth downgrade — lands as `Palette::mono(light: bool)`: every fg field, every syntax
entry, and the canvas background collapse to `Color::Reset` (`paint_canvas: false`), while the
11 diff/cursor washes become achromatic grayscale `Rgb` ladders (dark-terminal vs light-terminal
picked by `light`) rather than also going `Reset`, since `render.rs` has no non-color channel
(reverse/dim) to substitute for them and changing `render.rs` was out of scope. Add and Del share
one ladder — colorless mode can't carry that distinction by hue, so it falls to gutter
glyph/structure instead, an accepted degradation. `main.rs` applies this last, after theme
resolution AND override application (`NO_COLOR` is an env kill-switch that wins over any
`workon.review.theme.*` override), when `NO_COLOR` is set to any non-empty value (`no-color.org`);
`FORCE_COLOR` is deliberately not consulted — it answers a different question (this repo's own
test/output-capture posture), not "does this user want THIS tool's colors." One wrinkle,
found by driving the real binary under a PTY: crossterm ALSO honors `NO_COLOR`, by stripping
every color SGR at the output layer — which would erase the grayscale washes too and leave
cursor/selection/staged attribution invisible. The app owns `NO_COLOR` semantics at the palette
level instead, so the mono branch calls `crossterm::style::force_color_output(true)` to disable
that blanket suppression and let the achromatic ladders through.

## References

- [ADR-028](028-review-git-native-config-schema.md) — `workon.review.theme` config key
- [ADR-006](006-git-native-config.md) — git-native config this builds on
- `git-workon-review/src/highlight.rs` — existing base16-conformant capture→slot template
- `git-workon-review/src/render.rs` — `const` palette + `tint_toward` blend helper being generalized
- base16 styling spec — slot role conventions (base08 red/Diff-Deleted, base0B green/Diff-Inserted, base0E keywords, …)
