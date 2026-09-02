//! [`AnnotationStore`]: the sqlite-backed CRUD/query API over the `annotation` table (ADR-039).
//!
//! The database lives at `<commondir>/workon-review/annotations.db` — `commondir`, not
//! `repo.path()`, so every worktree of a repo shares one store (the same discipline
//! `git-workon-lib`'s graphite reader uses for `.graphite_metadata.db`). This crate doesn't
//! resolve `commondir` itself (no git2 dep); callers pass it in, typically from
//! `git2::Repository::commondir()`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};

use crate::error::{AnnotationsError, Result};
use crate::{Anchor, Annotation, AnnotationKind, ChangesetKey, Fingerprint, NewAnnotation, Status};

/// One tour stop or chapter to write as part of [`AnnotationStore::put_walkthrough`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TourStop {
    pub anchor: Anchor,
    pub body: String,
    pub author: String,
    pub seq: i64,
}

/// A full walkthrough write: an optional per-changeset chapter plus its ordered tour stops,
/// applied in one transaction (a partial write — chapter with no stops, or stops without a
/// chapter — would leave the TUI's watcher observing a half-authored tour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walkthrough {
    pub changeset: ChangesetKey,
    pub tour: String,
    pub chapter: Option<String>,
    pub chapter_author: Option<String>,
    pub stops: Vec<TourStop>,
}

pub struct AnnotationStore {
    conn: Connection,
}

impl AnnotationStore {
    /// Open (creating if absent) the store at `<commondir>/workon-review/annotations.db` for
    /// reading and writing: creates the parent directory, migrates the schema, and sets
    /// `journal_mode=WAL` + `busy_timeout=3000`ms so concurrent writers (the TUI, an MCP
    /// server) block briefly instead of erroring.
    pub fn open(commondir: &Path) -> Result<Self> {
        let path = db_path(commondir);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| AnnotationsError::DbDirFailed {
                path: dir.display().to_string(),
                source,
            })?;
        }
        let conn = Connection::open(&path).map_err(|source| AnnotationsError::OpenFailed {
            path: path.display().to_string(),
            source,
        })?;
        conn.busy_timeout(std::time::Duration::from_millis(3000))
            .map_err(AnnotationsError::MigrationFailed)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(AnnotationsError::MigrationFailed)?;
        // The bundled sqlite build happens to default foreign_keys on; don't depend on a
        // compile-time flag for the parent_uid constraint the delete walk relies on.
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(AnnotationsError::MigrationFailed)?;
        crate::schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Open the store read-only (`SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`, the flags
    /// `git-workon-lib`'s graphite-metadata reader uses). Errors if the database doesn't
    /// exist yet — there's nothing to read, and read-only can't create it.
    pub fn open_read_only(commondir: &Path) -> Result<Self> {
        let path = db_path(commondir);
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| AnnotationsError::OpenFailed {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Self { conn })
    }

    /// Insert a new top-level annotation (comment, tour stop, or chapter). Starts `Open`.
    pub fn insert(&self, new: NewAnnotation) -> Result<Annotation> {
        let now = now();
        let fields = anchor_fields(new.anchor.as_ref());
        let uid: String = self
            .conn
            .query_row(
                "INSERT INTO annotation (
                     uid, kind, status, parent_uid, changeset_name, changeset_uncommitted,
                     anchor_path, anchor_new_side, anchor_lineno, anchor_end_lineno,
                     anchor_target, anchor_before, anchor_after, anchor_ctx_hash,
                     body, author, tour, seq, created_at, updated_at
                 ) VALUES (
                     lower(hex(randomblob(16))), ?1, ?2, NULL, ?3, ?4,
                     ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, ?16, ?17, ?17
                 ) RETURNING uid",
                params![
                    new.kind.as_str(),
                    Status::Open.as_str(),
                    new.changeset.name(),
                    new.changeset.uncommitted(),
                    fields.path,
                    fields.new_side,
                    fields.lineno,
                    fields.end_lineno,
                    fields.target,
                    fields.before,
                    fields.after,
                    fields.ctx_hash,
                    new.body,
                    new.author,
                    new.tour,
                    new.seq,
                    now,
                ],
                |row| row.get(0),
            )
            .map_err(AnnotationsError::QueryFailed)?;
        self.bump_revision()?;

        Ok(Annotation {
            uid,
            kind: new.kind,
            status: Status::Open,
            parent_uid: None,
            changeset: new.changeset,
            anchor: new.anchor,
            body: new.body,
            author: new.author,
            tour: new.tour,
            seq: new.seq,
            created_at: now,
            updated_at: now,
        })
    }

    /// Reply to `parent_uid`. A reply carries no anchor of its own — it inherits the
    /// parent's location implicitly, so re-anchoring never has to keep two copies in sync.
    pub fn reply(&self, parent_uid: &str, body: &str, author: &str) -> Result<Annotation> {
        let parent = self
            .get(parent_uid)?
            .ok_or_else(|| AnnotationsError::NotFound {
                uid: parent_uid.to_string(),
            })?;
        let now = now();
        let uid: String = self
            .conn
            .query_row(
                "INSERT INTO annotation (
                     uid, kind, status, parent_uid, changeset_name, changeset_uncommitted,
                     body, author, created_at, updated_at
                 ) VALUES (
                     lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8
                 ) RETURNING uid",
                params![
                    parent.kind.as_str(),
                    Status::Open.as_str(),
                    parent_uid,
                    parent.changeset.name(),
                    parent.changeset.uncommitted(),
                    body,
                    author,
                    now,
                ],
                |row| row.get(0),
            )
            .map_err(AnnotationsError::QueryFailed)?;
        self.bump_revision()?;

        Ok(Annotation {
            uid,
            kind: parent.kind,
            status: Status::Open,
            parent_uid: Some(parent_uid.to_string()),
            changeset: parent.changeset,
            anchor: None,
            body: body.to_string(),
            author: author.to_string(),
            tour: None,
            seq: None,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn set_status(&self, uid: &str, status: Status) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE annotation SET status = ?1, updated_at = ?2 WHERE uid = ?3",
                params![status.as_str(), now(), uid],
            )
            .map_err(AnnotationsError::QueryFailed)?;
        if changed == 0 {
            return Err(AnnotationsError::NotFound {
                uid: uid.to_string(),
            });
        }
        self.bump_revision()
    }

    pub fn update_body(&self, uid: &str, body: &str) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE annotation SET body = ?1, updated_at = ?2 WHERE uid = ?3",
                params![body, now(), uid],
            )
            .map_err(AnnotationsError::QueryFailed)?;
        if changed == 0 {
            return Err(AnnotationsError::NotFound {
                uid: uid.to_string(),
            });
        }
        self.bump_revision()
    }

    /// Delete `uid` and (transitively) every reply to it.
    pub fn delete(&self, uid: &str) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(AnnotationsError::TransactionFailed)?;

        let exists: bool = tx
            .query_row("SELECT 1 FROM annotation WHERE uid = ?1", [uid], |_| Ok(()))
            .optional()
            .map_err(AnnotationsError::QueryFailed)?
            .is_some();
        if !exists {
            return Err(AnnotationsError::NotFound {
                uid: uid.to_string(),
            });
        }

        // One level of nesting today (replies don't themselves get replies), but delete
        // walks transitively in case that changes.
        let mut frontier = vec![uid.to_string()];
        let mut to_delete = Vec::new();
        while let Some(id) = frontier.pop() {
            let children: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT uid FROM annotation WHERE parent_uid = ?1")
                    .map_err(AnnotationsError::QueryFailed)?;
                let rows = stmt
                    .query_map([&id], |row| row.get(0))
                    .map_err(AnnotationsError::QueryFailed)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(AnnotationsError::QueryFailed)?
            };
            frontier.extend(children);
            to_delete.push(id);
        }
        // Reverse discovery order: children were discovered after their parents, so the
        // reverse walk deletes them first and never violates the parent_uid foreign key.
        for id in to_delete.iter().rev() {
            tx.execute("DELETE FROM annotation WHERE uid = ?1", [id])
                .map_err(AnnotationsError::QueryFailed)?;
        }
        bump_revision_tx(&tx)?;
        tx.commit().map_err(AnnotationsError::TransactionFailed)?;
        Ok(())
    }

    pub fn get(&self, uid: &str) -> Result<Option<Annotation>> {
        self.conn
            .query_row(
                "SELECT * FROM annotation WHERE uid = ?1",
                [uid],
                row_to_annotation,
            )
            .optional()
            .map_err(AnnotationsError::QueryFailed)
    }

    pub fn by_changeset(&self, key: &ChangesetKey) -> Result<Vec<Annotation>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM annotation WHERE changeset_name = ?1 AND changeset_uncommitted = ?2",
            )
            .map_err(AnnotationsError::QueryFailed)?;
        collect(stmt.query_map(params![key.name(), key.uncommitted()], row_to_annotation))
    }

    pub fn by_path(&self, key: &ChangesetKey, path: &str) -> Result<Vec<Annotation>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM annotation \
                 WHERE changeset_name = ?1 AND changeset_uncommitted = ?2 AND anchor_path = ?3",
            )
            .map_err(AnnotationsError::QueryFailed)?;
        collect(stmt.query_map(
            params![key.name(), key.uncommitted(), path],
            row_to_annotation,
        ))
    }

    /// Tour stops for `tour`, ordered by `seq`.
    pub fn tour(&self, tour: &str) -> Result<Vec<Annotation>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM annotation WHERE tour = ?1 ORDER BY seq")
            .map_err(AnnotationsError::QueryFailed)?;
        collect(stmt.query_map([tour], row_to_annotation))
    }

    /// The chapter annotation for `changeset`, if one exists.
    pub fn chapter(&self, changeset: &ChangesetKey) -> Result<Option<Annotation>> {
        self.conn
            .query_row(
                "SELECT * FROM annotation \
                 WHERE changeset_name = ?1 AND changeset_uncommitted = ?2 AND kind = ?3",
                params![
                    changeset.name(),
                    changeset.uncommitted(),
                    AnnotationKind::Chapter.as_str()
                ],
                row_to_annotation,
            )
            .optional()
            .map_err(AnnotationsError::QueryFailed)
    }

    /// Write a whole walkthrough (chapter + ordered tour stops) in one transaction, so a
    /// watcher never observes a half-authored tour.
    pub fn put_walkthrough(&self, walkthrough: Walkthrough) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(AnnotationsError::TransactionFailed)?;
        let now = now();

        if let Some(chapter) = &walkthrough.chapter {
            tx.execute(
                "INSERT INTO annotation (
                     uid, kind, status, changeset_name, changeset_uncommitted,
                     body, author, created_at, updated_at
                 ) VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    AnnotationKind::Chapter.as_str(),
                    Status::Open.as_str(),
                    walkthrough.changeset.name(),
                    walkthrough.changeset.uncommitted(),
                    chapter,
                    walkthrough.chapter_author.as_deref().unwrap_or(""),
                    now,
                ],
            )
            .map_err(AnnotationsError::QueryFailed)?;
        }

        for stop in &walkthrough.stops {
            tx.execute(
                "INSERT INTO annotation (
                     uid, kind, status, changeset_name, changeset_uncommitted,
                     anchor_path, anchor_new_side, anchor_lineno, anchor_end_lineno,
                     anchor_target, anchor_before, anchor_after, anchor_ctx_hash,
                     body, author, tour, seq, created_at, updated_at
                 ) VALUES (
                     lower(hex(randomblob(16))), ?1, ?2, ?3, ?4,
                     ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, ?16, ?17, ?17
                 )",
                params![
                    AnnotationKind::TourStop.as_str(),
                    Status::Open.as_str(),
                    walkthrough.changeset.name(),
                    walkthrough.changeset.uncommitted(),
                    stop.anchor.path,
                    stop.anchor.new_side,
                    stop.anchor.lineno,
                    stop.anchor.end_lineno,
                    stop.anchor.target,
                    join_lines(&stop.anchor.before),
                    join_lines(&stop.anchor.after),
                    crate::schema::ctx_hash(
                        &stop.anchor.before,
                        &stop.anchor.target,
                        &stop.anchor.after
                    ),
                    stop.body,
                    stop.author,
                    walkthrough.tour,
                    stop.seq,
                    now,
                ],
            )
            .map_err(AnnotationsError::QueryFailed)?;
        }

        bump_revision_tx(&tx)?;
        tx.commit().map_err(AnnotationsError::TransactionFailed)
    }

    /// A cheap fingerprint of the store's write state, for a poll-based watcher.
    /// `data_version` is sqlite's `PRAGMA data_version`: it changes only when some OTHER
    /// connection commits a write (this connection's own writes don't move it), so it's a
    /// free echo suppression the TUI would otherwise have to hand-build. `revision` is this
    /// crate's own counter, bumped on every write through this store (including this
    /// connection's), for callers that want to detect their own writes too.
    pub fn fingerprint(&self) -> Result<Fingerprint> {
        let data_version: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(AnnotationsError::QueryFailed)?;
        let revision: i64 = self
            .conn
            .query_row("SELECT revision FROM meta WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(AnnotationsError::QueryFailed)?;
        Ok(Fingerprint {
            data_version,
            revision,
        })
    }

    fn bump_revision(&self) -> Result<()> {
        self.conn
            .execute("UPDATE meta SET revision = revision + 1 WHERE id = 1", [])
            .map_err(AnnotationsError::QueryFailed)?;
        Ok(())
    }
}

fn bump_revision_tx(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute("UPDATE meta SET revision = revision + 1 WHERE id = 1", [])
        .map_err(AnnotationsError::QueryFailed)?;
    Ok(())
}

fn db_path(commondir: &Path) -> PathBuf {
    commondir.join("workon-review").join("annotations.db")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn join_lines(lines: &[String]) -> String {
    lines.join("\n")
}

fn split_lines(joined: &str) -> Vec<String> {
    if joined.is_empty() {
        Vec::new()
    } else {
        joined.split('\n').map(str::to_string).collect()
    }
}

/// An anchor's columns, pre-materialized into owned `Option`s (all `None` for `anchor:
/// None`) so callers can splice them straight into a `params!` invocation — `params!` needs
/// each argument to own (or outlive the call) its storage, which a `match`-per-field inline
/// can't give it.
struct AnchorFields {
    path: Option<String>,
    new_side: Option<bool>,
    lineno: Option<u32>,
    end_lineno: Option<u32>,
    target: Option<String>,
    before: Option<String>,
    after: Option<String>,
    ctx_hash: Option<String>,
}

fn anchor_fields(anchor: Option<&Anchor>) -> AnchorFields {
    match anchor {
        Some(a) => AnchorFields {
            path: Some(a.path.clone()),
            new_side: Some(a.new_side),
            lineno: Some(a.lineno),
            end_lineno: Some(a.end_lineno),
            target: Some(a.target.clone()),
            before: Some(join_lines(&a.before)),
            after: Some(join_lines(&a.after)),
            ctx_hash: Some(crate::schema::ctx_hash(&a.before, &a.target, &a.after)),
        },
        None => AnchorFields {
            path: None,
            new_side: None,
            lineno: None,
            end_lineno: None,
            target: None,
            before: None,
            after: None,
            ctx_hash: None,
        },
    }
}

fn collect(
    rows: rusqlite::Result<
        rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<Annotation>>,
    >,
) -> Result<Vec<Annotation>> {
    rows.map_err(AnnotationsError::QueryFailed)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(AnnotationsError::QueryFailed)
}

fn row_to_annotation(row: &Row<'_>) -> rusqlite::Result<Annotation> {
    let kind_str: String = row.get("kind")?;
    let status_str: String = row.get("status")?;
    let kind = AnnotationKind::from_str(&kind_str).unwrap_or(AnnotationKind::Comment);
    let status = Status::from_str(&status_str).unwrap_or(Status::Open);

    let anchor_path: Option<String> = row.get("anchor_path")?;
    let anchor = anchor_path.map(|path| -> rusqlite::Result<Anchor> {
        Ok(Anchor {
            path,
            new_side: row.get("anchor_new_side")?,
            lineno: row.get("anchor_lineno")?,
            end_lineno: row.get("anchor_end_lineno")?,
            target: row.get("anchor_target")?,
            before: split_lines(&row.get::<_, String>("anchor_before")?),
            after: split_lines(&row.get::<_, String>("anchor_after")?),
        })
    });
    let anchor = anchor.transpose()?;

    Ok(Annotation {
        uid: row.get("uid")?,
        kind,
        status,
        parent_uid: row.get("parent_uid")?,
        changeset: ChangesetKey::new(
            row.get::<_, String>("changeset_name")?,
            row.get("changeset_uncommitted")?,
        ),
        anchor,
        body: row.get("body")?,
        author: row.get("author")?,
        tour: row.get("tour")?,
        seq: row.get("seq")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_temp() -> (tempfile::TempDir, AnnotationStore) {
        let dir = tempdir().unwrap();
        let store = AnnotationStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn sample_anchor() -> Anchor {
        Anchor {
            path: "src/lib.rs".into(),
            new_side: true,
            lineno: 10,
            end_lineno: 10,
            target: "fn main() {}".into(),
            before: vec!["// comment".into()],
            after: vec![],
        }
    }

    #[test]
    fn schema_round_trip() {
        let (_dir, store) = open_temp();
        let key = ChangesetKey::new("feature-x", false);
        let inserted = store
            .insert(NewAnnotation {
                kind: AnnotationKind::Comment,
                changeset: key.clone(),
                anchor: Some(sample_anchor()),
                body: "why is this here?".into(),
                author: "reviewer".into(),
                tour: None,
                seq: None,
            })
            .unwrap();

        let fetched = store.get(&inserted.uid).unwrap().unwrap();
        assert_eq!(fetched, inserted);
        assert_eq!(
            fetched.anchor.unwrap().before,
            vec!["// comment".to_string()]
        );
    }

    #[test]
    fn reply_cascade_delete() {
        let (_dir, store) = open_temp();
        let key = ChangesetKey::new("feature-x", false);
        let root = store
            .insert(NewAnnotation {
                kind: AnnotationKind::Comment,
                changeset: key,
                anchor: None,
                body: "root".into(),
                author: "a".into(),
                tour: None,
                seq: None,
            })
            .unwrap();
        let reply = store.reply(&root.uid, "reply", "b").unwrap();

        store.delete(&root.uid).unwrap();
        assert!(store.get(&root.uid).unwrap().is_none());
        assert!(store.get(&reply.uid).unwrap().is_none());
    }

    #[test]
    fn fingerprint_unchanged_on_own_write() {
        let (dir, store) = open_temp();
        let before = store.fingerprint().unwrap();
        store
            .insert(NewAnnotation {
                kind: AnnotationKind::Comment,
                changeset: ChangesetKey::new("x", false),
                anchor: None,
                body: "b".into(),
                author: "a".into(),
                tour: None,
                seq: None,
            })
            .unwrap();
        let after = store.fingerprint().unwrap();
        // Own writes don't move sqlite's data_version...
        assert_eq!(before.data_version, after.data_version);
        // ...but this crate's own revision counter does track them.
        assert_eq!(before.revision + 1, after.revision);

        // A second connection's commit DOES move data_version.
        let other = AnnotationStore::open(dir.path()).unwrap();
        other
            .insert(NewAnnotation {
                kind: AnnotationKind::Comment,
                changeset: ChangesetKey::new("x", false),
                anchor: None,
                body: "c".into(),
                author: "a".into(),
                tour: None,
                seq: None,
            })
            .unwrap();
        let observed = store.fingerprint().unwrap();
        assert_ne!(after.data_version, observed.data_version);
    }

    #[test]
    fn tour_orders_by_seq() {
        let (_dir, store) = open_temp();
        store
            .put_walkthrough(Walkthrough {
                changeset: ChangesetKey::new("feature-x", false),
                tour: "onboarding".into(),
                chapter: Some("This changeset does X.".into()),
                chapter_author: Some("agent".into()),
                stops: vec![
                    TourStop {
                        anchor: sample_anchor(),
                        body: "second".into(),
                        author: "agent".into(),
                        seq: 2,
                    },
                    TourStop {
                        anchor: sample_anchor(),
                        body: "first".into(),
                        author: "agent".into(),
                        seq: 1,
                    },
                ],
            })
            .unwrap();

        let stops = store.tour("onboarding").unwrap();
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].body, "first");
        assert_eq!(stops[1].body, "second");

        let chapter = store
            .chapter(&ChangesetKey::new("feature-x", false))
            .unwrap()
            .unwrap();
        assert_eq!(chapter.body, "This changeset does X.");
    }

    #[test]
    fn open_read_only_rejects_missing_db() {
        let dir = tempdir().unwrap();
        let result = AnnotationStore::open_read_only(dir.path());
        assert!(result.is_err());
    }
}
