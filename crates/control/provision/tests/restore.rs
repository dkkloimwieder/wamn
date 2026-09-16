//! Live round-trip gate for the per-project-env RESTORE path (wamn-q3n.11).
//!
//! The restore counterpart of `tests/dump.rs`. Two tests, both driving the REAL
//! [`wamn_control_provision::pg_restore_argv`] builder against a `pg_dump -Fd` artifact
//! produced by the REAL [`wamn_control_provision::pg_dump_argv`]:
//!
//! 1. **scratch restore** (`clean = false`): seed a database, dump it, restore into
//!    a fresh scratch database, assert the rows survive — the non-destructive
//!    default (the carve-out path);
//! 2. **in-place clean restore** (`clean = true`): restore over a database that
//!    already holds a stale row, and assert `--clean` dropped it (the restored
//!    state replaces, not appends). This makes the `--clean` flag load-bearing.
//!
//! The databases are test databases on the test PostgreSQL server, and the
//! client tools must be on `PATH`.
//! The object-store transport is out of scope here; this validates the restore
//! of the artifact, substrate-independent.

use std::process::Command as Proc;

use wamn_control_provision::{pg_dump_argv, pg_restore_argv};

#[test]
fn restore_round_trips_and_clean_replaces_in_place() {
    for tool in ["psql", "pg_dump", "pg_restore"] {
        assert!(
            tool_present(tool),
            "the restore gate requires {tool} on PATH"
        );
    }

    let src_database = wamn_test_postgres::database();
    let scratch_database = wamn_test_postgres::database();
    let inplace_database = wamn_test_postgres::database();
    let src = src_database.url().to_owned();
    let scratch = scratch_database.url().to_owned();
    let inplace = inplace_database.url().to_owned();
    let dump_dir = std::env::temp_dir().join(format!("wamn-restore-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dump_dir);

    // Seed the source with an exact-decimal column (the no-float rule) so the
    // round-trip checks exact values and row count.
    run_psql(
        &src,
        "CREATE TABLE widgets (id int PRIMARY KEY, name text, qty numeric(6,2)); \
         INSERT INTO widgets VALUES (1,'alpha',12.50),(2,'beta',0.10),(3,'gamma',99.99)",
    );

    // Dump the source → directory-format artifact (the REAL dump builder).
    let out = dump_dir.to_string_lossy().to_string();
    let dump = pg_dump_argv(&src, &out);
    let status = Proc::new(&dump[0])
        .args(&dump[1..])
        .status()
        .expect("spawn pg_dump");
    assert!(status.success(), "pg_dump failed ({status})");
    assert!(
        dump_dir.join("toc.dat").exists(),
        "pg_dump -Fd must produce a directory"
    );

    // --- Test 1: scratch restore (clean = false) into a fresh empty database. ---
    let restore = pg_restore_argv(&scratch, &out, false);
    assert_eq!(restore[0], "pg_restore");
    let status = Proc::new(&restore[0])
        .args(&restore[1..])
        .status()
        .expect("spawn pg_restore");
    assert!(status.success(), "scratch pg_restore failed ({status})");

    let count = run_psql_query(&scratch, "SELECT count(*) FROM widgets");
    assert_eq!(count.trim(), "3", "all seeded rows restored into scratch");
    let qty = run_psql_query(&scratch, "SELECT qty::text FROM widgets WHERE id=1");
    assert_eq!(
        qty.trim(),
        "12.50",
        "exact-decimal value restored without loss"
    );

    // --- Test 2: in-place clean restore (clean = true) over a stale database. ---
    // Seed the in-place target with a STALE table carrying a row (id=99) that the
    // dump does not have. A `--clean` restore must DROP the table first, so id=99 is
    // gone and only the dump's rows remain. Without --clean the stale row survives
    // (kills the "--clean dropped" mutant).
    run_psql(
        &inplace,
        "CREATE TABLE widgets (id int PRIMARY KEY, name text, qty numeric(6,2)); \
         INSERT INTO widgets VALUES (99,'stale',0.00)",
    );
    let restore = pg_restore_argv(&inplace, &out, true);
    assert!(
        restore.iter().any(|a| a == "--clean"),
        "in-place restore must --clean"
    );
    let status = Proc::new(&restore[0])
        .args(&restore[1..])
        .status()
        .expect("spawn pg_restore");
    assert!(status.success(), "in-place pg_restore failed ({status})");

    let stale = run_psql_query(&inplace, "SELECT count(*) FROM widgets WHERE id=99");
    assert_eq!(
        stale.trim(),
        "0",
        "--clean dropped the stale pre-existing row (restore replaces, not appends)"
    );
    let total = run_psql_query(&inplace, "SELECT count(*) FROM widgets");
    assert_eq!(
        total.trim(),
        "3",
        "the in-place database now holds exactly the dump's rows"
    );

    // Teardown: the test databases drop with their values.
    let _ = std::fs::remove_dir_all(&dump_dir);
}

fn tool_present(tool: &str) -> bool {
    Proc::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_psql(url: &str, sql: &str) {
    let out = Proc::new("psql")
        .args([url, "-v", "ON_ERROR_STOP=1", "-q", "-c", sql])
        .output()
        .expect("spawn psql");
    assert!(
        out.status.success(),
        "psql failed for {sql:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_psql_query(url: &str, sql: &str) -> String {
    let out = Proc::new("psql")
        .args([url, "-v", "ON_ERROR_STOP=1", "-tAc", sql])
        .output()
        .expect("spawn psql");
    assert!(
        out.status.success(),
        "psql query failed for {sql:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}
