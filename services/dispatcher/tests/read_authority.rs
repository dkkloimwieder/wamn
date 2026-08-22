//! Live-apply gate for the DISPATCHER READ AUTHORITY floor (wamn-0h0g.12.66).
//!
//! Set `WAMN_PROVISION_PG_URL` to a **superuser** URL of a throwaway Postgres
//! (`CREATE DATABASE` / `CREATE ROLE` need it, exactly as the CNPG cluster
//! superuser does in production) — the same variable the provision crate's live
//! gates use, so one container serves them all. Skipped cleanly when unset.
//!
//! This gate lives with the dispatcher, not with the provisioner, because what is
//! under proof is a RELATIONSHIP between the two: the provisioner's real builders
//! mint the role, and the dispatcher's real statements — the very `parked_due_sql`
//! and [`RUN_QUEUE_DEPTH_SQL`] its sweep executes — must still run as it. A copy
//! of either statement would let the two drift apart silently.
//!
//! Four proofs:
//!
//! 1. **the surface is exactly right** — swept over EVERY relation and function in
//!    the project database, the reader holds zero write privileges, exactly two
//!    `SELECT`s, and can call zero functions.
//! 2. **the reads still work** — both real dispatcher statements execute as the
//!    reader and return the tenant's rows. This is the arm that matters most: an
//!    authority reduction that silently reads NOTHING presents as a perfectly
//!    healthy dispatcher (the wamn-0h0g.12.103 failure class).
//! 3. **the writes are denied, for the RIGHT reason** — every refused statement is
//!    deliberately RLS-legal for the pinned tenant, and each denial is checked to
//!    be a privilege refusal and *not* a row-level-security refusal. Both raise
//!    SQLSTATE 42501, so a naive probe passes for the wrong reason.
//! 4. **the denials are not vacuous** — each refused statement is replayed as
//!    `wamn_app` (the role the dispatcher used to authenticate as) inside a
//!    rolled-back transaction, and must SUCCEED there.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::{APP_ROLE, DISPATCH_READER_ROLE, project_env_database_name, sql};
use wamn_dispatcher::RUN_QUEUE_DEPTH_SQL;
use wamn_run_state::queue::parked_due_sql;

/// The tenant the probe's dispatcher session pins. [`OTHER_TENANT`] exists only so
/// the cross-tenant assertions are a real discrimination.
const TENANT: &str = "t-a";
const OTHER_TENANT: &str = "t-b";
const SCHEMA: &str = "wamn_run";
const READER_PASSWORD: &str = "dispatch-reader-probe";

/// SHA-256 of the empty input — the only bundle hash `catalog.execution_bundles`
/// accepts for a zero-byte artifact (its CHECK recomputes the digest).
const EMPTY_BUNDLE: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn psql(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-t", "-A", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    child.wait_with_output().expect("psql output")
}

fn run_ok(url: &str, script: &str) -> String {
    let out = psql(url, script);
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The `host:port` of a connection URL.
fn authority(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host_and_path = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    host_and_path.split('/').next().unwrap_or_default()
}

/// The same principal as `url`, pointed at another database.
fn with_database(url: &str, database: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("postgres", url));
    let userinfo = rest.rsplit_once('@').map(|(user, _)| user);
    match userinfo {
        Some(user) => format!("{scheme}://{user}@{}/{database}", authority(url)),
        None => format!("{scheme}://{}/{database}", authority(url)),
    }
}

/// Dial `role` as its OWN authenticated principal. Deliberately not `SET ROLE`,
/// which would prove nothing about the credential, the `CONNECT` grant, or
/// `pg_hba`.
fn role_url(superuser_url: &str, role: &str, password: &str, database: &str) -> String {
    format!(
        "postgres://{role}:{password}@{}/{database}",
        authority(superuser_url)
    )
}

/// The session the dispatcher's `dial` pins, in the same plain (non-`LOCAL`) form.
fn session() -> String {
    format!("SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';")
}

/// A statement that MUST be refused for lack of privilege.
///
/// The `EXCEPTION` arm catches `insufficient_privilege`, which is SQLSTATE 42501 —
/// and a row-level-security rejection raises **the same** 42501. So the message is
/// inspected too: if the refusal came from RLS, this probe has proved nothing about
/// the grant matrix and must fail loudly rather than pass.
fn assert_denied(url: &str, label: &str, statement: &str) {
    let script = format!(
        "{session} \
         DO $probe$ DECLARE state text; msg text; BEGIN \
           BEGIN \
             {statement}; \
             RAISE EXCEPTION 'VACUOUS PROBE: {label} was PERMITTED to the dispatch reader'; \
           EXCEPTION WHEN insufficient_privilege THEN \
             GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, msg = MESSAGE_TEXT; \
             ASSERT state = '42501', '{label} refused with a different sqlstate'; \
             ASSERT msg NOT LIKE '%row-level security%', \
               '{label} was refused by RLS, not by privilege — this probe would have passed for the wrong reason'; \
           END; \
         END $probe$;\n",
        session = session(),
    );
    let out = psql(url, &script);
    assert!(
        out.status.success(),
        "denial probe {label:?} did not hold:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The same statement must SUCCEED as `wamn_app` with the same tenant claim, inside
/// a transaction that is rolled back. This is what makes [`assert_denied`]
/// non-vacuous: it proves the statement is well-formed, FK-satisfiable and
/// RLS-legal, so the reader's refusal can only have been the missing grant.
fn assert_permitted_to_app_role(url: &str, label: &str, statement: &str) {
    let script = format!(
        "BEGIN; SET LOCAL search_path TO {SCHEMA}; SET LOCAL app.tenant TO '{TENANT}'; \
         {statement}; ROLLBACK;\n"
    );
    let out = psql(url, &script);
    assert!(
        out.status.success(),
        "non-vacuity arm {label:?} failed as {APP_ROLE} — the matching denial proved nothing:\
         \n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dispatcher_reads_the_queue_as_a_reader_that_cannot_write_it() {
    let Ok(url) = std::env::var("WAMN_PROVISION_PG_URL") else {
        eprintln!(
            "skipping dispatcher_reads_the_queue_as_a_reader_that_cannot_write_it \
             (set WAMN_PROVISION_PG_URL to run)"
        );
        return;
    };
    let database = project_env_database_name("acme", "dispatch", "dev", "k3m9x2p7");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

    // HERMETIC: drop the database AND the reader role first. A leftover healthy
    // role satisfies the builder's `IF NOT EXISTS` guard and masks a broken create
    // arm (the M2 gate-blind-spot lesson).
    //
    // THE DATABASE MUST GO FIRST. `DROP ROLE` fails outright while the role still
    // holds a grant in ANY database on the cluster — "role cannot be dropped
    // because some objects depend on it: privileges for database …" — and the
    // grantee's own database is not exempt. Reverse these two statements and both
    // the preamble and the teardown break.
    let teardown = format!(
        "{drop_db};\nDROP ROLE IF EXISTS \"{DISPATCH_READER_ROLE}\";\n",
        drop_db = sql::drop_database_named_sql(&database),
    );
    run_ok(&url, &teardown);

    // The REAL role builders. The run-plane DDL below assumes the stable ACL roles
    // already exist, exactly as it does in production.
    run_ok(
        &url,
        &format!(
            "{app}\n{owner}\n{effect}\n{reader}\n",
            app = sql::ensure_app_role_sql(APP_ROLE),
            owner = sql::ensure_db_owner_role_sql(),
            effect = sql::ensure_effect_writer_acl_role_sql(),
            reader = sql::ensure_dispatch_reader_role_sql(READER_PASSWORD),
        ),
    );
    run_ok(
        &url,
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$;\n",
    );
    // IDEMPOTENCY, arm 1: the role builder applied a SECOND time is a clean no-op.
    run_ok(&url, &sql::ensure_dispatch_reader_role_sql(READER_PASSWORD));

    // RE-HARDEN. The hermetic preamble drops the role, so the builder's CREATE arm
    // is the only one any other assertion here reaches and its `ELSIF` re-harden
    // would be dead code. Drift the role on purpose, re-apply, and require
    // convergence — this is the arm that separates "creates a correct role" from
    // "keeps a role correct", which is the whole point of a convergent builder.
    run_ok(
        &url,
        &format!(
            "ALTER ROLE \"{DISPATCH_READER_ROLE}\" BYPASSRLS CREATEDB INHERIT NOLOGIN;\n\
             DO $$ BEGIN \
               ASSERT (SELECT rolbypassrls AND NOT rolcanlogin FROM pg_roles \
                        WHERE rolname = '{DISPATCH_READER_ROLE}'), \
                 'the drift seed must really take, or the re-harden proof is vacuous'; \
             END $$;\n"
        ),
    );
    run_ok(&url, &sql::ensure_dispatch_reader_role_sql(READER_PASSWORD));
    run_ok(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT rolcanlogin AND NOT rolbypassrls AND NOT rolcreatedb \
                              AND NOT rolinherit \
                         FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}'), \
                 'a drifted dispatch reader must be re-hardened, not reported healthy'; \
             END $$;\n"
        ),
    );

    run_ok(&url, &sql::create_database_named_sql(&database));
    // Ownership converges BEFORE either CONNECT grant: `ALTER DATABASE … OWNER TO`
    // rewrites the outgoing owner's ACL entry, and a CONNECT granted while that
    // role still owned the database is carried away with it.
    let privilege_sql = format!(
        "{owner};\n{connect}\n{reader_connect}\n",
        owner = sql::set_database_owner_sql(&database),
        connect = sql::grant_connect_on_database_sql(&database),
        reader_connect = sql::grant_dispatch_reader_connect_sql(&database),
    );
    run_ok(&url, &privilege_sql);
    // IDEMPOTENCY, arm 2: the privilege batch is convergent, not one-shot.
    run_ok(&url, &privilege_sql);

    // The run-plane schema, applied as the cluster superuser exactly as the
    // reconciler does.
    let project_url = with_database(&url, &database);
    for ddl in ["catalog-schema.sql", "run-state.sql", "run-queue.sql"] {
        let path = format!("{root}/deploy/sql/{ddl}");
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        run_ok(&project_url, &body);
    }

    // The read surface, applied INSIDE the project database once its relations
    // exist — and applied TWICE, because the narrowing REVOKEs make a replay a real
    // operation rather than a skipped guard.
    let surface = sql::grant_dispatch_reader_read_surface_sql(SCHEMA);
    run_ok(&project_url, &surface);
    run_ok(&project_url, &surface);

    // Seed two tenants. The second exists so every cross-tenant assertion below is
    // a discrimination rather than a query that matches nothing.
    run_ok(
        &project_url,
        &format!(
            "INSERT INTO catalog.catalogs (tenant_id, catalog_id, version, schema_version) VALUES \
               ('{TENANT}','cat-a',1,'0.1'), ('{OTHER_TENANT}','cat-b',1,'0.1'); \
             INSERT INTO catalog.release_manifests (tenant_id, catalog_id, catalog_version) VALUES \
               ('{TENANT}','cat-a',1), ('{OTHER_TENANT}','cat-b',1); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length) VALUES \
               ('{TENANT}','{EMPTY_BUNDLE}','0.1','\\x'::bytea,0), \
               ('{OTHER_TENANT}','{EMPTY_BUNDLE}','0.1','\\x'::bytea,0); \
             INSERT INTO wamn_run.runs \
               (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                environment, execution_bundle_hash) VALUES \
               ('{TENANT}','run-a1','flow-a',1,'cat-a',1,'dev','{EMPTY_BUNDLE}'), \
               ('{TENANT}','run-a2','flow-a',1,'cat-a',1,'dev','{EMPTY_BUNDLE}'), \
               ('{OTHER_TENANT}','run-b1','flow-b',1,'cat-b',1,'dev','{EMPTY_BUNDLE}'); \
             INSERT INTO wamn_run.run_queue (tenant_id, run_id) VALUES \
               ('{TENANT}','run-a1'), ('{OTHER_TENANT}','run-b1');\n"
        ),
    );

    // --- proof 1: the surface is exactly right ------------------------------
    //
    // Swept over EVERY relation in the database, not over a list this test chose —
    // a hand-written list cannot notice a table someone adds later.
    run_ok(
        &project_url,
        &format!(
            r#"
DO $$ DECLARE writes int; reads int; app_writes int; callable int; app_callable int; BEGIN
  SELECT count(*) INTO writes FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','p','v','m') AND n.nspname NOT IN ('pg_catalog','information_schema')
     AND (has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'INSERT')
       OR has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'UPDATE')
       OR has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'DELETE')
       OR has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'TRUNCATE')
       OR has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'REFERENCES')
       OR has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'TRIGGER'));
  ASSERT writes = 0, 'the dispatch reader holds a write privilege on some relation';

  SELECT count(*) INTO reads FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','p','v','m') AND n.nspname NOT IN ('pg_catalog','information_schema')
     AND has_table_privilege('{DISPATCH_READER_ROLE}', c.oid, 'SELECT');
  ASSERT reads = 2, 'the dispatch reader must read exactly run_queue and effect_attempts, got ' || reads;

  -- NON-VACUITY: the identical sweep finds wamn_app holding many writes. Without
  -- this, `writes = 0` would also pass against a sweep that matches nothing.
  SELECT count(*) INTO app_writes FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relkind IN ('r','p','v','m') AND n.nspname NOT IN ('pg_catalog','information_schema')
     AND (has_table_privilege('{APP_ROLE}', c.oid, 'INSERT')
       OR has_table_privilege('{APP_ROLE}', c.oid, 'UPDATE')
       OR has_table_privilege('{APP_ROLE}', c.oid, 'DELETE'));
  ASSERT app_writes > 0, 'the write sweep matches nothing at all — it proves nothing';

  -- Exactly the two named relations, and specifically NOT the run history.
  ASSERT has_table_privilege('{DISPATCH_READER_ROLE}', '{SCHEMA}.run_queue', 'SELECT');
  ASSERT has_table_privilege('{DISPATCH_READER_ROLE}', '{SCHEMA}.effect_attempts', 'SELECT');
  ASSERT NOT has_table_privilege('{DISPATCH_READER_ROLE}', '{SCHEMA}.runs', 'SELECT'),
    'the dispatch reader must not read run history';

  -- Schema and database authority: USAGE only, CONNECT only.
  ASSERT has_schema_privilege('{DISPATCH_READER_ROLE}', '{SCHEMA}', 'USAGE');
  ASSERT NOT has_schema_privilege('{DISPATCH_READER_ROLE}', '{SCHEMA}', 'CREATE'),
    'the dispatch reader can create objects in the run-plane schema';
  ASSERT has_database_privilege('{DISPATCH_READER_ROLE}', '{database}', 'CONNECT');
  ASSERT NOT has_database_privilege('{DISPATCH_READER_ROLE}', '{database}', 'CREATE');
  ASSERT NOT has_database_privilege('{DISPATCH_READER_ROLE}', '{database}', 'TEMPORARY');

  -- Functions. Trigger functions are EXECUTE-to-PUBLIC by default and every new
  -- role inherits that, but a `RETURNS trigger` function cannot be called from SQL
  -- at all — so the number that matters is the CALLABLE ones.
  SELECT count(*) INTO callable FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
   WHERE n.nspname NOT IN ('pg_catalog','information_schema')
     AND pg_get_function_result(p.oid) <> 'trigger'
     AND has_function_privilege('{DISPATCH_READER_ROLE}', p.oid, 'EXECUTE');
  ASSERT callable = 0, 'the dispatch reader can call ' || callable || ' function(s)';
  -- NON-VACUITY: wamn_app can call at least one — wamn_run.lock_catalog_head, which
  -- is SECURITY DEFINER. That is authority the dispatcher never needed.
  SELECT count(*) INTO app_callable FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
   WHERE n.nspname NOT IN ('pg_catalog','information_schema')
     AND pg_get_function_result(p.oid) <> 'trigger'
     AND has_function_privilege('{APP_ROLE}', p.oid, 'EXECUTE');
  ASSERT app_callable > 0, 'the callable-function sweep matches nothing — it proves nothing';

  -- The role attributes the builder promised.
  ASSERT (SELECT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole
                 AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls
            FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}'),
    'the dispatch reader must be a LOGIN role with every other attribute off';
  -- NOINHERIT plus zero memberships: the grant inventory above is the whole story.
  ASSERT (SELECT count(*) FROM pg_auth_members m
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}')) = 0,
    'the dispatch reader is a member of no role';
  -- It owns nothing, anywhere (pg_shdepend is a shared catalog).
  ASSERT (SELECT count(*) FROM pg_shdepend d
           WHERE d.refclassid = 'pg_authid'::regclass AND d.deptype = 'o'
             AND d.refobjid = (SELECT oid FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}')) = 0,
    'the dispatch reader owns objects';
END $$;
"#
        ),
    );

    // --- proof 2: the real dispatcher statements still work -----------------
    let reader_url = role_url(&url, DISPATCH_READER_ROLE, READER_PASSWORD, &database);

    let identity = run_ok(
        &reader_url,
        &format!(
            "{} SELECT current_user::text || '|' || current_database()::text || '|' \
             || (SELECT rolbypassrls FROM pg_roles WHERE rolname = current_user)::text;\n",
            session()
        ),
    );
    assert_eq!(
        identity,
        format!("{DISPATCH_READER_ROLE}|{database}|false"),
        "the dispatcher's runtime identity is not the scoped, non-bypassing reader"
    );

    // THE ARM THAT MATTERS MOST. A reduction that silently reads zero rows looks
    // exactly like a healthy dispatcher with an empty queue.
    let woken = run_ok(
        &reader_url,
        &format!("{} {};\n", session(), parked_due_sql(64)),
    );
    assert_eq!(
        woken, "run-a1",
        "the reader must see its tenant's due row — and ONLY its tenant's"
    );
    let depth = run_ok(
        &reader_url,
        &format!("{} {RUN_QUEUE_DEPTH_SQL};\n", session()),
    );
    assert_eq!(
        depth, "1",
        "the queue-depth gauge must sample a non-zero depth"
    );

    // Cross-tenant rows exist but stay invisible: RLS, not the grant, scopes them.
    let other = run_ok(
        &reader_url,
        &format!(
            "{} SELECT count(*) FROM run_queue WHERE tenant_id = '{OTHER_TENANT}';\n",
            session()
        ),
    );
    assert_eq!(other, "0", "the reader saw another tenant's queue rows");
    let visible_to_owner = run_ok(
        &project_url,
        &format!("SELECT count(*) FROM wamn_run.run_queue WHERE tenant_id = '{OTHER_TENANT}';\n"),
    );
    assert_eq!(
        visible_to_owner, "1",
        "the other tenant's row must really exist, or the isolation check is vacuous"
    );

    // --- proofs 3 and 4: denied, for the right reason, non-vacuously --------
    //
    // Every statement names TENANT, so the RLS policy admits it and only the
    // missing grant can refuse it.
    // Each case carries BOTH forms of the same statement: the plpgsql form the
    // denial probe runs inside its `DO` block (reads use `PERFORM`, whose result
    // has somewhere to go), and the plain-SQL form the non-vacuity arm replays as
    // `wamn_app`. `None` means no replay is possible — see the ledger case.
    let insert_queue =
        format!("INSERT INTO run_queue (tenant_id, run_id) VALUES ('{TENANT}','run-a2')");
    let update_queue =
        format!("UPDATE run_queue SET available_at = now() WHERE tenant_id = '{TENANT}'");
    let delete_queue = format!("DELETE FROM run_queue WHERE tenant_id = '{TENANT}'");
    // PostgreSQL requires UPDATE privilege on at least one column for ANY
    // row-locking clause, so this is the assertion that would break the instant the
    // dispatcher grew a `FOR UPDATE` — which is exactly why a pure-SELECT role is
    // possible for it and for no other queue consumer.
    let lock_queue_plpgsql =
        format!("PERFORM 1 FROM run_queue WHERE tenant_id = '{TENANT}' FOR UPDATE");
    let lock_queue_sql = format!("SELECT 1 FROM run_queue WHERE tenant_id = '{TENANT}' FOR UPDATE");
    let read_runs_plpgsql = format!("PERFORM 1 FROM runs WHERE tenant_id = '{TENANT}'");
    let read_runs_sql = format!("SELECT 1 FROM runs WHERE tenant_id = '{TENANT}'");
    // No replay arm: `wamn_app` holds SELECT but not INSERT on the ledger either,
    // so there is no principal to prove the statement legal with. It stays because
    // privilege is checked BEFORE column constraints — a 42501 here can only be the
    // missing grant — and the RLS discrimination in `assert_denied` still applies.
    let insert_ledger =
        format!("INSERT INTO effect_attempts (tenant_id, run_id) VALUES ('{TENANT}','run-a1')");

    let app_url = role_url(&url, APP_ROLE, APP_ROLE, &database);
    for (label, plpgsql, replay) in [
        (
            "INSERT into run_queue",
            insert_queue.as_str(),
            Some(insert_queue.as_str()),
        ),
        (
            "UPDATE run_queue",
            update_queue.as_str(),
            Some(update_queue.as_str()),
        ),
        (
            "DELETE from run_queue",
            delete_queue.as_str(),
            Some(delete_queue.as_str()),
        ),
        (
            "SELECT FOR UPDATE on run_queue",
            lock_queue_plpgsql.as_str(),
            Some(lock_queue_sql.as_str()),
        ),
        (
            "SELECT from runs",
            read_runs_plpgsql.as_str(),
            Some(read_runs_sql.as_str()),
        ),
        ("INSERT into effect_attempts", insert_ledger.as_str(), None),
    ] {
        assert_denied(&reader_url, label, plpgsql);
        if let Some(replay) = replay {
            assert_permitted_to_app_role(&app_url, label, replay);
        }
    }

    // Teardown: self-contained; never touches a shared database.
    run_ok(&url, &teardown);
}
