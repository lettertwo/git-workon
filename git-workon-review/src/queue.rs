//! FIFO staging queue (trap 4): callers enqueue [`StagingOp`]s, [`StagingQueue::pump`] runs the
//! head op synchronously against the live index. Runtime-agnostic per the M2 design decision —
//! no tokio, no owned thread; the caller (M4's TUI event loop) decides when to pump.
//!
//! ## The stale-snapshot trap
//!
//! An op MUST resolve its direction (stage vs. unstage, etc.) by querying the LIVE index
//! INSIDE `run` — via [`path_has_staged_changes`] or equivalent — never from a snapshot taken
//! at enqueue time. Two ops enqueued back-to-back for the same path (e.g. a user double-toggling
//! a file before the first op has run) both see whatever the index looks like when THEY run, not
//! when they were queued; a snapshot-at-enqueue implementation would have both ops decide the
//! same direction and the second would silently no-op instead of round-tripping the toggle.
//!
//! ## In-flight accounting
//!
//! [`OpContext::queue_len`] includes the op currently running — `pump` computes it from the
//! queue BEFORE popping the head op, and only removes the op once `run` returns (success,
//! failure, or panic). This lets an op (or a refresh gate reading [`StagingQueue::len`]) see
//! "at least one more op is in flight" for its own duration, not just for the ops still waiting
//! behind it. See `refresh.rs` for the consumer: `RefreshCoordinator::note_index_event` refuses
//! to schedule a refresh while this count is nonzero.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use git2::{Repository, StatusOptions};

use crate::apply::Applier;
use crate::error::ApplyError;

/// Identifies a queued operation, assigned in enqueue order.
pub type OpId = u64;

/// What a running [`StagingOp`] needs to do its work and to make its own live-index checks.
pub struct OpContext<'a> {
    pub repo: &'a Repository,
    pub applier: &'a dyn Applier,
    /// Number of ops in the queue, INCLUDING the one currently running.
    pub queue_len: usize,
}

/// Whether `path` currently has any staged (index-side) changes, resolved against the LIVE
/// index at call time — the direction-resolution primitive ops must call from inside `run`
/// (never at enqueue time; see the module doc's stale-snapshot trap).
pub fn path_has_staged_changes(repo: &Repository, path: &str) -> Result<bool, ApplyError> {
    let mut opts = StatusOptions::new();
    opts.pathspec(path);
    // Without this, `pathspec` treats `path` as a glob — a path containing glob metacharacters
    // (e.g. `app/[slug]/page.tsx`) then never matches itself, and this always resolves "no
    // staged changes" regardless of the real index state.
    opts.disable_pathspec_match(true);
    opts.include_untracked(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let index_bits = git2::Status::INDEX_NEW
        | git2::Status::INDEX_MODIFIED
        | git2::Status::INDEX_DELETED
        | git2::Status::INDEX_RENAMED
        | git2::Status::INDEX_TYPECHANGE;
    Ok(statuses
        .iter()
        .any(|entry| entry.status().intersects(index_bits)))
}

/// A single queued staging action. Implementations resolve their own direction from the live
/// index inside `run` (see module docs) rather than trusting anything decided at enqueue time.
pub trait StagingOp: Send {
    fn run(&mut self, ctx: &OpContext<'_>) -> Result<(), ApplyError>;
}

/// The result of running one queued op.
#[derive(Debug)]
pub enum OpOutcome {
    Completed(OpId),
    Failed(OpId, ApplyError),
    Panicked(OpId),
}

/// Injectable sleep for retry backoff — a type alias keeps `StagingQueue`'s field type simple
/// enough for clippy's `type_complexity` lint (a bare `Box<dyn FnMut(Duration)>` field would
/// otherwise read as a false positive candidate once combined with the rest of the struct).
type SleepFn = Box<dyn FnMut(Duration)>;

/// Runtime-agnostic FIFO queue of staging operations (trap 4). Only the head op ever runs;
/// [`StagingQueue::pump`] runs it synchronously to completion (including its one retry, if
/// index-lock contention is hit) before removing it.
pub struct StagingQueue {
    queue: VecDeque<(OpId, Box<dyn StagingOp>)>,
    next_id: OpId,
    retry_delay: Duration,
    sleep: SleepFn,
}

impl Default for StagingQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl StagingQueue {
    /// A queue with the real 100ms retry delay and `std::thread::sleep`.
    pub fn new() -> Self {
        Self::with_retry(Duration::from_millis(100), std::thread::sleep)
    }

    /// A queue with an injectable retry delay and sleep function — tests pass `Duration::ZERO`
    /// and a counting closure so the retry-once policy can be asserted without actually
    /// sleeping.
    pub fn with_retry(delay: Duration, sleep: impl FnMut(Duration) + 'static) -> Self {
        Self {
            queue: VecDeque::new(),
            next_id: 0,
            retry_delay: delay,
            sleep: Box::new(sleep),
        }
    }

    /// Enqueue `op` at the tail, returning its assigned id.
    pub fn enqueue(&mut self, op: impl StagingOp + 'static) -> OpId {
        let id = self.next_id;
        self.next_id += 1;
        self.queue.push_back((id, Box::new(op)));
        id
    }

    /// Run the head op synchronously against `repo`/`applier`. Returns `None` if the queue is
    /// empty.
    ///
    /// The op is removed from the queue only AFTER it finishes — `queue_len` is computed from
    /// the queue's current length (including the head op itself) before the op runs, so an op
    /// can see "how many ops, including me, are outstanding" via [`OpContext::queue_len`].
    ///
    /// On a lock-contention error ([`crate::apply::is_lock_contention`]), the op is retried
    /// exactly once after `sleep(retry_delay)`; a second lock failure yields
    /// `Failed(id, ApplyError::IndexLocked { attempts: 2 })` rather than propagating the
    /// original error, since by that point the queue has given up and the notable fact is the
    /// retry count, not which particular lock error surfaced. Non-lock errors fail immediately,
    /// no retry.
    pub fn pump(&mut self, repo: &Repository, applier: &dyn Applier) -> Option<OpOutcome> {
        let queue_len = self.queue.len();
        let (id, mut op) = self.queue.pop_front()?;

        let ctx = OpContext {
            repo,
            applier,
            queue_len,
        };

        // SAFETY/soundness note (plan risk #9): `&Repository` isn't `UnwindSafe`, so `run`
        // can't be called under `catch_unwind` without asserting it. This is sound because the
        // op (and the `ctx` borrowing `repo`) is discarded immediately after a panic is caught
        // — nothing observes `op`'s or `repo`'s state through a broken invariant afterward; the
        // queue only ever looks at its own `VecDeque`, which is untouched by the panic.
        let result = catch_unwind(AssertUnwindSafe(|| op.run(&ctx)));

        let outcome = match result {
            Ok(Ok(())) => OpOutcome::Completed(id),
            Ok(Err(e)) if crate::apply::is_lock_contention(&e) => {
                (self.sleep)(self.retry_delay);
                let retry_ctx = OpContext {
                    repo,
                    applier,
                    queue_len,
                };
                match catch_unwind(AssertUnwindSafe(|| op.run(&retry_ctx))) {
                    Ok(Ok(())) => OpOutcome::Completed(id),
                    // Only a SECOND lock-contention error collapses into the "gave up
                    // retrying" outcome — any other error on retry is its own distinct
                    // failure and must propagate as itself, not be swallowed under a
                    // misleading `IndexLocked` label.
                    Ok(Err(e)) if crate::apply::is_lock_contention(&e) => {
                        OpOutcome::Failed(id, ApplyError::IndexLocked { attempts: 2 })
                    }
                    Ok(Err(e)) => OpOutcome::Failed(id, e),
                    Err(_) => OpOutcome::Panicked(id),
                }
            }
            Ok(Err(e)) => OpOutcome::Failed(id, e),
            Err(_) => OpOutcome::Panicked(id),
        };

        Some(outcome)
    }

    /// Pump until the queue is empty, collecting each op's outcome in order.
    pub fn drain(&mut self, repo: &Repository, applier: &dyn Applier) -> Vec<OpOutcome> {
        let mut outcomes = Vec::new();
        while let Some(outcome) = self.pump(repo, applier) {
            outcomes.push(outcome);
        }
        outcomes
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use git_workon_fixture::prelude::*;

    use super::*;
    use crate::apply::CliApplier;
    use crate::file_ops::unstage_file;

    /// A fake op that records its id into a shared log on run and always succeeds. Uses
    /// `Arc<Mutex<_>>` (not `Rc<RefCell<_>>`) because [`StagingOp`] requires `Send`.
    struct RecordingOp {
        id_slot: OpId,
        log: Arc<Mutex<Vec<OpId>>>,
    }

    impl StagingOp for RecordingOp {
        fn run(&mut self, _ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            self.log.lock().unwrap().push(self.id_slot);
            Ok(())
        }
    }

    /// An op that asserts `ctx.queue_len` equals an expected value the first time it runs.
    struct AssertQueueLenOp {
        expected: usize,
        seen: Arc<Mutex<Option<usize>>>,
    }

    impl StagingOp for AssertQueueLenOp {
        fn run(&mut self, ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            *self.seen.lock().unwrap() = Some(ctx.queue_len);
            assert_eq!(
                ctx.queue_len, self.expected,
                "op did not see itself counted in queue_len while running"
            );
            Ok(())
        }
    }

    fn lock_error() -> ApplyError {
        ApplyError::Git(git2::Error::new(
            git2::ErrorCode::Locked,
            git2::ErrorClass::Index,
            "locked",
        ))
    }

    /// A fake op that fails with a lock error for its first `attempts_to_fail` calls, then
    /// succeeds.
    struct FlakyLockOp {
        attempts_to_fail: u32,
        calls: u32,
    }

    impl StagingOp for FlakyLockOp {
        fn run(&mut self, _ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            self.calls += 1;
            if self.calls <= self.attempts_to_fail {
                Err(lock_error())
            } else {
                Ok(())
            }
        }
    }

    /// A fake op that always fails with a non-lock error.
    struct AlwaysNonLockErrorOp;

    impl StagingOp for AlwaysNonLockErrorOp {
        fn run(&mut self, _ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            Err(ApplyError::GitSpawn(std::io::Error::other("boom")))
        }
    }

    /// A fake op that fails with a lock error on its first call, then a DIFFERENT (non-lock)
    /// error on every call after — the shape that catches the retry-arm bug: a second failure
    /// that isn't itself a lock error must surface as itself, not get relabeled `IndexLocked`.
    struct FlakyThenDifferentErrorOp {
        calls: u32,
    }

    impl StagingOp for FlakyThenDifferentErrorOp {
        fn run(&mut self, _ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            self.calls += 1;
            if self.calls == 1 {
                Err(lock_error())
            } else {
                Err(ApplyError::GitSpawn(std::io::Error::other("boom")))
            }
        }
    }

    /// An op whose `run` panics unconditionally.
    struct PanickingOp;

    impl StagingOp for PanickingOp {
        fn run(&mut self, _ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            panic!("staging op exploded");
        }
    }

    /// Toggles staged/unstaged state of `path` by resolving direction from the LIVE index
    /// inside `run` — proves ops must not cache the direction at enqueue time (trap 4's
    /// stale-snapshot bug).
    struct ToggleOp {
        path: &'static str,
    }

    impl StagingOp for ToggleOp {
        fn run(&mut self, ctx: &OpContext<'_>) -> Result<(), ApplyError> {
            if path_has_staged_changes(ctx.repo, self.path)? {
                unstage_file(ctx.repo, self.path)
            } else {
                let mut index = ctx.repo.index()?;
                index.add_path(std::path::Path::new(self.path))?;
                index.write()?;
                Ok(())
            }
        }
    }

    fn fresh_queue() -> (StagingQueue, Arc<Mutex<u32>>) {
        let sleep_calls = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&sleep_calls);
        let queue = StagingQueue::with_retry(Duration::ZERO, move |_| {
            *counter.lock().unwrap() += 1;
        });
        (queue, sleep_calls)
    }

    #[test]
    fn fifo_order_preserved_through_drain() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let log = Arc::new(Mutex::new(Vec::new()));
        let (mut queue, _sleep_calls) = fresh_queue();
        let a = queue.enqueue(RecordingOp {
            id_slot: 0,
            log: Arc::clone(&log),
        });
        let b = queue.enqueue(RecordingOp {
            id_slot: 1,
            log: Arc::clone(&log),
        });
        let c = queue.enqueue(RecordingOp {
            id_slot: 2,
            log: Arc::clone(&log),
        });

        let outcomes = queue.drain(repo, &CliApplier);

        assert_eq!(*log.lock().unwrap(), vec![a, b, c]);
        assert!(matches!(outcomes[0], OpOutcome::Completed(id) if id == a));
        assert!(matches!(outcomes[1], OpOutcome::Completed(id) if id == b));
        assert!(matches!(outcomes[2], OpOutcome::Completed(id) if id == c));
    }

    #[test]
    fn head_op_sees_itself_counted_in_queue_len() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let seen = Arc::new(Mutex::new(None));
        let (mut queue, _sleep_calls) = fresh_queue();
        // Two ops queued: the head op should see queue_len == 2 (itself + the one behind it).
        queue.enqueue(AssertQueueLenOp {
            expected: 2,
            seen: Arc::clone(&seen),
        });
        queue.enqueue(RecordingOp {
            id_slot: 99,
            log: Arc::new(Mutex::new(Vec::new())),
        });

        let outcome = queue.pump(repo, &CliApplier).expect("pump");
        assert!(matches!(outcome, OpOutcome::Completed(_)));
        assert_eq!(*seen.lock().unwrap(), Some(2));
    }

    /// Regression: `path_has_staged_changes` must match a literal path even when it contains
    /// glob metacharacters (`[...]`) — without `disable_pathspec_match`, `StatusOptions::pathspec`
    /// treats `path` as a glob, and a bracketed path like `app/[slug]/page.tsx` never matches
    /// itself, so this always resolved "no staged changes" regardless of the real index state.
    #[test]
    fn path_has_staged_changes_matches_bracketed_path() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .staged_file("app/[slug]/page.tsx", "content\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        assert!(
            path_has_staged_changes(repo, "app/[slug]/page.tsx").expect("path_has_staged_changes"),
            "expected the bracketed path's staged entry to be found"
        );
    }

    /// Two toggles queued for the same path, starting unstaged: if an implementation resolved
    /// "stage" once at enqueue time and reused it, both ops would stage, leaving the file
    /// staged. Because `ToggleOp::run` calls `path_has_staged_changes` against the LIVE index
    /// each time it actually runs, the first toggle stages it and the second (seeing the
    /// now-staged index) unstages it back — the file round-trips to unstaged, exactly the
    /// trap-4 stale-snapshot bug this test guards against.
    #[test]
    fn two_queued_toggles_resolve_from_live_index_and_round_trip() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .untracked_file("f.txt", "hello\n")
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let (mut queue, _sleep_calls) = fresh_queue();
        queue.enqueue(ToggleOp { path: "f.txt" });
        queue.enqueue(ToggleOp { path: "f.txt" });

        let outcomes = queue.drain(repo, &CliApplier);

        assert!(outcomes
            .iter()
            .all(|o| matches!(o, OpOutcome::Completed(_))));
        fixture.assert(predicate::repo::has_untracked_file("f.txt"));
    }

    #[test]
    fn lock_failure_then_success_retries_exactly_once() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let (mut queue, sleep_calls) = fresh_queue();
        queue.enqueue(FlakyLockOp {
            attempts_to_fail: 1,
            calls: 0,
        });

        let outcome = queue.pump(repo, &CliApplier).expect("pump");
        assert!(matches!(outcome, OpOutcome::Completed(_)));
        assert_eq!(
            *sleep_calls.lock().unwrap(),
            1,
            "expected exactly one retry sleep"
        );
    }

    #[test]
    fn lock_failure_twice_fails_with_index_locked_after_two_attempts() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let (mut queue, sleep_calls) = fresh_queue();
        queue.enqueue(FlakyLockOp {
            attempts_to_fail: 2,
            calls: 0,
        });

        let outcome = queue.pump(repo, &CliApplier).expect("pump");
        match outcome {
            OpOutcome::Failed(_, ApplyError::IndexLocked { attempts }) => {
                assert_eq!(attempts, 2);
            }
            other => panic!("expected Failed(IndexLocked{{attempts: 2}}), got {other:?}"),
        }
        assert_eq!(
            *sleep_calls.lock().unwrap(),
            1,
            "expected only one retry sleep, not one per attempt"
        );
    }

    /// Regression: a lock error on the first attempt followed by a DIFFERENT (non-lock) error
    /// on the retry must surface as that second error, not get collapsed into
    /// `IndexLocked{attempts: 2}` — the retry arm previously matched `Ok(Err(_))` unconditionally
    /// on the second attempt, swallowing whatever error actually occurred.
    #[test]
    fn lock_failure_then_different_error_propagates_that_error() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let (mut queue, sleep_calls) = fresh_queue();
        queue.enqueue(FlakyThenDifferentErrorOp { calls: 0 });

        let outcome = queue.pump(repo, &CliApplier).expect("pump");
        assert!(
            matches!(outcome, OpOutcome::Failed(_, ApplyError::GitSpawn(_))),
            "expected the retry's own GitSpawn error to propagate, got {outcome:?}"
        );
        assert_eq!(
            *sleep_calls.lock().unwrap(),
            1,
            "expected exactly one retry sleep (only the first attempt was a lock error)"
        );
    }

    #[test]
    fn non_lock_error_fails_immediately_without_retry() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let (mut queue, sleep_calls) = fresh_queue();
        queue.enqueue(AlwaysNonLockErrorOp);

        let outcome = queue.pump(repo, &CliApplier).expect("pump");
        assert!(matches!(
            outcome,
            OpOutcome::Failed(_, ApplyError::GitSpawn(_))
        ));
        assert_eq!(
            *sleep_calls.lock().unwrap(),
            0,
            "non-lock errors must not retry"
        );
    }

    #[test]
    fn panicking_op_is_contained_and_queue_remains_usable() {
        let fixture = FixtureBuilder::new()
            .config("core.autocrlf", "false")
            .worktree("main")
            .bare(true)
            .build()
            .expect("fixture build");
        let repo = fixture.repo().expect("repo");

        let log = Arc::new(Mutex::new(Vec::new()));
        let (mut queue, _sleep_calls) = fresh_queue();
        queue.enqueue(PanickingOp);
        // `id_slot` need not match the queue-assigned `OpId` (that's only known once
        // `enqueue` returns) — the log just needs one entry to prove this op ran.
        let following = queue.enqueue(RecordingOp {
            id_slot: 1,
            log: Arc::clone(&log),
        });

        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let first = queue.pump(repo, &CliApplier).expect("pump");
        std::panic::set_hook(prev_hook);

        assert!(matches!(first, OpOutcome::Panicked(_)));

        let second = queue.pump(repo, &CliApplier).expect("pump");
        assert!(matches!(second, OpOutcome::Completed(id) if id == following));
        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "expected the following op to run once"
        );
    }
}
