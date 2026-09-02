//! Anchored-annotation store shared by review comments and walkthrough content
//! (`git-workon-review`'s comment threads and the explain-diff-style tour/chapter prose).
//!
//! One substrate, two uses (ADR-039): a comment is `AnnotationKind::Comment`; a walkthrough
//! stop is `AnnotationKind::TourStop` ordered by `(tour, seq)`; a chapter is per-changeset
//! prose, `AnnotationKind::Chapter`. All three share one table, one anchoring scheme, and one
//! store API.
//!
//! This crate is serde-free and git2-free: [`store::AnnotationStore`] takes a `commondir`
//! path (the caller resolves it, e.g. via `git2::Repository::commondir()`), and the types
//! here are plain structs with rusqlite row mapping. JSON only enters at the MCP boundary
//! (a later slice, hosted as a second bin target in this crate — see ADR-039).
//!
//! ## Status
//!
//! Slice 1 (this crate's scaffold): types, schema, [`store::AnnotationStore`], and the pure
//! resolver in [`anchor`]. Nothing here is wired into `git-workon-review` yet.

pub mod anchor;
pub mod error;
mod schema;
pub mod store;

pub use error::{AnnotationsError, Result};

/// Identity of the changeset an annotation is anchored to: branch name plus whether it names
/// the uncommitted layer. Mirrors `git-workon-review`'s `app::ChangesetIdentity` (name alone
/// is ambiguous — the uncommitted layer is named after the current branch, so it collides
/// with that branch's committed changeset without this flag).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangesetKey {
    name: String,
    uncommitted: bool,
}

impl ChangesetKey {
    /// Build a key naming `name`'s committed changeset (`uncommitted: false`) or its
    /// uncommitted layer (`uncommitted: true`).
    pub fn new(name: impl Into<String>, uncommitted: bool) -> Self {
        Self {
            name: name.into(),
            uncommitted,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn uncommitted(&self) -> bool {
        self.uncommitted
    }
}

/// What an annotation is for. A walkthrough tour is annotations sharing a `tour` name,
/// ordered by `seq`; a chapter is per-changeset prose. See ADR-039.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationKind {
    Comment,
    TourStop,
    Chapter,
}

impl AnnotationKind {
    fn as_str(self) -> &'static str {
        match self {
            AnnotationKind::Comment => "comment",
            AnnotationKind::TourStop => "tour_stop",
            AnnotationKind::Chapter => "chapter",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "comment" => Some(AnnotationKind::Comment),
            "tour_stop" => Some(AnnotationKind::TourStop),
            "chapter" => Some(AnnotationKind::Chapter),
            _ => None,
        }
    }
}

/// Persisted lifecycle state. `Orphaned` (an anchor that no longer resolves) is deliberately
/// NOT here — it's derived per load by [`anchor::resolve`], not persisted, so a discarded
/// edit that restores the original content un-orphans the annotation for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Open,
    Resolved,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Resolved => "resolved",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Status::Open),
            "resolved" => Some(Status::Resolved),
            _ => None,
        }
    }
}

/// How an anchor resolved against the current file content this load. Derived by
/// [`anchor::resolve`]; never persisted (only the anchor's captured target/context is).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchoring {
    /// The stored line number still holds the stored target text and context.
    Exact,
    /// The target text was found elsewhere; `from` is the line number the anchor was
    /// captured at, for a "moved from line N" UI hint.
    Shifted { from: u32 },
    /// No occurrence of the target text (exact or whitespace-tolerant) resolved with enough
    /// context confidence. Renders as "unanchored", never silently wrong.
    Orphaned,
}

/// A captured location: the target line's text plus up to 3 lines of context each way, over
/// one side (old or new) of one file. `before`/`after` are top-to-bottom reading order
/// (`before[0]` is furthest from the target, `after.last()` is furthest); each holds 0-3
/// lines, fewer at file edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub path: String,
    /// Which side of the diff the anchor targets — the new-file side (`true`) or the
    /// old-file side (`false`). Resolution is per (file, role) view: see the gotcha in
    /// ADR-039 about `FileView::load`'s role-dependent "new side".
    pub new_side: bool,
    pub lineno: u32,
    /// End of the anchored span for a multi-line selection; equal to `lineno` for a
    /// single-line anchor. The resolver only tracks `target`/context for `lineno` — callers
    /// recompute the span length against the resolved line.
    pub end_lineno: u32,
    pub target: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// One annotation: a comment, tour stop, or chapter. `anchor` is `None` for a chapter (prose
/// scoped to the whole changeset, not a line) and for a reply (a reply inherits its parent's
/// anchor implicitly; storing it again would just drift out of sync on re-resolve).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub uid: String,
    pub kind: AnnotationKind,
    pub status: Status,
    pub parent_uid: Option<String>,
    pub changeset: ChangesetKey,
    pub anchor: Option<Anchor>,
    pub body: String,
    pub author: String,
    pub tour: Option<String>,
    pub seq: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Fields for a new top-level annotation ([`store::AnnotationStore::insert`]). `uid`,
/// `status` (always starts `Open`), `created_at`, and `updated_at` are assigned by the
/// store. Use [`store::AnnotationStore::reply`] for replies, which take a `parent_uid`
/// instead of an anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAnnotation {
    pub kind: AnnotationKind,
    pub changeset: ChangesetKey,
    pub anchor: Option<Anchor>,
    pub body: String,
    pub author: String,
    pub tour: Option<String>,
    pub seq: Option<i64>,
}

/// The store's write-visibility fingerprint (`store::AnnotationStore::fingerprint`). The TUI
/// watcher polls this to decide whether to reload; it changes only when some OTHER
/// connection committed a write (see the `PRAGMA data_version` note on `fingerprint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub data_version: i64,
    pub revision: i64,
}
