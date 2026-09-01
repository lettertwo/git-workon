# 035 — Review TUI Theming: Hybrid base16, Render-Time Resolution, Terminal-Derived `auto`

## Context

The review TUI's colors were hardcoded during the initial-renderer-through-stack-and-outline work: a `const … Color::Rgb(…)` block
atop `render.rs` (dark-only) and a parallel `HIGHLIGHT_NAMES`/`HIGHLIGHT_COLORS` pair in
`highlight.rs`. The everyday-usability pass (ahead of the source-selector work, see [ADR-034](034-review-git-native-config-schema.md))
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
  base16-ramp-controlled (revised after the terminal-derivation-probe work):** originally these were ANSI-named
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
derivation is luminance-dependent, not a single "blend toward base00" (corrected in the
base16-palette-primitive work):**
- **Dark (base00 dark):** the shipped initial-renderer-through-stack-and-outline tints are more saturated/darker than *any* convex
  blend of an accent toward a dark base00 can produce (their green/blue channels sit *below*
  base00's). A blend toward a dark base00 also yields muddy mid-tones, not punchy washes. So
  the **dark tints are held explicit** in `Palette::dark()` (byte-identical to the
  initial-renderer-through-stack-and-outline values, per the
  pixel-identity gate). Deriving them would require scaling the accent toward *black* plus a
  desaturation step, not a base00 blend — not worth reverse-engineering the hand-tuned values.
- **Light (base00 light) and terminal-derived:** blending an accent toward a *light* base00
  gives the correct pale tint, so the `tint_toward` derivation applies there (the
  curated-light-scheme and terminal-derivation-probe work). A
  terminal-derived theme on a *dark* background hits the same problem as dark and needs the
  toward-black+desaturate construction — a terminal-derivation-probe concern.

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

**Terminal-derivation-probe refinement — the diff-bg tints stay curated, only the scheme is derived.** In
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

**Derived-washes addendum (2026-07-20) — `auto`'s diff washes derive from the probed accents
after all; cursor/selection washes stay curated.** The terminal-derivation-probe refinement above is partially
reversed. Its objection (1) — "a convex blend toward a dark base00 can't reproduce the
hand-tuned washes" — turned out to answer the wrong question: the goal isn't to reproduce the
curated washes from probed inputs, it's to produce the washes the terminal's *theme author*
would have picked. Dogfooding `auto` against laserwave showed the curated washes as the one
discordant element (generic red/green under a personalized syntax palette), and laserwave itself
computes its editor diff backgrounds as `accent:mix(bg, 90)` — exactly the
`tint_toward(accent, bg, k)` shape. `Palette::from_terminal` now derives del washes from probed
base08 and add washes from probed base0B toward the probed base00: a dark probed background uses
the dogfood-validated ratios (subtle 0.90, strong 0.75, staged 0.94/0.85 — staged still reads
dimmer, per the staging-verbs staged-attribution decision), a light one reuses `Palette::light`'s hand-tuned ratio set.
Objection (2) — arbitrary-palette unpredictability — is accepted residual risk, bounded by the
`workon.review.theme.*` override tier (a wash that derives badly on some exotic palette is
pinnable per-user). Cursor/selection/unfocused washes keep borrowing the curated set: they have
no ANSI counterpart to derive from, and deriving them from probed base0D/base0C produces
surprises (a teal cursor row on an aqua-leaning theme), so that judgment stays curated-or-overridden.

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

## Revised (promoting semantic foregrounds to palette knobs, visual-polish pass)

The "chrome that is never a theme knob (error/warn/current-marker) stays ANSI/const in
`render.rs`" clause above is superseded. Those three colors are now `Palette` fields
(`error_fg`/`warn_fg`/`current_fg`, mapped to base08/base0A/base0B) rather than module
consts — the user explicitly approved revisiting this boundary during the icons/semantic-fg
polish pass. `dark()` keeps the shipped RGB values verbatim (the same pixel-identity
precedent the diff/cursor tints follow); `light()` takes `ONE_LIGHT`'s base08/base0A/base0B;
`from_terminal()` takes the probed scheme's base08/base0A/base0B directly, same reasoning as
the syntax slots (matching the terminal, not curated-tint-borrowing). No other part of the
hybrid boundary changes: this only moves three named colors from `const` to palette fields.

## Revised (user-configurable color-override keys)

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
| `del-subtle`, `del-strong`, `add-subtle`, `add-strong`, `del-staged-subtle`, `del-staged-strong`, `add-staged-subtle`, `add-staged-strong`, `cursor-bg`, `selection-bg`, `cursor-unfocused-bg`, `pane-header-focused-fg` | diff/cursor tint override (kebab-case, mirroring the `Palette` field names) | the matching field, verbatim |

Values are `#rrggbb` or bare `rrggbb` (six hex digits only — no 3-digit shorthand). Applied via
`Palette::apply_overrides`, on top of whichever base (`dark`/`light`/`auto`'s probe) was already
resolved — the mechanism is base-agnostic, so an override key works identically regardless of
`workon.review.theme`'s selection. **Uniform slot rule:** a slot override rewrites its
role-mapped field(s) even when the current base hand-authored that field explicitly (e.g.
`base08` under `theme = dark` replaces `dark()`'s hand-tuned `error_fg`) — the alternative
(silently ignoring slot overrides for authored fields) is a UX trap: a user who sets `base08`
expects red to change. Slot overrides do NOT re-derive the diff/cursor tints; that stays the 12
tint keys' job, applied last and verbatim, so a slot override can't reshape a hand-tuned wash it
wasn't asked to touch. An invalid value or an unrecognized key under `workon.review.theme.*` is
ignored with a startup warning (the same posture as ADR-034's keybinding validation) — not a
hard error.

**Named bundled schemes explicitly deferred.** `theme = <scheme-name>` selecting a whole
vendored base16 scheme (e.g. from tinted-theming/schemes, MIT-licensed and so licensing-clean
to vendor) was considered and set aside — the override-key tier covers the immediate need, and
named schemes slot in additively later (a `Theme::Named` variant + a `schemes.rs` of vendored
constants) without touching this work if demand appears.

**NO_COLOR (monochrome rendering).** The other extra this tier's Context section named — `NO_COLOR`, no CLI flag,
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
that blanket suppression and let the achromatic ladders through. That same re-enable, however,
also re-opens the icon color channel for `icons::icon_for_path`'s hardcoded per-filetype `Rgb` —
a palette-EXTERNAL color source `mono()`'s own `Color::Reset` fields can't reach — so `Palette`
carries a `colorless` flag (`false` on every curated/probed constructor, `true` only on `mono`)
and `render.rs`'s icon paint sites collapse to `foreground` themselves whenever it's set.

## Revised (diff foreground/background split)

The user-configurable-color-override-keys work's override table above named the diff washes `subtle`/`strong`. That naming is retired: it
described *intensity*, and intensity names invite reuse wherever something should look emphatic.
`render.rs`'s outline status column duly reached for `add_strong`/`del_strong` as **foregrounds**
for the X/Y letters — a background wash used as text color. On a theme whose washes are dark (the
motivating case: `add-strong #2d4654` on `background #27212e`) those letters land near 1.6:1
contrast and are effectively invisible.

The underlying axis was never intensity. It is **attribution precision**: one wash says "this line
contains a change", the other says "this exact text IS the change". Renamed accordingly, and split
across the two color channels:

| Old key | New key | Meaning |
| --- | --- | --- |
| `del-subtle` | `del-line-bg` | wash for a line containing a deletion |
| `del-strong` | `del-edit-bg` | wash for the deleted text itself |
| `add-subtle` | `add-line-bg` | wash for a line containing an addition |
| `add-strong` | `add-edit-bg` | wash for the added text itself |
| `del-staged-subtle` | `del-staged-line-bg` | staged counterparts of the four above |
| `del-staged-strong` | `del-staged-edit-bg` | |
| `add-staged-subtle` | `add-staged-line-bg` | |
| `add-staged-strong` | `add-staged-edit-bg` | |
| — | `add-fg` | tint foreground for added text |
| — | `del-fg` | tint foreground for deleted text |
| — | `add-staged-fg` | staged counterparts |
| — | `del-staged-fg` | |

Unqualified keys mean **unstaged (or combined-view)**; only the staged side is spelled out. The
asymmetry is deliberate — the unqualified form is the one most themes set, and lengthening it to
`add-unstaged-line-bg` taxes the common case to remove an ambiguity the table resolves.

**Why `edit` and not `word`.** `content_spans` paints the edit wash across a line's full width when
that line has no counterpart to word-diff against (a pure insertion or deletion). A `word` name
would be false in exactly that branch. `edit` is honest in both: on an unpaired line, the whole
line *is* the edit. The term is also standard diff vocabulary (edit script, edit distance) and
unclaimed elsewhere in this codebase, where `change` already means a file's change kind and
`Changeset` is a domain object.

**Foregrounds are per-state, not per-scope.** Four foreground keys, not eight: the line/edit
distinction is already carried by the background, and a foreground shift on top of a background
shift double-encodes one fact. The cost is that a theme cannot express "dimmed line, bright changed
words" — accepted, as it needs two foregrounds on one line and no scheme here has asked for it.

**Foreground defaults role-map to the accent slots**, matching how `error_fg`/`modified_fg` already
take base08/base09: `add-fg` ← base0B, `del-fg` ← base08. The staged pair dims toward base00, so
staged-ness reads in both channels — but **contrast-clamped**, not a fixed ratio. A flat 40% dim
collapses to 1.65:1 on a theme that sets its staged washes equal to its unstaged ones (staged-ness
then has no background signal, and the foreground is dimming against a full-strength wash). The
derivation dims by up to the nominal ratio and stops early at a relative-luminance floor against
that state's own edit wash. This is the first real contrast math in `theme.rs`, whose only prior
arithmetic was `tint_toward`'s per-channel lerp; it is worth the ~25 lines because the failure it
prevents is silent and theme-dependent. Note this is *not* the user-configurable-color-override-keys blend trap — that was about a
convex blend being unable to *reproduce* `dark()`'s hand-tuned washes (channels below base00);
blending an accent toward base00 for a foreground is well-defined, and `light()` already does it.

**The outline's X/Y status letters take `add-fg`/`del-fg`** — the bug that prompted this revision.
They are one concept with diff text ("the foreground color of added-ness"), so they share the key
rather than getting a dedicated pair. This does couple outline chrome to a diff key: retinting diff
text also retints the status column. Accepted; a theme wanting them apart can be revisited if it
appears.

**`workon.review.diff.text` selects the foreground source on changed lines** — `syntax` (default,
pixel-identical to the user-configurable-color-override-keys behavior), `tint` (changed lines take the tint foreground), `edit` (syntax
stays on the line; only edits take the tint foreground). Context lines always keep syntax
highlighting in every mode; `NO_COLOR`/`mono` still wins over all of it, unchanged. In `edit` mode
an unpaired line takes the tint foreground across its full width, preserving the invariant
**wherever the edit wash is painted, the tint foreground is painted** — one rule covering both
branches, rather than a foreground/background disagreement of the kind that produced the original
`strong` drift.

**base01 and base02 gain roles** (→ `filler_fg`, `selection_bg`), joining base03→`dim` and
base04→`gutter`. They were accepted by the parser and wired to nothing, so setting them failed
silently. The uniform slot rule is unchanged and the no-clobber rule survives: slot overrides seed,
tint keys still apply last and verbatim, so an explicit `selection-bg` beats a `base02`.
**base06, base07, and base0f remain unmapped** — nothing in this TUI is brighter than its
foreground, and base0f is base16's legacy grab-bag. They parse (namespace uniformity) and do
nothing, now documented rather than surprising.

**Migration is a hard rename.** The eight old wash keys are simply unrecognized and hit the
existing unknown-key startup warning. Pre-1.0, and a dual vocabulary would keep the retired model
discoverable — which is the thing this revision exists to undo.

**Corrections to the user-configurable-color-override-keys table above:** it said "the 11 tint keys" while listing 12 (fixed above to 12), and omits
`filler-fg` entirely (added later, when the filler hatch was screened back to its own base01
foreground). The table in this revision supersedes it for the diff keys; `cursor-bg`,
`selection-bg`, `cursor-unfocused-bg`, `pane-header-focused-fg`, and `filler-fg` are unchanged and
remain valid.

## References

- [ADR-034](034-review-git-native-config-schema.md) — `workon.review.theme` config key
- [ADR-006](006-git-native-config.md) — git-native config this builds on
- `git-workon-review/src/highlight.rs` — existing base16-conformant capture→slot template
- `git-workon-review/src/render.rs` — `const` palette + `tint_toward` blend helper being generalized
- base16 styling spec — slot role conventions (base08 red/Diff-Deleted, base0B green/Diff-Inserted, base0E keywords, …)
