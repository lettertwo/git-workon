//! Cache for the `theme = auto` probe's "this terminal never answers" verdict (a follow-up to
//! ADR-031, whose scope section deferred it).
//!
//! [`crate::terminal_query::detect_auto_palette`] pays an 800ms timeout ONLY when the terminal
//! answers nothing at all — every real interactive terminal answers within a few ms, so a full
//! timeout means the controlling terminal (tmux without passthrough, plain ssh, CI, a dumb
//! terminal) structurally cannot answer and never will, this launch or the next. This module
//! remembers that one verdict so later launches from the same terminal skip the probe and go
//! straight to the curated fallback ([`crate::theme::Palette::dark`] — exactly what an empty
//! probe result already produces, so a cache hit changes timing, never the resulting palette).
//!
//! **Only silence is cached.** A terminal that answers ANYTHING — even just the DA1 sentinel,
//! even a partial/malformed color — returns in milliseconds and is never recorded here, so live
//! theme detection (e.g. macOS Terminal flipping between its light and dark profiles) keeps
//! working every launch.
//!
//! ## Key
//! The controlling tty's CONCRETE device path (`/dev/ttysNNN` on macOS, `/dev/pts/N` on Linux,
//! via `ttyname_r` on the first standard fd that is a tty) plus `$TERM` and `$TERM_PROGRAM`. The
//! tty path scopes a verdict to one terminal window; TERM/TERM_PROGRAM guard against a later,
//! DIFFERENT emulator reusing a recycled tty number and inheriting a stale "silent" verdict it
//! never earned. The name must NOT come from an fd opened on `/dev/tty`: macOS's `ttyname_r`
//! reports that fd as the literal "/dev/tty", one constant key shared by every terminal window —
//! which let a single silent verdict (a dogfood run under `expect`) put every real kitty window
//! on the curated-dark fallback for the whole TTL (the 2026-07 auto-theme-goes-dark bug).
//!
//! ## Store
//! A small human-readable JSON array of `{tty, term, term_program, timestamp}` objects under
//! [`dirs::cache_dir`]`/git-workon-review/silent-terminals.json` (overridable via the
//! `WORKON_REVIEW_PROBE_CACHE` env var — used by the PTY test suites to keep them off the real
//! user cache; see `tests/pty_support/mod.rs`). Entries expire after [`TTL_SECS`] (30 days) and
//! are pruned opportunistically the next time an entry is written, so the file never grows
//! unboundedly across many terminals.
//!
//! ## Escaping a wrong verdict
//! A "silent" verdict recorded against a terminal that later becomes capable of answering (rare —
//! e.g. reconfiguring tmux passthrough) self-heals after [`TTL_SECS`]. To force it sooner: delete
//! the cache file, or pin `workon.review.theme` to `dark`/`light` instead of `auto`.
//!
//! ## Failure posture
//! Every step here — resolving the cache dir, reading, parsing, writing — degrades silently to
//! "probe again": a missing/corrupt/unwritable cache file, no resolvable cache dir, or no
//! controlling tty (so no key) all fall through to [`crate::terminal_query::detect_auto_palette`]
//! running its probe exactly as it would with no cache at all. No new error type exists for this
//! module on purpose — there is nothing for a caller to react to.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// How long a "silent terminal" verdict stays trusted before the probe runs again.
const TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// Identifies one controlling terminal window for cache lookups — see the module doc's "Key"
/// section for the rationale behind each field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalKey {
    tty: String,
    term: String,
    term_program: String,
}

/// Build this launch's [`TerminalKey`] from the controlling tty and environment. `None` when no
/// standard fd is a tty to key against (stdio fully redirected, not unix) — callers treat that
/// the same as a cache miss, so an un-keyable launch just probes.
///
/// The device name is resolved from the first of stdin/stdout/stderr that `isatty` reports —
/// deliberately NOT from an fd opened on `/dev/tty`, even though the probe itself converses over
/// `/dev/tty`: on macOS, `ttyname_r` on such an fd returns the literal "/dev/tty", collapsing
/// every terminal window onto one shared cache key (see the module doc's "Key" section for the
/// poisoning this caused).
#[cfg(unix)]
pub(crate) fn terminal_key() -> Option<TerminalKey> {
    use std::ffi::CStr;

    let fd = [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .find(|&fd| unsafe { libc::isatty(fd) } == 1)?;
    let mut buf = [0 as std::os::raw::c_char; 256];
    if unsafe { libc::ttyname_r(fd, buf.as_mut_ptr(), buf.len()) } != 0 {
        return None;
    }
    // Safety: `ttyname_r` returning 0 guarantees `buf` holds a NUL-terminated string.
    let tty_path = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .ok()?
        .to_string();

    Some(TerminalKey {
        tty: tty_path,
        term: std::env::var("TERM").unwrap_or_default(),
        term_program: std::env::var("TERM_PROGRAM").unwrap_or_default(),
    })
}

#[cfg(not(unix))]
pub(crate) fn terminal_key() -> Option<TerminalKey> {
    None
}

/// Whether `key` has a live (unexpired) "silent" verdict cached. Any failure to resolve or read
/// the cache — no cache dir, missing/corrupt file — reports `false`, so the caller probes.
pub(crate) fn is_cached_silent(key: &TerminalKey) -> bool {
    match cache_path() {
        Some(path) => is_silent_at(&path, key, now_unix()),
        None => false,
    }
}

/// Record that `key`'s terminal timed out silent on this launch (which already paid the full
/// probe deadline). Best-effort: any failure to resolve the cache dir or write the file is
/// swallowed — a launch that can't cache its verdict just probes again next time, same as today.
pub(crate) fn record_silent(key: &TerminalKey) {
    if let Some(path) = cache_path() {
        record_silent_at(&path, key, now_unix());
    }
}

/// The cache file's location: `WORKON_REVIEW_PROBE_CACHE` when set (the PTY test suites' escape
/// hatch — see the module doc), else `dirs::cache_dir()/git-workon-review/silent-terminals.json`.
/// `None` when neither resolves (no `HOME`/`XDG_CACHE_HOME` equivalent for `dirs` to find).
fn cache_path() -> Option<PathBuf> {
    if let Ok(overridden) = std::env::var("WORKON_REVIEW_PROBE_CACHE") {
        return Some(PathBuf::from(overridden));
    }
    Some(
        dirs::cache_dir()?
            .join("git-workon-review")
            .join("silent-terminals.json"),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── The pure, path-injected core (unit-tested against a temp dir, never the real cache) ────────

/// `true` when `path`'s cache holds a `key` entry whose `timestamp` is within [`TTL_SECS`] of
/// `now`. A missing or corrupt file, or an all-expired/no-match set of entries, is `false`.
fn is_silent_at(path: &Path, key: &TerminalKey, now: u64) -> bool {
    read_entries(path)
        .iter()
        .any(|entry| entry_matches(entry, key) && entry_is_live(entry, now))
}

/// Write a fresh `key` verdict timestamped `now`, first dropping any expired entry (per
/// [`TTL_SECS`]) and any existing entry for `key` (a re-verdict replaces, not duplicates). Silent
/// on any I/O failure — see the module doc's "Failure posture".
fn record_silent_at(path: &Path, key: &TerminalKey, now: u64) {
    let mut entries = read_entries(path);
    entries.retain(|entry| entry_is_live(entry, now) && !entry_matches(entry, key));
    entries.push(json!({
        "tty": key.tty,
        "term": key.term,
        "term_program": key.term_program,
        "timestamp": now,
    }));
    write_entries(path, &entries);
}

fn entry_matches(entry: &Value, key: &TerminalKey) -> bool {
    entry.get("tty").and_then(Value::as_str) == Some(key.tty.as_str())
        && entry.get("term").and_then(Value::as_str) == Some(key.term.as_str())
        && entry.get("term_program").and_then(Value::as_str) == Some(key.term_program.as_str())
}

fn entry_is_live(entry: &Value, now: u64) -> bool {
    matches!(
        entry.get("timestamp").and_then(Value::as_u64),
        Some(ts) if now.saturating_sub(ts) < TTL_SECS
    )
}

/// Read the cache file into a list of raw JSON entries. Anything short of "a valid JSON array" —
/// a missing file, unreadable file, malformed JSON, or JSON that isn't an array — is treated as
/// an empty cache, never an error.
fn read_entries(path: &Path) -> Vec<Value> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Array(entries)) => entries,
        _ => Vec::new(),
    }
}

/// Best-effort pretty-printed write of `entries`, creating the parent directory if needed. Any
/// failure (read-only filesystem, missing permissions, ...) is swallowed.
fn write_entries(path: &Path, entries: &[Value]) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(&Value::Array(entries.to_vec())) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tty: &str) -> TerminalKey {
        TerminalKey {
            tty: tty.to_string(),
            term: "xterm-256color".to_string(),
            term_program: "iTerm.app".to_string(),
        }
    }

    fn cache_file() -> (assert_fs::TempDir, PathBuf) {
        let dir = assert_fs::TempDir::new().expect("temp dir");
        let path = dir.path().join("silent-terminals.json");
        (dir, path)
    }

    #[test]
    fn missing_file_is_not_silent() {
        let (_dir, path) = cache_file();
        assert!(!is_silent_at(&path, &key("/dev/ttys000"), 1_000));
    }

    #[test]
    fn record_then_lookup_round_trips() {
        let (_dir, path) = cache_file();
        let k = key("/dev/ttys000");
        record_silent_at(&path, &k, 1_000);
        assert!(
            is_silent_at(&path, &k, 1_500),
            "just-recorded verdict must be live"
        );
    }

    #[test]
    fn a_different_tty_term_or_term_program_is_a_miss() {
        let (_dir, path) = cache_file();
        record_silent_at(&path, &key("/dev/ttys000"), 1_000);

        assert!(
            !is_silent_at(&path, &key("/dev/ttys001"), 1_000),
            "different tty"
        );

        let mut different_term = key("/dev/ttys000");
        different_term.term = "screen".to_string();
        assert!(
            !is_silent_at(&path, &different_term, 1_000),
            "different $TERM"
        );

        let mut different_program = key("/dev/ttys000");
        different_program.term_program = "Apple_Terminal".to_string();
        assert!(
            !is_silent_at(&path, &different_program, 1_000),
            "different $TERM_PROGRAM — guards against tty-number recycling"
        );
    }

    #[test]
    fn an_expired_entry_is_ignored() {
        let (_dir, path) = cache_file();
        let k = key("/dev/ttys000");
        record_silent_at(&path, &k, 1_000);

        assert!(
            is_silent_at(&path, &k, 1_000 + TTL_SECS - 1),
            "just inside the TTL"
        );
        assert!(
            !is_silent_at(&path, &k, 1_000 + TTL_SECS),
            "exactly at the TTL boundary"
        );
        assert!(
            !is_silent_at(&path, &k, 1_000 + TTL_SECS + 1_000),
            "well past the TTL"
        );
    }

    #[test]
    fn writing_prunes_expired_entries() {
        let (_dir, path) = cache_file();
        let stale = key("/dev/ttys000");
        let fresh = key("/dev/ttys001");
        record_silent_at(&path, &stale, 1_000);
        // Advance well past the stale entry's TTL, then record a second (different) verdict —
        // the write must drop the stale entry rather than accumulate it forever.
        record_silent_at(&path, &fresh, 1_000 + TTL_SECS + 1);

        let entries = read_entries(&path);
        assert_eq!(
            entries.len(),
            1,
            "the expired entry must be pruned on write"
        );
        assert!(entry_matches(&entries[0], &fresh));
    }

    #[test]
    fn re_recording_the_same_key_replaces_rather_than_duplicates() {
        let (_dir, path) = cache_file();
        let k = key("/dev/ttys000");
        record_silent_at(&path, &k, 1_000);
        record_silent_at(&path, &k, 2_000);

        let entries = read_entries(&path);
        assert_eq!(entries.len(), 1, "a re-verdict must replace, not duplicate");
        assert_eq!(
            entries[0].get("timestamp").and_then(Value::as_u64),
            Some(2_000)
        );
    }

    #[test]
    fn a_corrupt_file_is_tolerated_and_overwritten() {
        let (_dir, path) = cache_file();
        std::fs::write(&path, b"not json at all { [").expect("write garbage");

        assert!(
            !is_silent_at(&path, &key("/dev/ttys000"), 1_000),
            "corrupt file must read back as no cache, not a crash"
        );

        // Recovery: a subsequent write must succeed and be readable, proving the corrupt file
        // doesn't wedge the cache permanently.
        record_silent_at(&path, &key("/dev/ttys000"), 1_000);
        assert!(is_silent_at(&path, &key("/dev/ttys000"), 1_000));
    }

    #[test]
    fn a_missing_cache_directory_is_created_on_write() {
        let dir = assert_fs::TempDir::new().expect("temp dir");
        let path = dir.path().join("nested").join("silent-terminals.json");
        record_silent_at(&path, &key("/dev/ttys000"), 1_000);
        assert!(path.exists(), "write must create missing parent dirs");
    }
}
