//! The `theme = auto` terminal-derivation probe (ADR-035, CS6).
//!
//! `auto` derives the base16 *scheme* (syntax + monochrome ramp) from the terminal's own colors,
//! so code in the diff is highlighted in the same palette the user's terminal already uses. It
//! does this by querying the terminal over the controlling `/dev/tty` with OSC escape sequences
//! (`OSC 4;n;?` for the 16 ANSI colors, `OSC 11;?`/`OSC 10;?` for background/foreground), parsing
//! the RGB replies, and mapping ANSI-16 → the 16 base16 slots ([`crate::theme::Base16`]). The six
//! slots ANSI lacks are synthesized by interpolation (see [`build_base16`]). The diff/cursor
//! *tints* are NOT derived from the probe — [`crate::theme::Palette::from_terminal`] keeps them
//! curated by background luminance (the CS6 refinement of ADR-035).
//!
//! ## Robustness is the whole point
//!
//! The probe is the single most terminal-fragile component in the review TUI, so its blast radius
//! is contained to "return a curated theme instead":
//! - **Never hangs, but always waits.** The tty is set **non-blocking** and the whole read
//!   ([`read_replies`]) is bounded by a hard `Instant` deadline. `read()` therefore can never
//!   block on a terminal that doesn't answer (tmux without passthrough, ssh, CI, a dumb
//!   terminal) — a not-yet-answered read yields `WouldBlock` or, with the `VMIN=0` polling-read
//!   semantics the probe sets, `Ok(0)`. BOTH mean "no data yet", never EOF: treating `Ok(0)` as
//!   EOF made the deadline loop exit in microseconds, so the probe read nothing, the replies
//!   arrived after the [`query_terminal_raw`] flush, leaked into crossterm, and froze input at
//!   startup (the dogfood-round-2 wedge). Only the deadline and the DA1 sentinel end the wait.
//!   (A blocking read guarded only by `poll(2)` is NOT safe: `poll` on a tty is unreliable on
//!   macOS — it can report spurious readability — and `cfmakeraw` sets `VMIN=1`, so a blocking
//!   `read` after a bad `poll` waits forever.)
//! - **Never corrupts the terminal.** The probe runs BEFORE `tui::run` installs its own raw mode /
//!   alternate screen. It saves the tty's `termios`, sets raw for the duration of the read, and
//!   **always restores** the saved `termios` before returning — leaving the tty exactly as it was
//!   found. A trailing drain consumes any bytes the terminal still owed so none leak into
//!   crossterm's later input reads.
//! - **Any failure → `None`.** Can't open `/dev/tty`, not a tty, a partial/malformed reply, a
//!   missing color, or a timeout all collapse to an empty [`ProbeResult`], and the pure
//!   [`palette_for_auto`] decision turns that into a curated fallback.
//!
//! Everything above the thin `#[cfg(unix)]` tty read — parsing, the ANSI→base16 build, the
//! fallback decision — is pure and unit-tested with injected bytes; the tests never touch a real
//! terminal.

use std::time::Duration;

use ratatui::style::Color;

use crate::probe_cache;
use crate::theme::{self, tint_toward, Base16, Palette};

/// The colors read back from a terminal OSC probe. `ansi16` is `Some` only if **all 16** ANSI
/// colors answered (a partial answer is treated as no answer — see the module doc); `background`
/// and `foreground` are the `OSC 11`/`OSC 10` replies, each independently optional. A fully-empty
/// result (every field `None`) is what a failed or timed-out probe produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeResult {
    pub ansi16: Option<[Color; 16]>,
    pub background: Option<Color>,
    pub foreground: Option<Color>,
}

/// Resolve the palette for `theme = auto` by probing the controlling terminal. Always returns a
/// usable [`Palette`] — a terminal-derived one when the probe succeeds, a curated fallback
/// otherwise. This is the entry point `main.rs` calls; the timeout is the non-negotiable backstop
/// against a silent terminal.
///
/// The deadline is generous because it almost never bites: every interactive terminal answers the
/// DA1 sentinel (a VT100-era query), so the probe normally returns at the sentinel within a few
/// ms (or one network round-trip over ssh). Only a tty whose far end answers *nothing* waits the
/// full deadline — and giving up early on a merely-slow terminal is worse than the wait, because
/// replies that arrive after the probe stopped listening leak into crossterm as phantom
/// keystrokes (`r` → refresh storms, `d` → a discard confirm that captures the keyboard).
///
/// [`crate::probe_cache`] remembers a terminal that has already paid this deadline and gotten
/// nothing back: when this launch's controlling terminal has a live "silent" verdict cached, the
/// probe is skipped entirely and the curated fallback returns immediately (identical to what an
/// empty probe result would have produced). A terminal that answers ANYTHING is never cached, so
/// live detection keeps working there every launch.
///
/// The second element of the returned tuple is whether a real probe conversation happened on the
/// controlling tty this call — `false` only on a cache hit. `main.rs` uses it (instead of just
/// "theme was auto") to decide whether [`flush_pending_tty_input`] is needed: a cache hit writes
/// nothing to the tty, so no replies are ever owed and flushing would only risk eating legitimate
/// type-ahead (see that function's doc comment).
pub fn detect_auto_palette() -> (Palette, bool) {
    let key = probe_cache::terminal_key();
    if key.as_ref().is_some_and(probe_cache::is_cached_silent) {
        return (Palette::dark(), false);
    }

    let (probe, timed_out_silent) = probe_terminal(Duration::from_millis(800));
    if timed_out_silent {
        if let Some(key) = &key {
            probe_cache::record_silent(key);
        }
    }
    (palette_for_auto(&probe), true)
}

/// Discard any bytes pending on the controlling tty's input queue. `main.rs` calls this after the
/// `theme = auto` probe and immediately before the TUI takes over the terminal: OSC replies that
/// straggle in while the app is still assembling changesets (an ssh round-trip can outlast the
/// probe's deadline) would otherwise sit in the queue and reach crossterm as phantom keystrokes.
/// Only meaningful after a probe — an un-probed launch has no replies owed, and flushing would
/// discard legitimate type-ahead.
pub fn flush_pending_tty_input() {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Ok(tty) = std::fs::File::options().read(true).open("/dev/tty") {
            unsafe { libc::tcflush(tty.as_raw_fd(), libc::TCIFLUSH) };
        }
    }
}

/// The pure decision that turns a [`ProbeResult`] into a [`Palette`] (unit-tested with injected
/// results — it performs no I/O):
/// - A complete probe (all 16 ANSI colors **and** a background) → a terminal-derived palette
///   ([`Palette::from_terminal`] over the [`build_base16`] scheme).
/// - Otherwise, if only the background answered → the curated scheme picked by its luminance.
/// - Otherwise (no background at all) → curated dark, the safe default.
pub fn palette_for_auto(probe: &ProbeResult) -> Palette {
    match (probe.ansi16, probe.background) {
        (Some(ansi), Some(bg)) => Palette::from_terminal(build_base16(&ansi, bg, probe.foreground)),
        (_, Some(bg)) => {
            if theme::is_light_background(bg) {
                Palette::light()
            } else {
                Palette::dark()
            }
        }
        (_, None) => Palette::dark(),
    }
}

/// Map a probed ANSI-16 palette + background (+ optional foreground) onto the 16 base16 slots,
/// synthesizing the six slots ANSI has no equivalent for (ADR-035). The diff-critical slots
/// (`base00`/`base08`/`base0B`) are always real; the synthesized slots are secondary accents and
/// ramp intermediates:
/// - **Ramp** (`base01`/`base02`): interpolated `base00 → base03`.
/// - **Ramp** (`base04`/`base06`): interpolated across `base03 → base05 → base07`.
/// - **`base09` (orange):** blend of `base08` (red) toward `base0A` (yellow).
/// - **`base0F` (brown):** blend of `base09` toward `base08`.
///
/// ANSI role mapping (the standard base16 ↔ ANSI correspondence): `base00`=bg, `base03`=ANSI 8
/// (bright black), `base05`=fg or ANSI 7 (white), `base07`=ANSI 15 (bright white), `base08`=ANSI 1
/// (red), `base0A`=ANSI 3 (yellow), `base0B`=ANSI 2 (green), `base0C`=ANSI 6 (cyan), `base0D`=ANSI
/// 4 (blue), `base0E`=ANSI 5 (magenta).
pub fn build_base16(ansi: &[Color; 16], background: Color, foreground: Option<Color>) -> Base16 {
    let base00 = background;
    let base03 = ansi[8]; // bright black
    let base05 = foreground.unwrap_or(ansi[7]); // fg, else white
    let base07 = ansi[15]; // bright white
    let base08 = ansi[1]; // red
    let base0a = ansi[3]; // yellow
    let base0b = ansi[2]; // green
    let base0c = ansi[6]; // cyan
    let base0d = ansi[4]; // blue
    let base0e = ansi[5]; // magenta

    // Synthesized: ramp intermediates + the two accents ANSI has no slot for.
    let base01 = tint_toward(base00, base03, 1.0 / 3.0);
    let base02 = tint_toward(base00, base03, 2.0 / 3.0);
    let base04 = tint_toward(base03, base05, 0.5);
    let base06 = tint_toward(base05, base07, 0.5);
    let base09 = tint_toward(base08, base0a, 0.5); // orange between red and yellow
    let base0f = tint_toward(base09, base08, 0.5); // brown

    Base16 {
        slots: [
            base00, base01, base02, base03, base04, base05, base06, base07, base08, base09, base0a,
            base0b, base0c, base0d, base0e, base0f,
        ],
    }
}

/// Run the tty probe, collapsing any failure to an empty [`ProbeResult`]. On non-unix platforms
/// (no `/dev/tty` / `termios`) it always reports empty, so `auto` degrades to curated dark there.
///
/// The second element is `true` only when [`query_terminal_raw`] paid the FULL `timeout` and
/// still got zero reply bytes — [`probe_cache`]'s one cacheable case. A terminal that answered
/// (even partially) or a probe that couldn't even start (no `/dev/tty`, not a tty, a failed
/// write) are both `false`: the former has nothing to cache, the latter never waited long enough
/// for caching to save anything.
fn probe_terminal(timeout: Duration) -> (ProbeResult, bool) {
    #[cfg(unix)]
    {
        match query_terminal_raw(&build_query(), timeout) {
            ProbeOutcome::Replied(bytes) => (parse_osc_replies(&bytes), false),
            ProbeOutcome::TimedOutSilent => (ProbeResult::default(), true),
            ProbeOutcome::Unavailable => (ProbeResult::default(), false),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        (ProbeResult::default(), false)
    }
}

/// The outcome of one attempt at [`query_terminal_raw`] — distinguishes "the terminal answered"
/// from the two different ways it can answer nothing, only one of which is worth caching (see
/// [`probe_terminal`]'s doc comment).
#[cfg(unix)]
enum ProbeOutcome {
    /// At least one reply byte arrived.
    Replied(Vec<u8>),
    /// The probe wrote its query, waited the full `timeout`, and got nothing back — the terminal
    /// this launch already paid the deadline for.
    TimedOutSilent,
    /// Probing wasn't possible this launch at all (no `/dev/tty`, not a tty, the query write
    /// failed) — always fast, never worth remembering.
    Unavailable,
}

/// The bytes we write to the terminal: `OSC 4;n;?` for each of the 16 ANSI colors, then
/// `OSC 11;?` (background) and `OSC 10;?` (foreground), then a primary Device Attributes query
/// (`ESC [ c`). Terminals answer in order, so the DA1 reply is a sentinel: once we see it, every
/// OSC reply that is going to arrive already has (see [`has_da1_terminator`]). Each OSC query is
/// String-Terminated with `ESC \` (ST).
fn build_query() -> Vec<u8> {
    let mut q = Vec::new();
    for n in 0..16 {
        q.extend_from_slice(format!("\x1b]4;{n};?\x1b\\").as_bytes());
    }
    q.extend_from_slice(b"\x1b]11;?\x1b\\");
    q.extend_from_slice(b"\x1b]10;?\x1b\\");
    q.extend_from_slice(b"\x1b[c"); // DA1 sentinel
    q
}

/// Parse one OSC color-spec body of the form `rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel,
/// per the XParseColor grammar terminals reply with) into an 8-bit [`Color::Rgb`]. Each channel is
/// scaled from its `0..=(16^digits - 1)` range to `0..=255`, so a 4-digit `cccc` yields `0xcc`
/// (the "take the high byte" the ADR describes) and a 2-digit `cc` yields `0xcc` too. Returns
/// `None` on any malformation (wrong prefix, missing channel, non-hex, empty/over-long channel).
fn parse_rgb_spec(spec: &str) -> Option<Color> {
    let rest = spec.strip_prefix("rgb:")?;
    let mut channels = rest.split('/');
    let r = parse_channel(channels.next()?)?;
    let g = parse_channel(channels.next()?)?;
    let b = parse_channel(channels.next()?)?;
    if channels.next().is_some() {
        return None; // more than three channels → malformed
    }
    Some(Color::Rgb(r, g, b))
}

/// Parse and 8-bit-scale one hex channel (`1..=4` digits). `None` on empty, over-long, or non-hex.
fn parse_channel(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(s, 16).ok()?;
    let max = (1u32 << (4 * s.len())) - 1; // 16^digits - 1
                                           // Round-to-nearest scale into 0..=255.
    Some(((value * 255 + max / 2) / max) as u8)
}

/// Slice out the payloads of every complete OSC sequence in `bytes`: the run between an `ESC ]`
/// introducer and its terminator (`BEL`, or `ESC \` ST). An unterminated trailing OSC is dropped.
fn osc_payloads(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b']' {
            let start = i + 2;
            let mut j = start;
            let mut terminated = None;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    terminated = Some((j, j + 1)); // BEL
                    break;
                }
                if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                    terminated = Some((j, j + 2)); // ESC \ (ST)
                    break;
                }
                j += 1;
            }
            match terminated {
                Some((content_end, next)) => {
                    out.push(&bytes[start..content_end]);
                    i = next;
                }
                None => break, // incomplete trailing OSC
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Parse a raw terminal reply buffer into a [`ProbeResult`]: pick out every `OSC 4;n;<spec>`,
/// `OSC 11;<spec>`, and `OSC 10;<spec>` reply and decode its color. `ansi16` is `Some` only when
/// all 16 indices decoded; a duplicate or out-of-range index is ignored, and the DA1 reply (and
/// any other noise) is skipped since it carries no OSC color prefix.
fn parse_osc_replies(bytes: &[u8]) -> ProbeResult {
    let mut ansi: [Option<Color>; 16] = [None; 16];
    let mut background = None;
    let mut foreground = None;

    for payload in osc_payloads(bytes) {
        let Ok(s) = std::str::from_utf8(payload) else {
            continue;
        };
        if let Some(rest) = s.strip_prefix("4;") {
            if let Some((index, spec)) = rest.split_once(';') {
                if let (Ok(idx), Some(color)) = (index.parse::<usize>(), parse_rgb_spec(spec)) {
                    if idx < 16 {
                        ansi[idx] = Some(color);
                    }
                }
            }
        } else if let Some(spec) = s.strip_prefix("11;") {
            background = parse_rgb_spec(spec);
        } else if let Some(spec) = s.strip_prefix("10;") {
            foreground = parse_rgb_spec(spec);
        }
    }

    let ansi16 = if ansi.iter().all(Option::is_some) {
        Some(std::array::from_fn(|i| ansi[i].expect("all-some checked")))
    } else {
        None
    };
    ProbeResult {
        ansi16,
        background,
        foreground,
    }
}

/// Whether the buffer contains a complete DA1 reply (`ESC [ ? … c`) — our "the terminal is done
/// answering" sentinel. A `c` following an `ESC [ ?` control sequence introducer.
fn has_da1_terminator(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'?' {
            // Scan for the final `c` of this CSI sequence.
            let mut j = i + 3;
            while j < bytes.len() {
                if bytes[j] == b'c' {
                    return true;
                }
                // A different final byte (letter) ends the CSI without being DA1; keep scanning
                // the buffer for another `ESC [ ?`.
                if bytes[j].is_ascii_alphabetic() {
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    false
}

/// Open the controlling `/dev/tty`, put it in raw mode, write `query`, and read the reply with a
/// hard total `timeout` — restoring the saved `termios` before returning on **every** path. This
/// is the one function the unit tests do NOT call (it needs a real tty); everything it feeds
/// ([`parse_osc_replies`], [`build_base16`], [`palette_for_auto`]) is pure and tested directly.
///
/// Returns a [`ProbeOutcome`]: `Replied` bytes, `TimedOutSilent` when the full `timeout` elapsed
/// with nothing back, or `Unavailable` when `/dev/tty` can't be opened, isn't a tty, or the query
/// write itself fails (all of which return fast, well under `timeout`). The
/// `elapsed >= timeout` check distinguishing the latter two is deliberately a wall-clock
/// comparison rather than a distinct signal threaded up from [`read_replies`] — it needs no
/// change to that function or its already-covered unit tests, and the two cases are only ever
/// milliseconds vs. the full deadline apart.
#[cfg(unix)]
fn query_terminal_raw(query: &[u8], timeout: Duration) -> ProbeOutcome {
    use std::os::unix::io::AsRawFd;
    use std::time::Instant;

    let Ok(mut tty) = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        return ProbeOutcome::Unavailable;
    };
    let fd = tty.as_raw_fd();

    // Save the current termios; bail (leaving the tty untouched) if this isn't a tty.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return ProbeOutcome::Unavailable;
    }

    // Switch to raw so the OSC replies (terminated by ST/BEL, not newline) arrive uncooked and
    // unechoed. Override cfmakeraw's `VMIN=1` with `VMIN=0, VTIME=1` (0.1s): a defensive backstop
    // so that even if the `O_NONBLOCK` fcntl in `read_replies` were to fail, a blocking `read`
    // still returns (empty) after 0.1s rather than hanging on a silent terminal.
    let mut raw = saved;
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 1;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return ProbeOutcome::Unavailable; // termios unchanged — nothing to restore
    }

    let started = Instant::now();
    let outcome = read_replies(&mut tty, fd, query, timeout);

    // Discard anything still in the terminal's input queue before handing the tty back — a
    // terminal that answered our OSC queries may have more reply bytes buffered than `read_replies`
    // consumed (or that arrived just after it stopped at the DA1 sentinel). Left there, they leak
    // into crossterm's input once the TUI starts and get parsed as a burst of spurious key events
    // (the "unresponsive + refresh churn on launch" seen with `theme = auto`). The probe runs
    // before any real keypress, so flushing pending input is safe.
    unsafe { libc::tcflush(fd, libc::TCIFLUSH) };

    // ALWAYS restore, on success or failure.
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };

    match outcome {
        Some(bytes) => ProbeOutcome::Replied(bytes),
        None if started.elapsed() >= timeout => ProbeOutcome::TimedOutSilent,
        None => ProbeOutcome::Unavailable, // the query write failed — an early bail, not a wait
    }
}

/// The read half of [`query_terminal_raw`], factored out so `termios` restoration wraps it on
/// every exit. Writes `query`, then polls for replies until the DA1 sentinel arrives or the total
/// `timeout` elapses, then drains any straggler bytes so nothing leaks into later input reads.
#[cfg(unix)]
fn read_replies(
    tty: &mut std::fs::File,
    fd: std::os::unix::io::RawFd,
    query: &[u8],
    timeout: Duration,
) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    use std::time::Instant;

    if tty.write_all(query).is_err() || tty.flush().is_err() {
        return None;
    }

    // Switch to non-blocking for the read: a silent terminal must yield `WouldBlock` (or the
    // `VMIN=0` polling-read `Ok(0)`), never a blocked `read`. The `Instant` deadline (not `poll`)
    // is the sole timing authority.
    set_nonblocking(fd);

    let mut buf = collect_replies(|chunk| tty.read(chunk), Instant::now() + timeout);
    let mut chunk = [0u8; 256];

    // Drain anything immediately available (e.g. a terminal that answered without a DA1) so it
    // doesn't surface as spurious input once the TUI takes over the tty. Stops the moment nothing
    // is pending (`Ok(0)` or `WouldBlock`), so it never waits.
    loop {
        match tty.read(&mut chunk) {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }

    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Accumulate terminal reply bytes from `read` until the DA1 sentinel arrives or `deadline`
/// passes — the read half of [`read_replies`], seamed on the reader so the loop's give-up
/// conditions are unit-testable without a tty.
///
/// Both `Ok(0)` and `WouldBlock` mean "the terminal hasn't answered yet", NEVER end-of-file:
/// with the `VMIN=0` termios the probe sets, a tty `read` is a *polling read* that returns 0
/// immediately when the queue is empty. Only a genuine read error ends the wait early — every
/// "no data yet" result just yields briefly and retries until the deadline.
#[cfg(unix)]
fn collect_replies(
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
    deadline: std::time::Instant,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];

    while std::time::Instant::now() < deadline {
        match read(&mut chunk) {
            Ok(n) if n > 0 => {
                buf.extend_from_slice(&chunk[..n]);
                if has_da1_terminator(&buf) {
                    break;
                }
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
    buf
}

/// Set `O_NONBLOCK` on the fd so `read` returns `WouldBlock` instead of blocking when the terminal
/// has nothing (more) to say. Best-effort: a failed `fcntl` leaves the fd blocking, but the caller
/// only reaches here after a successful `tcgetattr`, and the deadline loop still bounds the wait in
/// the common case. The fd is closed when the `File` drops, so `O_NONBLOCK` needs no restoration.
#[cfg(unix)]
fn set_nonblocking(fd: std::os::unix::io::RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OSC color-spec parsing ───────────────────────────────────────────────

    #[test]
    fn parse_rgb_spec_takes_the_high_byte_of_a_16_bit_channel() {
        // The plan's worked example: cccc → cc, 9999 → 99.
        assert_eq!(
            parse_rgb_spec("rgb:cccc/9999/cccc"),
            Some(Color::Rgb(0xcc, 0x99, 0xcc))
        );
    }

    #[test]
    fn parse_rgb_spec_accepts_short_channels_and_scales_them() {
        // 8-bit channels pass through unchanged.
        assert_eq!(
            parse_rgb_spec("rgb:ff/80/00"),
            Some(Color::Rgb(0xff, 0x80, 0x00))
        );
        // A single hex digit f == 15/15 of full scale == 255.
        assert_eq!(parse_rgb_spec("rgb:f/0/f"), Some(Color::Rgb(255, 0, 255)));
    }

    #[test]
    fn parse_rgb_spec_rejects_malformed_specs() {
        assert_eq!(parse_rgb_spec("rgb:cccc/9999"), None); // too few channels
        assert_eq!(parse_rgb_spec("rgb:cc/dd/ee/ff"), None); // too many channels
        assert_eq!(parse_rgb_spec("cmyk:1/2/3"), None); // wrong prefix
        assert_eq!(parse_rgb_spec("rgb:zz/00/00"), None); // non-hex
        assert_eq!(parse_rgb_spec("rgb:/00/00"), None); // empty channel
        assert_eq!(parse_rgb_spec("rgb:11111/00/00"), None); // over-long channel
    }

    // ── Whole-reply parsing → ProbeResult ────────────────────────────────────

    /// A complete, well-formed reply: 16 `OSC 4` colors + `OSC 11` bg + `OSC 10` fg + a DA1 tail.
    fn full_reply() -> Vec<u8> {
        let mut r = Vec::new();
        for n in 0..16u8 {
            // A recognizable per-index color: R = n*16, so index 5 → 0x50….
            let hh = format!("{:02x}", n * 16);
            r.extend_from_slice(format!("\x1b]4;{n};rgb:{hh}{hh}/2020/4040\x1b\\").as_bytes());
        }
        r.extend_from_slice(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"); // dark bg
        r.extend_from_slice(b"\x1b]10;rgb:d3d3/d0d0/c8c8\x1b\\"); // fg
        r.extend_from_slice(b"\x1b[?62;c"); // DA1
        r
    }

    #[test]
    fn parse_osc_replies_decodes_a_complete_reply() {
        let result = parse_osc_replies(&full_reply());
        let ansi = result.ansi16.expect("all 16 colors present");
        assert_eq!(ansi[0], Color::Rgb(0x00, 0x20, 0x40));
        assert_eq!(ansi[5], Color::Rgb(0x50, 0x20, 0x40));
        assert_eq!(ansi[15], Color::Rgb(0xf0, 0x20, 0x40));
        assert_eq!(result.background, Some(Color::Rgb(0x1a, 0x1a, 0x1a)));
        assert_eq!(result.foreground, Some(Color::Rgb(0xd3, 0xd0, 0xc8)));
    }

    #[test]
    fn parse_osc_replies_treats_a_missing_ansi_color_as_no_ansi() {
        // Drop index 7's reply: the remaining 15 must NOT yield a partial ansi16.
        let mut r = Vec::new();
        for n in 0..16u8 {
            if n == 7 {
                continue;
            }
            r.extend_from_slice(format!("\x1b]4;{n};rgb:1010/2020/3030\x1b\\").as_bytes());
        }
        r.extend_from_slice(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\");
        let result = parse_osc_replies(&r);
        assert_eq!(result.ansi16, None, "an incomplete ANSI set is no set");
        // ...but the background still parsed, so the fallback can use its luminance.
        assert_eq!(result.background, Some(Color::Rgb(0x1a, 0x1a, 0x1a)));
    }

    #[test]
    fn parse_osc_replies_of_garbage_is_empty() {
        assert_eq!(parse_osc_replies(b""), ProbeResult::default());
        assert_eq!(
            parse_osc_replies(b"not an escape sequence"),
            ProbeResult::default()
        );
        // An unterminated OSC is dropped rather than mis-parsed.
        assert_eq!(
            parse_osc_replies(b"\x1b]11;rgb:1a1a/1a1a/1a1a"),
            ProbeResult::default()
        );
    }

    #[test]
    fn osc_payloads_handles_both_bel_and_st_terminators() {
        let bytes = b"\x1b]11;rgb:aa/bb/cc\x07\x1b]10;rgb:11/22/33\x1b\\";
        let payloads = osc_payloads(bytes);
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0], b"11;rgb:aa/bb/cc");
        assert_eq!(payloads[1], b"10;rgb:11/22/33");
    }

    // ── collect_replies give-up conditions ───────────────────────────────────

    /// A reader that scripts each successive `read` call's result: `Ok(&[u8])` delivers bytes,
    /// `Err(kind)` returns that error kind. Exhausting the script yields `Ok(0)` ("no data yet").
    #[cfg(unix)]
    fn scripted_reader(
        script: Vec<Result<&'static [u8], std::io::ErrorKind>>,
    ) -> impl FnMut(&mut [u8]) -> std::io::Result<usize> {
        let mut steps = script.into_iter();
        move |chunk: &mut [u8]| match steps.next() {
            Some(Ok(bytes)) => {
                chunk[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
            Some(Err(kind)) => Err(kind.into()),
            None => Ok(0),
        }
    }

    #[cfg(unix)]
    fn soon() -> std::time::Instant {
        std::time::Instant::now() + Duration::from_millis(200)
    }

    #[cfg(unix)]
    #[test]
    fn collect_replies_treats_zero_byte_reads_as_pending_not_eof() {
        // The dogfood-round-2 wedge: with `VMIN=0` a tty read returns `Ok(0)` while the terminal
        // is still composing its answer. The loop must keep waiting — bailing here left the
        // replies to arrive after the probe's flush and freeze crossterm's input at startup.
        let read = scripted_reader(vec![
            Ok(b""),
            Ok(b""),
            Ok(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"),
            Ok(b"\x1b[?62;22c"),
        ]);
        let buf = collect_replies(read, soon());
        assert!(
            buf.starts_with(b"\x1b]11;"),
            "replies after Ok(0) polling reads must still be collected"
        );
        assert!(has_da1_terminator(&buf), "loop ran on to the DA1 sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn collect_replies_stops_at_the_da1_sentinel() {
        // Bytes offered after the DA1 reply must never be consumed — the sentinel ends the read
        // so the probe returns promptly on a terminal that answered everything.
        let read = scripted_reader(vec![Ok(b"\x1b[?62;22c"), Ok(b"leftover")]);
        let buf = collect_replies(read, soon());
        assert_eq!(buf, b"\x1b[?62;22c");
    }

    #[cfg(unix)]
    #[test]
    fn collect_replies_waits_out_would_block_and_gives_up_at_the_deadline() {
        // WouldBlock is the O_NONBLOCK "no data yet"; a terminal that never answers must yield
        // an empty buffer once the deadline passes — bounded, not hung, and nothing invented.
        let read = scripted_reader(vec![Err(std::io::ErrorKind::WouldBlock); 3]);
        let buf = collect_replies(read, soon());
        assert!(buf.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn collect_replies_gives_up_on_a_real_read_error() {
        // A genuine error (not WouldBlock) ends the wait early with whatever already arrived.
        let start = std::time::Instant::now();
        let read = scripted_reader(vec![
            Ok(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"),
            Err(std::io::ErrorKind::Other),
        ]);
        let buf = collect_replies(read, std::time::Instant::now() + Duration::from_secs(5));
        assert!(buf.starts_with(b"\x1b]11;"));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "error must end the wait, not the deadline"
        );
    }

    #[test]
    fn da1_terminator_detected_only_when_complete() {
        assert!(has_da1_terminator(b"\x1b[?62;1;c"));
        assert!(has_da1_terminator(b"prefix\x1b[?6c trailing"));
        assert!(!has_da1_terminator(b"\x1b[?62;1")); // no final c yet
        assert!(!has_da1_terminator(b"\x1b[c")); // DA1 request, not a DA1 reply (no ?)
        assert!(!has_da1_terminator(b""));
    }

    // ── ANSI-16 → Base16 build + synthesis ───────────────────────────────────

    /// A recognizable ANSI-16 array: each color's red channel is its index * 16.
    fn sample_ansi() -> [Color; 16] {
        std::array::from_fn(|i| Color::Rgb((i as u8) * 16, 0x40, 0x80))
    }

    #[test]
    fn build_base16_maps_ansi_roles_onto_the_right_slots() {
        let ansi = sample_ansi();
        let bg = Color::Rgb(0x20, 0x20, 0x20);
        let base = build_base16(&ansi, bg, Some(Color::Rgb(0xd0, 0xd0, 0xd0)));

        assert_eq!(base.slots[0], bg); // base00 = background
        assert_eq!(base.slots[3], ansi[8]); // base03 = bright black
        assert_eq!(base.slots[5], Color::Rgb(0xd0, 0xd0, 0xd0)); // base05 = foreground
        assert_eq!(base.slots[7], ansi[15]); // base07 = bright white
        assert_eq!(base.slots[8], ansi[1]); // base08 = red
        assert_eq!(base.slots[10], ansi[3]); // base0A = yellow
        assert_eq!(base.slots[11], ansi[2]); // base0B = green
        assert_eq!(base.slots[12], ansi[6]); // base0C = cyan
        assert_eq!(base.slots[13], ansi[4]); // base0D = blue
        assert_eq!(base.slots[14], ansi[5]); // base0E = magenta
    }

    #[test]
    fn build_base16_falls_back_to_ansi7_when_no_foreground_probed() {
        let ansi = sample_ansi();
        let base = build_base16(&ansi, Color::Rgb(0x20, 0x20, 0x20), None);
        assert_eq!(base.slots[5], ansi[7]); // base05 = white when OSC 10 didn't answer
    }

    fn channels(color: Color) -> (i32, i32, i32) {
        match color {
            Color::Rgb(r, g, b) => (r as i32, g as i32, b as i32),
            other => panic!("expected RGB, got {other:?}"),
        }
    }

    #[test]
    fn build_base16_synthesizes_a_monotonic_dark_ramp() {
        // base00 (dark) → base01 → base02 → base03 must climb in luminance.
        let ansi = sample_ansi();
        let base = build_base16(&ansi, Color::Rgb(0x10, 0x10, 0x10), None);
        let lum = |i: usize| {
            let (r, g, b) = channels(base.slots[i]);
            r + g + b
        };
        assert!(lum(0) <= lum(1), "base00 ≤ base01");
        assert!(lum(1) <= lum(2), "base01 ≤ base02");
        assert!(lum(2) <= lum(3), "base02 ≤ base03");
    }

    #[test]
    fn build_base16_synthesizes_orange_between_red_and_yellow() {
        // base09 (orange) is the midpoint of base08 (red) and base0A (yellow), per channel.
        let red = Color::Rgb(0xf0, 0x00, 0x00);
        let yellow = Color::Rgb(0xf0, 0xf0, 0x00);
        let mut ansi = sample_ansi();
        ansi[1] = red; // base08
        ansi[3] = yellow; // base0A
        let base = build_base16(&ansi, Color::Rgb(0x20, 0x20, 0x20), None);
        let (r, g, b) = channels(base.slots[9]); // base09
        let (rr, rg, _) = channels(red);
        let (_, yg, _) = channels(yellow);
        assert_eq!(r, rr, "orange keeps the shared red channel");
        assert!(g > rg && g < yg, "orange green sits between red and yellow");
        assert_eq!(b, 0, "orange blue stays at zero");
    }

    // ── palette_for_auto decision ────────────────────────────────────────────

    #[test]
    fn palette_for_auto_builds_a_terminal_palette_from_a_complete_probe() {
        let probe = ProbeResult {
            ansi16: Some(sample_ansi()),
            background: Some(Color::Rgb(0x1a, 0x1a, 0x1a)), // dark
            foreground: Some(Color::Rgb(0xd0, 0xd0, 0xd0)),
        };
        let palette = palette_for_auto(&probe);
        // Syntax comes from the PROBED scheme (keyword → base0E = ANSI magenta = ansi[5]).
        let expected = build_base16(
            &sample_ansi(),
            Color::Rgb(0x1a, 0x1a, 0x1a),
            Some(Color::Rgb(0xd0, 0xd0, 0xd0)),
        );
        let keyword = crate::highlight::capture_index("keyword").unwrap();
        assert_eq!(palette.syntax(keyword), expected.slots[14]);
        // A dark probed bg borrows dark's curated tints.
        assert_eq!(palette.del_subtle, Palette::dark().del_subtle);
    }

    #[test]
    fn palette_for_auto_falls_back_to_curated_by_bg_luminance_when_ansi_is_missing() {
        // No ANSI colors, but the background answered light → curated light.
        let light_bg = ProbeResult {
            ansi16: None,
            background: Some(Color::Rgb(0xf5, 0xf5, 0xf5)),
            foreground: None,
        };
        assert_eq!(
            palette_for_auto(&light_bg).del_subtle,
            Palette::light().del_subtle
        );

        // Background answered dark → curated dark.
        let dark_bg = ProbeResult {
            ansi16: None,
            background: Some(Color::Rgb(0x1a, 0x1a, 0x1a)),
            foreground: None,
        };
        assert_eq!(
            palette_for_auto(&dark_bg).del_subtle,
            Palette::dark().del_subtle
        );
    }

    #[test]
    fn palette_for_auto_falls_back_to_dark_when_nothing_answered() {
        // The total-failure / timeout path: an empty result → curated dark, never a hang.
        assert_eq!(
            palette_for_auto(&ProbeResult::default()).del_subtle,
            Palette::dark().del_subtle
        );
    }
}
