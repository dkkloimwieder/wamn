//! Live round-trip gate for the per-project-env dump ARTIFACT (wamn-q3n.10).
//!
//! The essential proof (Q2, substrate-agnostic): the `pg_dump -Fd` artifact the
//! renderer schedules is **valid and restorable** — seed a database, dump it with
//! the REAL [`wamn_provision::pg_dump_argv`] builder, `pg_restore` into a scratch
//! database, and assert the seeded rows survive the round-trip. One artifact
//! serves restore-to-last-dump AND the 10.3 export; this proves it restores.
//!
//! Set `WAMN_DUMP_PG_URL` to a **superuser** URL (it CREATEs/DROPs two throwaway
//! databases); skipped cleanly when unset or when the `pg_dump`/`pg_restore`/`psql`
//! client tools are absent (the wamn-ddl / wamn-run-store live-gate pattern). The
//! object-store transport is deferred (wamn-e1g); this gate validates the artifact
//! itself, which is substrate-independent.

use std::process::Command as Proc;

use wamn_provision::pg_dump_argv;

const SRC_DB: &str = "wamn_dump_src_test";
const SCRATCH_DB: &str = "wamn_dump_scratch_test";

#[test]
fn dump_round_trips_a_seeded_database() {
    let Ok(admin) = std::env::var("WAMN_DUMP_PG_URL") else {
        eprintln!("skipping dump_round_trips_a_seeded_database (set WAMN_DUMP_PG_URL to run)");
        return;
    };
    for tool in ["psql", "pg_dump", "pg_restore"] {
        if !tool_present(tool) {
            eprintln!("skipping dump_round_trips_a_seeded_database (no {tool} on PATH)");
            return;
        }
    }

    let src = swap_db(&admin, SRC_DB);
    let scratch = swap_db(&admin, SCRATCH_DB);
    let dump_dir = std::env::temp_dir().join(format!("wamn-dump-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dump_dir);

    // Fresh source + scratch databases (CREATE DATABASE cannot run in a txn — psql
    // -c is autocommit). FORCE drops any leftover connections from a prior run.
    for db in [SRC_DB, SCRATCH_DB] {
        run_psql(
            &admin,
            &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"),
        );
        run_psql(&admin, &format!("CREATE DATABASE {db}"));
    }

    // Seed the source with a table carrying an exact-decimal column (the no-float
    // rule) so the round-trip proves value fidelity, not just row count.
    run_psql(
        &src,
        "CREATE TABLE widgets (id int PRIMARY KEY, name text, qty numeric(6,2)); \
         INSERT INTO widgets VALUES (1,'alpha',12.50),(2,'beta',0.10),(3,'gamma',99.99)",
    );

    // Dump with the REAL builder → directory format artifact.
    let out = dump_dir.to_string_lossy().to_string();
    let argv = pg_dump_argv(&src, &out);
    assert_eq!(argv[0], "pg_dump");
    let status = Proc::new(&argv[0])
        .args(&argv[1..])
        .status()
        .expect("spawn pg_dump");
    assert!(status.success(), "pg_dump failed ({status})");
    // A -Fd (directory) dump is a directory with a toc.dat — proves the format live.
    assert!(
        dump_dir.join("toc.dat").exists(),
        "pg_dump -Fd must produce a directory-format artifact (toc.dat)"
    );

    // Restore into the scratch database (--no-owner/--no-privileges: the scratch DB
    // need not carry the source's roles — this validates the DATA, not ownership).
    let status = Proc::new("pg_restore")
        .args(["--no-owner", "--no-privileges", "-d", &scratch, &out])
        .status()
        .expect("spawn pg_restore");
    assert!(status.success(), "pg_restore failed ({status})");

    // The seeded rows survive the round-trip — count, names, and the exact decimal.
    let count = run_psql_query(&scratch, "SELECT count(*) FROM widgets");
    assert_eq!(count.trim(), "3", "all seeded rows restored");
    let names = run_psql_query(
        &scratch,
        "SELECT string_agg(name, ',' ORDER BY id) FROM widgets",
    );
    assert_eq!(
        names.trim(),
        "alpha,beta,gamma",
        "row content restored in order"
    );
    let qty = run_psql_query(&scratch, "SELECT qty::text FROM widgets WHERE id=1");
    assert_eq!(
        qty.trim(),
        "12.50",
        "exact-decimal value restored without loss"
    );

    // Teardown: drop both throwaway databases and the dump directory.
    for db in [SRC_DB, SCRATCH_DB] {
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
    let (base, _old_db) = no_q
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

/// Run a statement via `psql` (autocommit, stop on first error), asserting success.
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

/// Run a single-value query via `psql -tAc`, returning the (trimmed) scalar.
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
