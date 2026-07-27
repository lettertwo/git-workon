//! OSC 52 clipboard writes (M11, `copy-path-line`).
//!
//! The only clipboard mechanism this crate has: an OSC 52 "set clipboard" escape sequence
//! written to the controlling tty. No `arboard`-style dependency — the M11 handoff's locked
//! decision 1 rejected both a pure-`arboard` approach (dependency tree, dead over SSH) and an
//! `arboard`-with-OSC-52-fallback hybrid (two code paths for one keybinding, which the
//! CLAUDE.md "simplicity wins" rule doesn't allow without a concrete reason). `base64_encode`
//! below hand-rolls the small amount of base64 OSC 52 needs rather than pulling in a crate for
//! it.
//!
//! ## Fire-and-forget, by protocol
//!
//! OSC 52 has no reply: a terminal that honors it just updates its clipboard silently, and one
//! that doesn't either ignores the sequence or (rarely) echoes stray bytes if some intermediate
//! layer mishandles it — either way, nothing comes back on the wire to tell the caller which
//! happened. [`write_osc52`] returning `Ok(())` therefore means only "the bytes reached
//! `/dev/tty`", never "the clipboard actually changed". [`crate::app::App::copy_path_line`]'s
//! footer notice is worded to match: "copied ... to clipboard", never "clipboard updated" —
//! the latter phrasing is a claim a silent failure could falsify.
//!
//! ## Known gaps (deliberately deferred)
//!
//! - **Terminal.app does not implement OSC 52 at all.** The write reaches the tty and is
//!   silently swallowed; there is no way to detect this from here.
//! - **tmux only forwards OSC 52 to the outer terminal when `set -g set-clipboard on`** is set
//!   in `tmux.conf`. Without it, tmux eats the sequence itself.
//!
//! Neither is fixable from this call site alone, and the target environment (Kitty, no tmux, no
//! SSH) doesn't hit either — so both are logged here as the discoverable next step (a fallback
//! mechanism behind [`write_osc52`]) rather than worked around now.
//!
//! ## Why this write skips `terminal_query.rs`'s tty discipline
//!
//! [`crate::terminal_query`]'s module doc documents hard-won rules for talking to `/dev/tty`:
//! non-blocking reads, a hard deadline, always-restore `termios`. Those rules exist because that
//! probe READS a reply under a raw-mode tty it does not yet own (it runs before `tui::run`
//! installs raw mode/alt screen). This call is a pure write, with no reply to wait for, running
//! AFTER the TUI has already put the tty in raw mode and owns it for the session — there is
//! nothing to save/restore and no deadline to bound, so none of that machinery applies. The one
//! rule that does carry over: never leave the tty in a different state than found. A write of a
//! newline-free escape sequence can't perturb canonical/raw mode or echo (those govern how input
//! is read back, not how output is written), so simply opening, writing, and closing `/dev/tty`
//! satisfies that rule for free.

use std::io;

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64-encode `data` with the standard alphabet and `=` padding (RFC 4648 section 4) — what
/// OSC 52's payload requires. Hand-rolled rather than a dependency; see the module doc.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Wrap `payload` as an OSC 52 "set clipboard" escape sequence: `ESC ] 52 ; c ; <base64> ESC \`.
/// `c` selects the system clipboard (as opposed to OSC 52's `p`/`q` primary/secondary selection
/// targets, which this crate never uses). Returns the raw bytes ready to write to a tty.
pub(crate) fn osc52_sequence(payload: &str) -> Vec<u8> {
    let b64 = base64_encode(payload.as_bytes());
    let mut seq = Vec::with_capacity(b64.len() + 8);
    seq.push(0x1b); // ESC
    seq.extend_from_slice(b"]52;c;");
    seq.extend_from_slice(b64.as_bytes());
    seq.push(0x1b); // ST (String Terminator) part 1
    seq.push(b'\\'); // ST part 2
    seq
}

/// Write an OSC 52 "set clipboard" sequence for `payload` to the controlling tty. See the module
/// doc for why this is fire-and-forget and why it doesn't touch `termios`. The `Err` case this
/// CAN detect and surface is real: `/dev/tty` failing to open (no controlling terminal at all,
/// e.g. output piped somewhere unusual) or the write itself failing — as opposed to the terminal
/// silently declining to honor a sequence that reached it, which is undetectable by design.
#[cfg(unix)]
pub(crate) fn write_osc52(payload: &str) -> io::Result<()> {
    use std::io::Write;
    let mut tty = std::fs::File::options().write(true).open("/dev/tty")?;
    tty.write_all(&osc52_sequence(payload))?;
    tty.flush()
}

#[cfg(not(unix))]
pub(crate) fn write_osc52(_payload: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OSC 52 clipboard write is only implemented on unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_pads_per_rfc_4648() {
        // The three padding cases: 0, 1, 2 bytes of trailing padding.
        assert_eq!(base64_encode(b"foo"), "Zm9v"); // 3 bytes, no padding
        assert_eq!(base64_encode(b"fo"), "Zm8="); // 2 bytes, one pad
        assert_eq!(base64_encode(b"f"), "Zg=="); // 1 byte, two pads
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_matches_a_realistic_path_line_payload() {
        assert_eq!(base64_encode(b"src/app.rs:42"), "c3JjL2FwcC5yczo0Mg==");
    }

    /// Assert the exact byte sequence, not terminal behavior (per the M11 handoff) — this is the
    /// wire format every OSC-52-aware terminal parses, so any drift here is a real regression.
    #[test]
    fn osc52_sequence_wraps_base64_payload_in_esc_bracket_st() {
        let seq = osc52_sequence("foo");
        let mut expected = vec![0x1b];
        expected.extend_from_slice(b"]52;c;Zm9v");
        expected.push(0x1b);
        expected.push(b'\\');
        assert_eq!(seq, expected);
    }

    #[test]
    fn osc52_sequence_encodes_a_path_line_payload() {
        let seq = osc52_sequence("src/app.rs:42");
        assert_eq!(seq, b"\x1b]52;c;c3JjL2FwcC5yczo0Mg==\x1b\\".to_vec());
    }
}
