//! Schema DDL and the fixed-content hash used to detect an anchor's captured context. DDL
//! idioms (batch-execute a `CREATE TABLE`/`CREATE INDEX` script, `busy_timeout` +
//! `journal_mode=WAL` for the writer, `READ_ONLY | NO_MUTEX` for a reader) follow
//! `git-workon-fixture`'s sqlite graphite-metadata writer and `git-workon-lib`'s reader
//! (`stack/graphite.rs`).

use rusqlite::Connection;

use crate::error::{AnnotationsError, Result};

/// Bumped when the DDL below changes in a way existing databases need migrating for. Slice 1
/// ships version 1; there is no migration path yet because there is nothing to migrate from.
pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS annotation (
    uid                   TEXT PRIMARY KEY,
    kind                  TEXT NOT NULL,
    status                TEXT NOT NULL,
    parent_uid            TEXT REFERENCES annotation(uid),
    changeset_name        TEXT NOT NULL,
    changeset_uncommitted INTEGER NOT NULL,
    anchor_path           TEXT,
    anchor_new_side       INTEGER,
    anchor_lineno         INTEGER,
    anchor_end_lineno     INTEGER,
    anchor_target         TEXT,
    anchor_before         TEXT,
    anchor_after          TEXT,
    anchor_ctx_hash       TEXT,
    body                  TEXT NOT NULL,
    author                TEXT NOT NULL,
    tour                  TEXT,
    seq                   INTEGER,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_annotation_changeset_path
    ON annotation (changeset_name, changeset_uncommitted, anchor_path);

CREATE INDEX IF NOT EXISTS idx_annotation_parent
    ON annotation (parent_uid);

CREATE INDEX IF NOT EXISTS idx_annotation_tour
    ON annotation (tour, seq);

CREATE TABLE IF NOT EXISTS meta (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL,
    revision       INTEGER NOT NULL
);
"#;

/// Apply the DDL (idempotent — every statement is `IF NOT EXISTS`) and seed the singleton
/// `meta` row if this is a fresh database.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)
        .map_err(AnnotationsError::MigrationFailed)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta (id, schema_version, revision) VALUES (1, ?1, 0)",
        [SCHEMA_VERSION],
    )
    .map_err(AnnotationsError::MigrationFailed)?;
    Ok(())
}

/// FNV-1a 64-bit over `before` + `target` + `after` (joined with `\n`), stored alongside the
/// captured context as `ctx_hash`. Deliberately NOT `std::hash::DefaultHasher`: that hasher's
/// output isn't guaranteed stable across Rust releases, and this hash is persisted to disk and
/// compared against on a later run, possibly built by a different toolchain.
pub fn ctx_hash(before: &[String], target: &str, after: &[String]) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    let mut feed = |s: &str| {
        for byte in s.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // A separator byte outside ASCII text keeps "a","bc" from hashing the same as "ab","c".
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    };
    for line in before {
        feed(line);
    }
    feed(target);
    for line in after {
        feed(line);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_hash_is_deterministic() {
        let a = ctx_hash(&["x".into(), "y".into()], "target", &["z".into()]);
        let b = ctx_hash(&["x".into(), "y".into()], "target", &["z".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn ctx_hash_distinguishes_boundary_shifts() {
        let a = ctx_hash(&["ab".into()], "c", &[]);
        let b = ctx_hash(&["a".into()], "bc", &[]);
        assert_ne!(a, b);
    }
}
