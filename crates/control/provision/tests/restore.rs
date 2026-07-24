//! Live round-trip gate for the per-project-env RESTORE path (wamn-q3n.11).
//!
//! The restore counterpart of `tests/dump.rs`. Two proofs, both driving the REAL
//! [`wamn_provision::pg_restore_argv`] builder against a `pg_dump -Fd` artifact
//! produced by the REAL [`wamn_provision::pg_dump_argv`]:
//!
//! 1. **scratch restore** (`clean = false`): seed a database, dump it, restore into
//!    a fresh scratch database, assert the rows survive — the non-destructive
//!    default (the carve-out path);
//! 2. **in-place clean restore** (`clean = true`): restore over a database that
//!    already holds a stale row, and assert `--clean` dropped it (the restored
//!    state replaces, not appends). This makes the `--clean` flag load-bearing.
//!
//! Set `WAMN_RESTORE_PG_URL` to a **superuser** URL (it CREATEs/DROPs throwaway
//! databases); skipped cleanly when unset or when the client tools are absent
//! (the wamn-ddl / dump-gate pattern). The object-store transport is deferred
//! (wamn-e1g); this validates the restore of the artifact, substrate-independent.

use std::process::Command as Proc;

use wamn_provision::{pg_dump_argv, pg_restore_argv};

const SRC_DB: &str = "wamn_restore_src_test";
const SCRATCH_DB: &str = "wamn_restore_scratch_test";
const INPLACE_DB: &str = "wamn_restore_inplace_test";

#[test]
fn restore_round_trips_and_clean_replaces_in_place() {
    let Ok(admin) = std::env::var("WAMN_RESTORE_PG_URL") else {
        eprintln!("skipping restore gate (set WAMN_RESTORE_PG_URL to run)");
        return;
    };
    for tool in ["psql", "pg_dump", "pg_restore"] {
        if !tool_present(tool) {
            eprintln!("skipping restore gate (no {tool} on PATH)");
            return;
        }
    }

    let src = swap_db(&admin, SRC_DB);
    let scratch = swap_db(&admin, SCRATCH_DB);
    let inplace = swap_db(&admin, INPLACE_DB);
    let dump_dir = std::env::temp_dir().join(format!("wamn-restore-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dump_dir);

    // Fresh throwaway databases (CREATE DATABASE cannot run in a txn — psql -c is
    // autocommit). FORCE drops leftover connections from a prior run.
    for db in [SRC_DB, SCRATCH_DB, INPLACE_DB] {
        run_psql(
            &admin,
            &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"),
        );
        run_psql(&admin, &format!("CREATE DATABASE {db}"));
    }

    // Seed the source with an exact-decimal column (the no-float rule) so the
    // round-trip proves value fidelity, not just row count.
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

    // --- Proof 1: scratch restore (clean = false) into a fresh empty database. ---
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

    // --- Proof 2: in-place clean restore (clean = true) over a stale database. ---
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

    // Teardown.
    for db in [SRC_DB, SCRATCH_DB, INPLACE_DB] {
        run_psql(
            &admin,
            &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"),
        );
    }
    let _ = std::fs::remove_dir_all(&dump_dir);
}

/// Swap the database path segment of a libpq URL, preserving any query string.
fn swap_db(url: &str, db: &str) -> String {
    let (no_q, query) = match url.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let (base, _old) = no_q
        .rsplit_once('/')
        .expect("url has a database path segment");
    match query {
        Some(q) => format!("{base}/{db}?{q}"),
        None => format!("{base}/{db}"),
    }
}

fn tool_present(tool: &str) -> bool {
    Proc::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
