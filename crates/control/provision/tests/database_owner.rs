//! Live-apply gate for the project-env DATABASE OWNERSHIP floor (R9) and the
//! cluster-wide PUBLIC `CONNECT` floor.
//!
//! Set `WAMN_PROVISION_PG_URL` to a **superuser** URL of a throwaway Postgres
//! (`CREATE DATABASE` / `CREATE ROLE` need it, exactly as the CNPG cluster
//! superuser does in production) — the same variable `tests/provision.rs` uses,
//! so one container serves both. Skipped cleanly when unset.
//!
//! Two proofs, both driving the REAL builders:
//!
//! 1. **ownership** — a project-env database's `datdba` is `wamn_db_owner`, and
//!    specifically NOT `wamn_app` (the role guest-authored SQL executes as) and
//!    NOT a superuser. `wamn_app` is left with `CONNECT` and nothing else: no
//!    `CREATE`, no `TEMPORARY`, and zero owned objects anywhere in the cluster.
//!    `wamn_db_owner` itself cannot log in and holds no membership in either
//!    direction — it holds title and nothing else.
//! 2. **the `template1` route** — after the floor runs, `PUBLIC` holds no
//!    `CONNECT` on `template1` (which a `NOT datistemplate` filter skipped while
//!    `template1` stayed connectable), and `template0` is untouched.
//!
//! The databases are created superuser-owned and then converged with the REAL
//! [`sql::set_database_owner_sql`], so this exercises the RECONCILE path an
//! already-provisioned environment takes — not just the fresh-CR path. **Two**
//! databases are provisioned on purpose: the cross-database "wamn_app owns
//! nothing" sweep passes trivially against one.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::{APP_ROLE, DB_OWNER_ROLE, project_env_database_name, sql};

fn psql(url: &str, script: &str) -> std::process::Output {
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

fn run_ok(url: &str, script: &str) {
    let out = psql(url, script);
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn project_env_databases_are_owned_by_the_title_role_and_template1_is_closed() {
    let Ok(url) = std::env::var("WAMN_PROVISION_PG_URL") else {
        eprintln!(
            "skipping project_env_databases_are_owned_by_the_title_role_and_template1_is_closed \
             (set WAMN_PROVISION_PG_URL to run)"
        );
        return;
    };

    // TWO project-env databases: a single one makes every cross-database sweep
    // below vacuous (a loop over one row proves nothing about the cluster).
    let first = project_env_database_name("acme", "ownership", "dev");
    let second = project_env_database_name("acme", "ownership", "prod");

    // Clean slate — a leftover healthy role would satisfy the idempotent guard
    // and mask a mutated builder (the M2 gate-blind-spot lesson).
    run_ok(
        &url,
        &format!(
            "{drop_first};\n{drop_second};\nDROP ROLE IF EXISTS \"{DB_OWNER_ROLE}\";\n",
            drop_first = sql::drop_database_named_sql(&first),
            drop_second = sql::drop_database_named_sql(&second),
        ),
    );

    // The REAL role builders. `wamn_db_owner` must exist before anything names
    // it as an owner — the ordering obligation the CNPG `Database` CR inherits.
    run_ok(
        &url,
        &format!(
            "{app_role}\n{owner_role}\n",
            app_role = sql::ensure_app_role_sql("wamn_app"),
            owner_role = sql::ensure_db_owner_role_sql(),
        ),
    );
    // A second apply must be a clean no-op (advisory-locked create-or-harden).
    run_ok(&url, sql::ensure_db_owner_role_sql());

    // Both databases are born SUPERUSER-owned, then converged by the real
    // builder: this is the already-provisioned reconcile path, and it is the
    // half a fresh-CR-only change would leave undone.
    for database in [&first, &second] {
        run_ok(&url, &sql::create_database_named_sql(database));
    }
    run_ok(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{first}') \
                      <> '{DB_OWNER_ROLE}', \
                 'the databases must start mis-owned, or the convergence proof is vacuous'; \
             END $$;\n"
        ),
    );

    // The privilege SQL, in the order the runbook applies it. The owner change
    // MUST precede the CONNECT grant: `ALTER DATABASE … OWNER TO` rewrites the
    // outgoing owner's ACL entry, and a `wamn_app` grant made while `wamn_app`
    // owns the database merges into exactly that entry.
    let privilege_sql = |database: &str| {
        format!(
            "{owner};\n{connect}\n",
            owner = sql::set_database_owner_sql(database),
            connect = sql::grant_connect_on_database_sql(database),
        )
    };
    for database in [&first, &second] {
        run_ok(&url, &privilege_sql(database));
        // Replay: the whole batch is convergent, not one-shot.
        run_ok(&url, &privilege_sql(database));
    }

    // Restore PostgreSQL's DEFAULT PUBLIC CONNECT on template1 before running
    // the floor, and assert it took. The floor is convergent, so on a cluster a
    // previous run already converged the revoke is a no-op and every assertion
    // below passes against a template-filtered builder too — this seed is what
    // makes the floor actually do work on every run (the M2 gate-blind-spot
    // lesson: a healthy leftover masks a mutated builder).
    run_ok(
        &url,
        "GRANT CONNECT ON DATABASE template1 TO PUBLIC;\n\
         DO $$ BEGIN \
           ASSERT (SELECT count(*) FROM pg_database d \
                     CROSS JOIN LATERAL \
                       aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
                    WHERE d.datname = 'template1' \
                      AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT') = 1, \
             'the template1 seed must really be in place, or the floor proves nothing'; \
         END $$;\n",
    );

    // The cluster-wide PUBLIC CONNECT floor.
    run_ok(&url, sql::revoke_public_connect_floor_sql());

    run_ok(
        &url,
        &format!(
            r#"
DO $$ BEGIN
  -- 1. Ownership sits on the NOLOGIN title role, on BOTH databases.
  ASSERT (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{first}')
         = '{DB_OWNER_ROLE}',
    'the first project-env database is owned by the title role';
  ASSERT (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{second}')
         = '{DB_OWNER_ROLE}',
    'the second project-env database is owned by the title role';
  -- ... and specifically NOT on the guest-reachable role or a superuser.
  ASSERT (SELECT count(*) FROM pg_database d JOIN pg_roles r ON r.oid = d.datdba
           WHERE d.datname IN ('{first}', '{second}')
             AND (r.rolname = '{APP_ROLE}' OR r.rolsuper)) = 0,
    'no project-env database is owned by wamn_app or by a superuser';

  -- 2. wamn_app keeps CONNECT and loses every ownership-implied right. These
  --    are the rights no grant matrix expressed and no REVOKE could remove.
  ASSERT has_database_privilege('{APP_ROLE}', '{first}', 'CONNECT')
     AND has_database_privilege('{APP_ROLE}', '{second}', 'CONNECT'),
    'wamn_app still reaches both project-env databases';
  ASSERT has_database_privilege('{APP_ROLE}', '{first}', 'CREATE') = false
     AND has_database_privilege('{APP_ROLE}', '{second}', 'CREATE') = false,
    'wamn_app holds no CREATE on a project-env database';
  ASSERT has_database_privilege('{APP_ROLE}', '{first}', 'TEMPORARY') = false
     AND has_database_privilege('{APP_ROLE}', '{second}', 'TEMPORARY') = false,
    'wamn_app holds no TEMPORARY on a project-env database';

  -- 3. wamn_app owns NOTHING, cluster-wide (pg_shdepend is a shared catalog:
  --    ownership in every database is visible from this one connection).
  ASSERT (SELECT count(*) FROM pg_shdepend d
           WHERE d.refclassid = 'pg_authid'::regclass AND d.deptype = 'o'
             AND d.refobjid = (SELECT oid FROM pg_roles WHERE rolname = '{APP_ROLE}')) = 0,
    'wamn_app owns zero objects anywhere in the cluster';
  ASSERT (SELECT count(*) FROM pg_database
           WHERE datdba = (SELECT oid FROM pg_roles WHERE rolname = '{APP_ROLE}')) = 0,
    'wamn_app is the datdba of zero databases';
  -- Non-vacuity: the same sweep finds the title role holding BOTH databases.
  ASSERT (SELECT count(*) FROM pg_shdepend d
           WHERE d.refclassid = 'pg_authid'::regclass AND d.deptype = 'o'
             AND d.classid = 'pg_database'::regclass
             AND d.refobjid = (SELECT oid FROM pg_roles WHERE rolname = '{DB_OWNER_ROLE}')) = 2,
    'the ownership sweep really sees both databases (it is not matching nothing)';

  -- 4. Owner-only by construction: NOLOGIN, no memberships in EITHER direction,
  --    and no credential generations to rotate.
  ASSERT (SELECT NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb
                 AND NOT rolcreaterole AND NOT rolinherit AND NOT rolreplication
                 AND NOT rolbypassrls
            FROM pg_roles WHERE rolname = '{DB_OWNER_ROLE}'),
    'the title role cannot log in and carries no elevated attribute';
  ASSERT (SELECT count(*) FROM pg_auth_members m
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{DB_OWNER_ROLE}')
              OR m.roleid = (SELECT oid FROM pg_roles WHERE rolname = '{DB_OWNER_ROLE}')) = 0,
    'the title role is a member of nothing and has no members';
  ASSERT (SELECT rolpassword IS NULL FROM pg_authid WHERE rolname = '{DB_OWNER_ROLE}'),
    'the title role has no password — nothing authenticates as it, nothing rotates';

  -- 5. The template1 route is closed, and template0 is untouched. template0
  --    KEEPS its PUBLIC CONNECT aclitem (datallowconn = false makes it inert),
  --    so this pair is a real discrimination, not two ways of saying "clean".
  ASSERT (SELECT count(*) FROM pg_database d
            CROSS JOIN LATERAL aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl
           WHERE d.datname = 'template1'
             AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT') = 0,
    'PUBLIC holds no CONNECT on template1 after the floor';
  ASSERT (SELECT count(*) FROM pg_database d
            CROSS JOIN LATERAL aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl
           WHERE d.datname = 'template0'
             AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT') = 1,
    'template0 is untouched (datallowconn = false already makes it unreachable)';
  ASSERT (SELECT NOT datallowconn FROM pg_database WHERE datname = 'template0'),
    'template0 is unconnectable, which is why the floor may skip it';
  ASSERT (SELECT datallowconn FROM pg_database WHERE datname = 'template1'),
    'template1 IS connectable — that is exactly why the template filter was wrong';
END $$;
"#
        ),
    );

    // The read-only proof builder agrees: nothing connectable grants PUBLIC
    // CONNECT any more. An empty result is the pass.
    let out = psql(
        &url,
        &format!(
            "SELECT count(*) FROM ({}) AS leaks;\n",
            sql::public_connect_databases_sql()
        ),
    );
    assert!(
        out.status.success(),
        "public-connect proof failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains('0'),
        "public_connect_databases_sql still reports a database granting PUBLIC CONNECT: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Teardown: self-contained; never touches shared databases.
    run_ok(
        &url,
        &format!(
            "{drop_first};\n{drop_second};\nDROP ROLE IF EXISTS \"{DB_OWNER_ROLE}\";\n",
            drop_first = sql::drop_database_named_sql(&first),
            drop_second = sql::drop_database_named_sql(&second),
        ),
    );
}
