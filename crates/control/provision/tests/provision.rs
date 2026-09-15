//! Live-apply gate for the platform extension builder, against a test database
//! on the test PostgreSQL server, through its superuser URL (the administrator
//! connection the extension builder documents). Shells out to `psql` so the
//! crate retains no database dependency.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::sql;

/// wamn-yk9l. The platform installs `btree_gist`, so an `EXCLUDE USING gist`
/// overlap constraint is expressible from a package's own `CREATE TABLE`.
///
/// The negative control is the point of this gate. Without the extension the
/// same statement refuses with `data type uuid has no default operator class
/// for access method "gist"`, which is what three pilot agents met, so this
/// asserts BOTH directions in one throwaway database.
/// The exact table three pilot agents needed and could not have.
const OVERLAP_PROBE_TABLE: &str = "CREATE TABLE overlap_probe.appointment (\
     id uuid PRIMARY KEY, dock_id uuid NOT NULL, during tstzrange NOT NULL, \
     CONSTRAINT appointment_no_overlap \
     EXCLUDE USING gist (dock_id WITH =, during WITH &&));";

#[test]
fn the_platform_extensions_make_an_exclusion_constraint_reachable() {
    let database = wamn_test_postgres::database();
    let url = database.url().to_owned();

    // 1. WITHOUT the extension: the constraint refuses for want of an operator
    //    class. Run alone, because ON_ERROR_STOP would abandon the script.
    let refusal = psql_output(
        &url,
        &format!(
            "CREATE SCHEMA overlap_probe;\n{OVERLAP_PROBE_TABLE}\nDROP SCHEMA overlap_probe CASCADE;\n"
        ),
    );
    assert!(
        !refusal.status.success(),
        "the constraint must refuse before the extension is installed"
    );
    let stderr = String::from_utf8_lossy(&refusal.stderr).to_string();
    assert!(
        stderr.contains("no default operator class for access method \"gist\""),
        "unexpected refusal:\n{stderr}"
    );

    // 2. WITH it: the same table is created, the first booking is accepted, and
    //    an overlapping second is refused BY THE DATABASE.
    let mut script = String::new();
    script.push_str(&sql::install_platform_extensions_sql());
    script.push_str("DROP SCHEMA IF EXISTS overlap_probe CASCADE;\n");
    script.push_str("CREATE SCHEMA overlap_probe;\n");
    script.push_str(OVERLAP_PROBE_TABLE);
    script.push_str(
        "\nINSERT INTO overlap_probe.appointment \
         VALUES (gen_random_uuid(), '11111111-1111-1111-1111-111111111111', \
         tstzrange('2026-10-01 09:00Z','2026-10-01 10:00Z'));\n",
    );
    let accepted = psql_output(&url, &script);
    assert!(
        accepted.status.success(),
        "the constraint must be reachable once the platform installs its extensions:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let overlap = psql_output(
        &url,
        "INSERT INTO overlap_probe.appointment \
         VALUES (gen_random_uuid(), '11111111-1111-1111-1111-111111111111', \
         tstzrange('2026-10-01 09:30Z','2026-10-01 10:30Z'));\n",
    );
    assert!(
        !overlap.status.success(),
        "an overlapping booking must be refused by the constraint"
    );
    assert!(
        String::from_utf8_lossy(&overlap.stderr)
            .contains("conflicting key value violates exclusion constraint"),
        "unexpected overlap result:\n{}",
        String::from_utf8_lossy(&overlap.stderr)
    );

    let cleanup = psql_output(&url, "DROP SCHEMA IF EXISTS overlap_probe CASCADE;\n");
    assert!(cleanup.status.success(), "probe schema removed");
}

/// Run one script through `psql` and hand back the whole outcome, so a test can
/// assert on a REFUSAL as readily as on success.
fn psql_output(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
