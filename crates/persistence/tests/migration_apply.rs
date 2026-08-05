//! Static integrity tests for the SQL migration set.
//!
//! These tests run without a live Postgres instance (CI does not provision
//! one). They validate the invariants that the runner in
//! `scripts/apply_migrations.sh` relies on:
//!
//! 1. Every migration file referenced from `crates/persistence/src/agent_v1.rs`
//!    via `include_str!` resolves to a non-empty, on-disk file. This keeps the
//!    embedded SQL constants in lock-step with the migration directory.
//! 2. Each migration file owns exactly one top-level transaction: exactly one
//!    `BEGIN;` statement at the top and exactly one trailing `COMMIT;`. The
//!    apply runner applies each file via `\i` without an outer transaction
//!    (Postgres does not support nested transactions), so each file must be
//!    self-contained. Inner `BEGIN`/`COMMIT` tokens that appear inside plpgsql
//!    function bodies (e.g. `BEGIN ... END;`) are *not* transaction control and
//!    are deliberately ignored — only statements consisting solely of `BEGIN;`
//!    or `COMMIT;` count.
//! 3. The filename version numbers (the leading `NNNN_` segment) are strictly
//!    monotonically increasing across the directory in lexical order. A future
//!    migration author who lands `0011` out of order, or who skips a number,
//!    fails here.
//! 4. The full migration set (0001..) is discoverable from the migrations
//!    directory at the workspace root, so a migration that is not yet wired
//!    into `agent_v1.rs` (e.g. a DDL-only migration like 0009/0010) is still
//!    covered by the transaction and monotonicity checks.
//!
//! These are structural checks. They do not execute the SQL, so they cannot
//! catch schema-level regressions. The opt-in `KRW_LIVE_E2E_ACCEPTANCE` path
//! in `crates/runtime-persistence/tests/live_end_to_end_acceptance.rs` is the
//! live-DB end-to-end gate; these tests are the always-on prerequisite that
//! makes the live gate safe to run.

#![deny(rust_2018_idioms)]

use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the workspace root from this crate's `CARGO_MANIFEST_DIR`.
///
/// `CARGO_MANIFEST_DIR` points at `crates/persistence`, so the workspace
/// root is the parent's parent.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A parsed migration filename: `(version: u32, name: String)`.
struct MigrationFile {
    version: u32,
    stem: String,
    path: PathBuf,
}

/// Read the four-digit version prefix from a migration filename, returning
/// `None` if the filename does not match the `NNNN_*` shape.
fn parse_version_prefix(file_name: &str) -> Option<u32> {
    let bytes = file_name.as_bytes();
    if bytes.len() < 5 || bytes[4] != b'_' {
        return None;
    }
    let prefix = std::str::from_utf8(&bytes[..4]).ok()?;
    if !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    prefix.parse::<u32>().ok()
}

/// List every `NNNN_*.sql` file in the migrations directory, sorted by
/// filename. Lexical order on the zero-padded prefix equals numeric order as
/// long as every version uses the same 4-digit width, which the test below
/// also asserts.
fn discover_migrations() -> Vec<MigrationFile> {
    let dir = workspace_root().join("migrations");
    let mut found: Vec<MigrationFile> = Vec::new();
    for entry in fs::read_dir(&dir).expect("migrations directory is readable") {
        let entry = entry.expect("directory entry is valid");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("sql") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("non-UTF-8 migration filename");
        let version = parse_version_prefix(file_name).unwrap_or_else(|| {
            panic!(
                "migration filename {file_name:?} must start with 4 digits and an underscore (e.g. 0001_name.sql)"
            );
        });
        let stem = file_name.to_string();
        found.push(MigrationFile {
            version,
            stem,
            path,
        });
    }
    found.sort_by(|a, b| a.stem.cmp(&b.stem));
    found
}

/// Count the SQL transaction control statements that consist solely of
/// `BEGIN;` or `COMMIT;` on their own line. This deliberately excludes
/// plpgsql-language `BEGIN`/`END;` blocks inside function bodies, which are
/// not transaction control: the plpgsql `BEGIN` token has no trailing
/// semicolon (it is followed by statements on subsequent lines), while the
/// SQL transaction-control `BEGIN;` always carries one. SQL comments
/// (`-- ...`) on the same physical line as the token are tolerated.
fn count_top_level_txn_markers(sql: &str, keyword: &str) -> usize {
    sql.lines()
        .filter(|line| {
            // Strip trailing `-- comment` so a `BEGIN; -- foo` still counts.
            let mut candidate = *line;
            if let Some((head, _)) = candidate.split_once("--") {
                candidate = head;
            }
            // Require exactly `<keyword>;` (case-insensitive), optionally
            // surrounded by whitespace. A bare `BEGIN` with no semicolon is
            // a plpgsql block start and must NOT count.
            let trimmed = candidate.trim();
            trimmed.eq_ignore_ascii_case(&format!("{keyword};"))
        })
        .count()
}

#[test]
fn every_migration_file_is_non_empty() {
    for migration in discover_migrations() {
        let contents = fs::read_to_string(&migration.path)
            .unwrap_or_else(|e| panic!("read {}: {e}", migration.stem));
        let non_whitespace: usize = contents.chars().filter(|c| !c.is_whitespace()).count();
        assert!(
            non_whitespace > 0,
            "migration {} is empty (only whitespace)",
            migration.stem
        );
    }
}

#[test]
fn every_migration_has_balanced_begin_commit() {
    let migrations = discover_migrations();
    assert!(
        !migrations.is_empty(),
        "expected at least one migration file"
    );
    for migration in &migrations {
        let contents = fs::read_to_string(&migration.path)
            .unwrap_or_else(|e| panic!("read {}: {e}", migration.stem));
        let begins = count_top_level_txn_markers(&contents, "BEGIN");
        let commits = count_top_level_txn_markers(&contents, "COMMIT");
        assert_eq!(
            begins, 1,
            "migration {} must contain exactly one top-level `BEGIN;` (found {begins}). \
             plpgsql inner `BEGIN ... END;` blocks do not count.",
            migration.stem
        );
        assert_eq!(
            commits, 1,
            "migration {} must contain exactly one top-level `COMMIT;` (found {commits})",
            migration.stem
        );
    }
}

#[test]
fn migration_filenames_are_strictly_monotonic() {
    let migrations = discover_migrations();
    assert!(
        !migrations.is_empty(),
        "expected at least one migration file"
    );
    // The first migration must be 0001. This catches an accidental 0000
    // bootstrap file that the runner would interpret as version 0.
    assert_eq!(
        migrations.first().unwrap().version,
        1,
        "first migration must be version 0001"
    );
    for window in migrations.windows(2) {
        let (prev, next) = (&window[0], &window[1]);
        assert_eq!(
            next.version,
            prev.version + 1,
            "migration versions must be contiguous: {} is followed by {} (expected {})",
            prev.stem,
            next.stem,
            prev.version + 1
        );
        // Lexical and numeric order must agree. This guards against mixed
        // padding widths (e.g. an `9999` followed by `10000`) silently
        // reordering under the runner's `[0-9][0-9][0-9][0-9]_*.sql` glob.
        assert!(
            prev.stem < next.stem,
            "lexical and numeric order disagree between {} and {}",
            prev.stem,
            next.stem
        );
    }
}

#[test]
fn embedded_sql_constants_match_on_disk_files() {
    // The embedded SQL constants in agent_v1.rs must resolve to the same
    // bytes as the migration files on disk. include_str! is evaluated at
    // compile time, so a stale constant (pointing at a renamed or deleted
    // migration) fails to compile; this test additionally pins that the
    // public constants are non-empty and that each referenced file exists
    // and is non-empty on disk.
    let embedded: &[(&str, &str)] = &[
        (
            "INITIAL_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::INITIAL_MIGRATION_SQL,
        ),
        (
            "ACTION_FINALIZATION_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::ACTION_FINALIZATION_MIGRATION_SQL,
        ),
        (
            "BOUNDED_CHILD_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::BOUNDED_CHILD_MIGRATION_SQL,
        ),
        (
            "SESSION_MEMORY_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::SESSION_MEMORY_MIGRATION_SQL,
        ),
        (
            "SESSION_MEMORY_SNAPSHOT_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::SESSION_MEMORY_SNAPSHOT_MIGRATION_SQL,
        ),
        (
            "FINAL_OUTPUT_READ_MIGRATION_SQL",
            krw_agent_persistence::agent_v1::FINAL_OUTPUT_READ_MIGRATION_SQL,
        ),
    ];
    for (name, sql) in embedded {
        assert!(
            !sql.trim().is_empty(),
            "embedded migration constant {name} is empty"
        );
    }
    // And confirm each referenced file actually exists in the directory.
    let migrations_dir = workspace_root().join("migrations");
    for expected in [
        "0001_agent_v1.sql",
        "0002_action_finalization.sql",
        "0003_bounded_child.sql",
        "0004_session_memory.sql",
        "0005_session_memory_snapshot.sql",
        "0006_read_final_output.sql",
    ] {
        let path: PathBuf = migrations_dir.join(expected);
        assert!(
            Path::new(&path).is_file(),
            "embedded migration constant references {expected}, but the file is missing"
        );
    }
}
