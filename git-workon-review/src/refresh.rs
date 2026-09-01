//! Refresh generation/livelock coordination (refresh echo suppression): a pure state machine
//! tracking which re-diff is the latest one requested, so a slow refresh that finishes after a newer one has
//! already started doesn't clobber fresher results.
//!
//! ## Interlock with `queue.rs`
//!
//! [`RefreshCoordinator::note_index_event`] refuses to schedule a refresh while
//! [`crate::queue::StagingQueue::len`] is nonzero (passed in as `staging_queue_len`) — a
//! refresh only makes sense once the queue has drained, since an in-flight staging op is about
//! to change the index again anyway (the live-index-staging-queue/refresh-echo-suppression
//! interlock).
//!
//! ## Staging-verbs wiring intent (forward-looking; not built here)
//!
//! A filesystem watcher will call [`RefreshCoordinator::note_index_event`] whenever `.git/index`
//! changes, using [`IndexSignature::read`] to build the signature. When it returns `true`, the
//! caller starts an async re-diff, calling [`RefreshCoordinator::begin`] before starting the
//! work and [`RefreshCoordinator::complete`] when it finishes.

use std::io;
use std::path::Path;

/// A cheap fingerprint of `.git/index`'s on-disk state (mtime + size), used to distinguish a
/// genuinely new index write from an echo of one this process just made itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSignature {
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub size: u64,
}

impl IndexSignature {
    /// Read the current signature of `<git_dir>/index`. A staging-verbs convenience for wiring a
    /// real filesystem watcher; this module's own tests use synthetic signatures (see `tests`
    /// below) since the coordinator's logic never inspects the fields itself, only compares
    /// whole signatures.
    pub fn read(git_dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(git_dir.join("index"))?;
        Ok(IndexSignature {
            mtime_sec: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            size: metadata.size(),
        })
    }
}

/// A ticket for one in-flight refresh, returned by [`RefreshCoordinator::begin`] and consumed by
/// [`RefreshCoordinator::complete`]. Carries the generation it was started at; fields are
/// private, only constructed by `begin`.
#[derive(Debug, Clone, Copy)]
pub struct RefreshTicket {
    generation: u64,
}

/// What a completing refresh should do with its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// This was the latest refresh started — commit its result.
    Commit,
    /// A newer refresh has started since this one began — discard this result, a fresher one
    /// is already on the way.
    Superseded,
}

/// Generation counter + last-seen index signature, implementing the refresh-echo-suppression
/// supersede/livelock invariants. See the module docs for the `queue.rs` interlock and the
/// staging-verbs wiring intent.
pub struct RefreshCoordinator {
    next_gen: u64,
    latest_started: u64,
    last_signature: Option<IndexSignature>,
}

impl Default for RefreshCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl RefreshCoordinator {
    pub fn new() -> Self {
        Self {
            next_gen: 1,
            latest_started: 0,
            last_signature: None,
        }
    }

    /// Whether an index-change event at `sig` should schedule a new refresh.
    ///
    /// `false` if `staging_queue_len > 0` — a staging op is still in flight, so the index is
    /// about to change again; wait for the queue to drain (the `queue.rs` interlock). `false` if
    /// `sig` matches `last_signature` — this is the echo of a write this process already
    /// accounted for (see `complete`'s doc comment for how that signature got recorded). `true`
    /// otherwise: a genuinely new, unseen index state with no staging op in flight.
    ///
    /// This method deliberately does NOT update `last_signature` itself — only `complete` does.
    /// If it recorded the signature here, a genuinely external change event would be "seen" and
    /// suppressed the moment it arrived, before any refresh even ran to observe it; worse, the
    /// signature that actually needs recording is the one a refresh completes with, at the
    /// specific point refresh echo suppression cares about (see `complete`), not the raw event
    /// that triggered the refresh in the first place. Comparisons live here; recording lives in
    /// `complete`.
    pub fn note_index_event(&mut self, sig: IndexSignature, staging_queue_len: usize) -> bool {
        if staging_queue_len > 0 {
            return false;
        }
        if self.last_signature == Some(sig) {
            return false;
        }
        true
    }

    /// Start a new refresh, returning a ticket carrying its generation. The generation becomes
    /// `self`'s `latest_started`, so any ticket from an earlier `begin` call will be superseded
    /// at `complete` time.
    pub fn begin(&mut self) -> RefreshTicket {
        let generation = self.next_gen;
        self.next_gen += 1;
        self.latest_started = generation;
        RefreshTicket { generation }
    }

    /// Complete a refresh. `sig_at_completion` is the index signature observed AT THE MOMENT the
    /// refresh finished (not when it started) — the refresh's own diffing work may itself have
    /// touched the index's stat cache, so the signature must be captured fresh here.
    ///
    /// INVARIANT (the livelock fix): `sig_at_completion` is recorded into `last_signature`
    /// UNCONDITIONALLY, before the generation check decides `Commit` vs `Superseded` — including
    /// on the LOSING (`Superseded`) path. If a losing completion skipped recording its signature,
    /// the stat-cache rewrite its own diffing caused would arrive at the watcher as an
    /// apparently-new, never-seen index event, `note_index_event` would say "schedule a
    /// refresh" for it, and under a storm of staging changes each losing refresh's echo would
    /// re-trigger another refresh forever. Recording unconditionally — even on the losing path —
    /// means that specific echo is always exactly the "already seen" signature, so it gets
    /// suppressed instead of livelocking the refresh loop.
    pub fn complete(
        &mut self,
        ticket: RefreshTicket,
        sig_at_completion: IndexSignature,
    ) -> Completion {
        self.last_signature = Some(sig_at_completion);
        if ticket.generation == self.latest_started {
            Completion::Commit
        } else {
            Completion::Superseded
        }
    }
}

#[cfg(test)]
mod tests {
    use git_workon_fixture::prelude::*;

    use super::*;

    fn sig(n: i64) -> IndexSignature {
        IndexSignature {
            mtime_sec: n,
            mtime_nsec: 0,
            size: n as u64,
        }
    }

    #[test]
    fn single_begin_complete_pair_commits() {
        let mut coordinator = RefreshCoordinator::new();
        let ticket = coordinator.begin();
        assert_eq!(coordinator.complete(ticket, sig(1)), Completion::Commit);
    }

    #[test]
    fn last_writer_wins_among_two_in_flight_refreshes() {
        let mut coordinator = RefreshCoordinator::new();
        let a = coordinator.begin();
        let b = coordinator.begin();

        assert_eq!(coordinator.complete(a, sig(1)), Completion::Superseded);
        assert_eq!(coordinator.complete(b, sig(2)), Completion::Commit);
    }

    /// THE LIVELOCK INVARIANT: a losing (`Superseded`) completion still records its signature,
    /// so the echo of its own write is suppressed by a subsequent `note_index_event` rather than
    /// re-triggering another refresh — see `complete`'s doc comment for the full story.
    #[test]
    fn losing_completion_still_records_signature_to_suppress_echo() {
        let mut coordinator = RefreshCoordinator::new();
        let a = coordinator.begin();
        let b = coordinator.begin();

        assert_eq!(coordinator.complete(a, sig(2)), Completion::Superseded);
        assert!(
            !coordinator.note_index_event(sig(2), 0),
            "the losing completion's own write should be suppressed as an echo"
        );

        assert_eq!(coordinator.complete(b, sig(3)), Completion::Commit);
        assert!(
            !coordinator.note_index_event(sig(3), 0),
            "the winning completion's own write should also be suppressed as an echo"
        );
        assert!(
            coordinator.note_index_event(sig(4), 0),
            "a genuinely new external change must still schedule a refresh"
        );
    }

    #[test]
    fn queue_gate_blocks_refresh_until_drained() {
        let mut coordinator = RefreshCoordinator::new();

        assert!(
            !coordinator.note_index_event(sig(5), 1),
            "a nonzero staging queue must block scheduling, even for a new signature"
        );
        assert!(
            coordinator.note_index_event(sig(5), 0),
            "the same signature should schedule once the queue has drained"
        );
    }

    #[test]
    fn index_signature_read_reads_a_real_index_file() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("f.txt", "hello\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let sig = IndexSignature::read(repo.path()).expect("read index signature");
        assert!(sig.size > 0, "expected a nonzero index file size");
    }
}
