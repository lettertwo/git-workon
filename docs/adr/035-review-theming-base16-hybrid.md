# 035 — Review TUI Theming: Hybrid base16, Render-Time Resolution, Terminal-Derived `auto`

## Context

The review TUI's colors were hardcoded during M3–M5: a `const … Color::Rgb(…)` block
atop `render.rs` (dark-only) and a parallel `HIGHLIGHT_NAMES`/`HIGHLIGHT_COLORS` pair in
`highlight.rs`. The everyday-usability pass (ahead of M7, see [ADR-034](034-review-git-native-config-schema.md))
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
- **Chrome, not on a tint → ANSI-named (`Color::Gray`/`DarkGray`/…):** gutter, borders,
  footer, dim labels, status. These inherit the terminal palette, self-adapt light/dark, and
  are **probe-independent** (work even when terminal-derivation fails). Half already are
  ANSI-named today.

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

**Selection — `workon.review.theme = auto | dark | light`** (git config, per ADR-034;
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

## References

- [ADR-034](034-review-git-native-config-schema.md) — `workon.review.theme` config key
- [ADR-006](006-git-native-config.md) — git-native config this builds on
- `git-workon-review/src/highlight.rs` — existing base16-conformant capture→slot template
- `git-workon-review/src/render.rs` — `const` palette + `tint_toward` blend helper being generalized
- base16 styling spec — slot role conventions (base08 red/Diff-Deleted, base0B green/Diff-Inserted, base0E keywords, …)
