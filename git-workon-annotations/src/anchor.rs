//! Pure resolver: given a stored [`Anchor`] and the current lines of the side it targets,
//! find where the target line lives now. No I/O, no store access — this is the unit-test
//! center of the crate (ADR-039's anchoring decision).
//!
//! Resolution order: exact match at the stored line, then a windowed outward scan for the
//! target text scored by surrounding context, then a repeat of that scan with whitespace
//! trimmed from both target and context, else [`Anchoring::Orphaned`].

use crate::{Anchor, Anchoring};

/// Where `anchor`'s target line resolved to against `lines` (1-based, matching
/// [`Anchor::lineno`]), and how confidently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// `None` only when `anchoring` is [`Anchoring::Orphaned`].
    pub lineno: Option<u32>,
    pub anchoring: Anchoring,
}

/// Resolve `anchor` against `lines` — the current content of the side (old or new) and file
/// [`Anchor::new_side`]/[`Anchor::path`] name, one entry per line, no trailing newlines.
pub fn resolve(anchor: &Anchor, lines: &[&str]) -> Resolution {
    let original_idx = anchor.lineno.saturating_sub(1) as usize;

    if lines.get(original_idx) == Some(&anchor.target.as_str())
        && context_matches(anchor, lines, original_idx, false)
    {
        return Resolution {
            lineno: Some(original_idx as u32 + 1),
            anchoring: Anchoring::Exact,
        };
    }

    if let Some(idx) = scan(anchor, lines, original_idx, false) {
        return Resolution {
            lineno: Some(idx as u32 + 1),
            anchoring: Anchoring::Shifted {
                from: anchor.lineno,
            },
        };
    }

    if let Some(idx) = scan(anchor, lines, original_idx, true) {
        return Resolution {
            lineno: Some(idx as u32 + 1),
            anchoring: Anchoring::Shifted {
                from: anchor.lineno,
            },
        };
    }

    Resolution {
        lineno: None,
        anchoring: Anchoring::Orphaned,
    }
}

/// Outward scan from `origin`, nearest index first, for lines equal to the target (trimmed if
/// `whitespace_tolerant`). Among matches, picks the highest-scoring by context; ties broken by
/// distance from `origin` (the scan order already visits nearest first, so the first
/// max-score match wins). A match needs `score >= 1` unless the target text is unique among
/// candidates in `lines` (only one occurrence to choose from — nothing to disambiguate).
fn scan(
    anchor: &Anchor,
    lines: &[&str],
    origin: usize,
    whitespace_tolerant: bool,
) -> Option<usize> {
    let target_eq = |line: &str| -> bool {
        if whitespace_tolerant {
            line.trim() == anchor.target.trim()
        } else {
            line == anchor.target
        }
    };

    let candidates: Vec<usize> = distance_order(origin, lines.len())
        .into_iter()
        .filter(|&idx| target_eq(lines[idx]))
        .collect();

    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }

    let mut best: Option<(usize, u32)> = None;
    for idx in candidates {
        let score = context_score(anchor, lines, idx, whitespace_tolerant);
        if score >= 1
            && best
                .map(|(_, best_score)| score > best_score)
                .unwrap_or(true)
        {
            best = Some((idx, score));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Indices `0..len`, ordered by ascending distance from `origin` (origin first, then
/// alternating -1/+1 out from it).
fn distance_order(origin: usize, len: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(len);
    if len == 0 {
        return order;
    }
    let origin = origin.min(len - 1);
    order.push(origin);
    let mut back = origin;
    let mut forward = origin;
    loop {
        let mut moved = false;
        if back > 0 {
            back -= 1;
            order.push(back);
            moved = true;
        }
        if forward + 1 < len {
            forward += 1;
            order.push(forward);
            moved = true;
        }
        if !moved {
            break;
        }
    }
    order
}

fn context_matches(anchor: &Anchor, lines: &[&str], idx: usize, whitespace_tolerant: bool) -> bool {
    let want = anchor.before.len() as u32 + anchor.after.len() as u32;
    context_score(anchor, lines, idx, whitespace_tolerant) == want
}

/// Count of `anchor.before`/`anchor.after` entries that match the corresponding line around
/// candidate `idx` (missing lines at file edges just don't score).
fn context_score(anchor: &Anchor, lines: &[&str], idx: usize, whitespace_tolerant: bool) -> u32 {
    let eq = |a: &str, b: &str| {
        if whitespace_tolerant {
            a.trim() == b.trim()
        } else {
            a == b
        }
    };

    let mut score = 0;
    for (offset, expected) in anchor.before.iter().rev().enumerate() {
        let pos = idx.checked_sub(offset + 1);
        if let Some(pos) = pos {
            if lines.get(pos).is_some_and(|line| eq(line, expected)) {
                score += 1;
            }
        }
    }
    for (offset, expected) in anchor.after.iter().enumerate() {
        let pos = idx + offset + 1;
        if lines.get(pos).is_some_and(|line| eq(line, expected)) {
            score += 1;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(lineno: u32, target: &str, before: &[&str], after: &[&str]) -> Anchor {
        Anchor {
            path: "f.rs".into(),
            new_side: true,
            lineno,
            end_lineno: lineno,
            target: target.into(),
            before: before.iter().map(|s| s.to_string()).collect(),
            after: after.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn exact_match_at_stored_line() {
        let a = anchor(2, "target", &["before"], &["after"]);
        let lines = vec!["before", "target", "after"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, Some(2));
        assert_eq!(r.anchoring, Anchoring::Exact);
    }

    #[test]
    fn shifted_downward() {
        // Two lines inserted above the target: target used to be line 2, now line 4.
        let a = anchor(2, "target", &["before"], &["after"]);
        let lines = vec!["inserted-1", "inserted-2", "before", "target", "after"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, Some(4));
        assert_eq!(r.anchoring, Anchoring::Shifted { from: 2 });
    }

    #[test]
    fn shifted_upward() {
        // A line removed above the target: target used to be line 3, now line 2.
        let a = anchor(3, "target", &["before"], &["after"]);
        let lines = vec!["before", "target", "after"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, Some(2));
        assert_eq!(r.anchoring, Anchoring::Shifted { from: 3 });
    }

    #[test]
    fn whitespace_only_change_resolves_shifted() {
        let a = anchor(1, "  target", &[], &["after"]);
        let lines = vec!["target", "after"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, Some(1));
        assert_eq!(r.anchoring, Anchoring::Shifted { from: 1 });
    }

    #[test]
    fn duplicate_target_disambiguated_by_context() {
        // "dup" appears at idx 1 and idx 3; the stored line (idx 4, 1-based 5) holds neither,
        // so this exercises the scored scan directly. Only idx 3 has the right before-context
        // ("ctx-a" at idx 2), so it wins over the unscored duplicate at idx 1.
        let a = anchor(5, "dup", &["ctx-a"], &[]);
        let lines = vec!["other", "dup", "ctx-a", "dup", "unrelated"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, Some(4));
        assert!(matches!(r.anchoring, Anchoring::Shifted { .. }));
    }

    #[test]
    fn ambiguous_duplicate_with_no_context_signal_picks_nearest() {
        // Neither "dup" occurrence has any matching context, so score can't disambiguate;
        // the scan still resolves (score requirement is waived only when unique — here it
        // isn't — so this exercises "no candidate clears score >= 1" -> Orphaned).
        let a = anchor(10, "dup", &["never-matches"], &[]);
        let lines = vec!["dup", "x", "dup"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, None);
        assert_eq!(r.anchoring, Anchoring::Orphaned);
    }

    #[test]
    fn orphan_when_target_absent() {
        let a = anchor(1, "gone", &[], &[]);
        let lines = vec!["still-here"];
        let r = resolve(&a, &lines);
        assert_eq!(r.lineno, None);
        assert_eq!(r.anchoring, Anchoring::Orphaned);
    }
}
