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
//! 1. **the surface is exactly right** — swept over EVERY relation in the project
//!    database, the reader holds zero write privileges and exactly two `SELECT`s,
//!    owns nothing, and belongs to exactly one role: `wamn_platform`, the shared
//!    NOLOGIN group that confers no grant of its own and exists only to be named
//!    by the tenant floor's permissive arm (`wamn-0h0g.22.17`).
//! 2. **the reads still work** — both real dispatcher statements execute as the
//!    reader and return the tenant's rows, AND the governed half of the surface
//!    (`effect_attempts`) answers at all. This is the arm that matters most: an
//!    authority reduction that silently reads NOTHING presents as a perfectly
//!    healthy dispatcher (the wamn-0h0g.12.103 failure class). `run_queue` alone
//!    cannot show it — that relation deliberately keeps the host-injected
//!    `app.tenant` claim, so it answers identically with the membership missing.
//! 3. **the writes are denied, for the RIGHT reason** — every refused statement is
//!    deliberately RLS-legal for the pinned tenant, and each denial is checked to
//!    be a privilege refusal and *not* a row-level-security refusal. Both raise
//!    SQLSTATE 42501, so a naive probe passes for the wrong reason.
//! 4. **the denials are not vacuous** — each refused statement is replayed as
//!    `wamn_app` (the role the dispatcher used to authenticate as) inside a
//!    rolled-back transaction, and must SUCCEED there.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DISPATCH_READER_ROLE, WorkloadRoleFamily, WorkloadRoleScope,
    project_env_database_name, sql, workload_generation_role,
};
use wamn_dispatcher::RUN_QUEUE_DEPTH_SQL;
use wamn_run_state::queue::parked_due_sql;

/// The tenant the probe's dispatcher session pins. [`OTHER_TENANT`] exists only so
/// the cross-tenant assertions are a real discrimination.
const TENANT: &str = "t-a";
const OTHER_TENANT: &str = "t-b";
const SCHEMA: &str = "wamn_run";
const READER_PASSWORD: &str = "dispatch-reader-probe";
/// The project-environment triple the probe's database and its dispatch-reader
/// generation both derive from. Naming it once is what keeps the login the gate
/// dials and the database it dials into the SAME scope.
const ORG: &str = "probe";
const PROJECT: &str = "dispatch";
const ENVIRONMENT: &str = "dev";

/// The dispatch-reader A generation this gate dials as, DERIVED from the same
/// builder `provision-project-env` uses rather than spelled (`wamn-0h0g.22.24`).
fn reader_generation(database: &str) -> String {
    workload_generation_role(
        WorkloadRoleFamily::DispatchReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database,
        },
        CredentialGeneration::A,
    )
    .expect("the dispatch reader takes a project-environment scope")
}

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

/// The same statement must SUCCEED for a principal that HOLDS the grant, with the
/// same tenant claim, inside a transaction that is rolled back. This is what
/// makes [`assert_denied`] non-vacuous: it proves the statement is well-formed,
/// FK-satisfiable and RLS-legal, so the reader's refusal can only have been the
/// missing grant.
///
/// The replay principal is NOT `wamn_app` any more, and this is a measured
/// correction rather than a preference. `wamn-0h0g.22.6.3` took the guest ACL
/// role's queue DML away: `deploy/sql/run-queue.sql` REVOKEs everything from
/// `wamn_app` and contains ZERO grants to it, so replaying a queue write as
/// `wamn_app` fails with `permission denied for table run_queue` and the arm
/// reports a broken control instead of a proof. Each queue statement now names
/// the principal the schema of record actually grants it to, or names none at
/// all — see the arm list.
fn assert_permitted_to(url: &str, role: &str, label: &str, statement: &str) {
    let script = format!(
        "BEGIN; SET LOCAL search_path TO {SCHEMA}; SET LOCAL app.tenant TO '{TENANT}'; \
         {statement}; ROLLBACK;\n"
    );
    let out = psql(url, &script);
    assert!(
        out.status.success(),
        "non-vacuity arm {label:?} failed as {role} — the matching denial proved nothing:\
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
    let database = project_env_database_name(ORG, PROJECT, ENVIRONMENT, "k3m9x2p7");
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
    let generation = reader_generation(&database);
    let teardown = format!(
        "{drop_db};\nDROP ROLE IF EXISTS \"{generation}\";\n\
         DROP ROLE IF EXISTS \"{DISPATCH_READER_ROLE}\";\n",
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
            reader = sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader),
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
    run_ok(
        &url,
        &sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader),
    );

    // RE-HARDEN. The hermetic preamble drops the role, so the builder's CREATE arm
    // is the only one any other assertion here reaches and its `ELSIF` re-harden
    // would be dead code. Drift the role on purpose, re-apply, and require
    // convergence — this is the arm that separates "creates a correct role" from
    // "keeps a role correct", which is the whole point of a convergent builder.
    // The drift seeded here is EXACTLY the shape `wamn-0h0g.22.24` retired — a
    // cluster-global LOGIN role carrying a password — because that is what a
    // pre-cutover cluster has and what the harden arm has to take away.
    run_ok(
        &url,
        &format!(
            "ALTER ROLE \"{DISPATCH_READER_ROLE}\" LOGIN PASSWORD 'legacy' BYPASSRLS CREATEDB;\n\
             DO $$ BEGIN \
               ASSERT (SELECT rolbypassrls AND rolcanlogin FROM pg_roles \
                        WHERE rolname = '{DISPATCH_READER_ROLE}'), \
                 'the drift seed must really take, or the re-harden proof is vacuous'; \
             END $$;\n"
        ),
    );
    run_ok(
        &url,
        &sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader),
    );
    run_ok(
        &url,
        &format!(
            "DO $$ BEGIN \
               ASSERT (SELECT NOT rolcanlogin AND rolpassword IS NULL AND NOT rolbypassrls \
                              AND NOT rolcreatedb AND NOT rolinherit \
                         FROM pg_authid WHERE rolname = '{DISPATCH_READER_ROLE}'), \
                 'a pre-cutover LOGIN dispatch reader must be re-hardened to a \
                  connection-free NOLOGIN carrier, not reported healthy'; \
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
        reader_connect = sql::revoke_dispatch_reader_connect_sql(&database),
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

    // THE SECOND HALF OF THE READ SURFACE IS NOT A GRANT (`wamn-0h0g.22.17`).
    // `DISPATCH_READER_RELATIONS` is `run_queue` PLUS `effect_attempts`, and only
    // the first is host-injected. `effect_attempts` carries the tenant floor,
    // whose key derives from `current_user` — and this role is named
    // `wamn_dispatch_reader`, which matches no guest generation pattern. Before
    // this bead the grant above bought `permission denied for function
    // current_tenant_key`; once the floor was narrowed `TO wamn_app` it would buy
    // a SILENT ZERO-ROW READ instead. The membership is what actually opens it,
    // and it is applied through the real builder rather than spelled here.
    run_ok(
        &project_url,
        &sql::platform_group_membership_sql(WorkloadRoleFamily::DispatchReader),
    );

    // THE CREDENTIAL IS A GENERATION, NOT THE STABLE ROLE (`wamn-0h0g.22.24`).
    // The stable role above is a NOLOGIN grant carrier with no CONNECT of its
    // own; this is the only thing in the cluster that can open a dispatcher
    // session, and it holds CONNECT on exactly one database.
    run_ok(
        &project_url,
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::DispatchReader,
            &database,
            &generation,
            READER_PASSWORD,
            "2100-01-01T00:00:00Z",
        ),
    );

    // Seed two tenants. The second exists so every cross-tenant assertion below is
    // a discrimination rather than a query that matches nothing.
    run_ok(
        &project_url,
        &format!(
            "INSERT INTO catalog.catalogs (tenant_id, catalog_id, version, schema_version) VALUES \
               ('{TENANT}','cat-a',1,'0.1'), ('{OTHER_TENANT}','cat-b',1,'0.1'); \
             INSERT INTO catalog.releases (tenant_id, catalog_id, catalog_version) VALUES \
               ('{TENANT}','cat-a',1), ('{OTHER_TENANT}','cat-b',1); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id, expected_environment, durability_class) VALUES \
               ('{TENANT}','dev','standard'), ('{OTHER_TENANT}','dev','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                environment) VALUES \
               ('{TENANT}','run-a1','flow-a',1,'cat-a',1,'dev'), \
               ('{TENANT}','run-a2','flow-a',1,'cat-a',1,'dev'), \
               ('{OTHER_TENANT}','run-b1','flow-b',1,'cat-b',1,'dev'); \
             INSERT INTO wamn_run.run_queue (tenant_id, run_id) VALUES \
               ('{TENANT}','run-a1'), ('{OTHER_TENANT}','run-b1');\n"
        ),
    );
    // One effect-ledger row per tenant. `effect_attempts` is the GOVERNED half of
    // the dispatch reader's surface, so a cross-tenant discrimination there needs
    // two tenants present exactly as the queue seed above does.
    run_ok(
        &project_url,
        &format!(
            "INSERT INTO wamn_run.effect_attempts \
               (tenant_id, attempt_id, run_id, root_plan_hash, current_plan_hash, frame_id, \
                local_node_id, source_artifact_hash, requirement_name, occurrence, seq, \
                generation_fact_kind, attempt_started_at, attempt_deadline_at, \
                attempt_input_ref, created_at) \
             SELECT t, gen_random_uuid(), 'run-x', 'sha256:' || repeat('0', 64), \
                    'sha256:' || repeat('0', 64), 0, 'n', 'sha256:' || repeat('0', 64), \
                    'req', 0, 0, 'not-required', now(), now(), 'ref', now() \
               FROM unnest(ARRAY['{TENANT}', '{OTHER_TENANT}']) AS t;\n"
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
DO $$ DECLARE writes int; reads int; app_writes int; BEGIN
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
  -- CONNECT BELONGS TO THE GENERATION, NEVER TO THE STABLE ROLE
  -- (`wamn-0h0g.22.24`). The stable role is CLUSTER-GLOBAL and every generation
  -- inherits it WITH INHERIT TRUE, so a CONNECT here is a session on every
  -- database on the cluster. `has_database_privilege` resolves THROUGH
  -- membership, which is why the generation reads true while its parent reads
  -- false — and why the parent reading true would be the whole defect back.
  ASSERT NOT has_database_privilege('{DISPATCH_READER_ROLE}', '{database}', 'CONNECT'),
    'the cluster-global dispatch reader holds CONNECT; every generation inherits it';
  ASSERT has_database_privilege('{generation}', '{database}', 'CONNECT');
  ASSERT NOT has_database_privilege('{generation}', '{database}', 'CREATE');
  ASSERT NOT has_database_privilege('{generation}', '{database}', 'TEMPORARY');
  ASSERT (SELECT count(*) FROM pg_database d
           WHERE d.datname <> '{database}' AND NOT d.datistemplate
             AND has_database_privilege('{generation}', d.oid, 'CONNECT')) = 0,
    'the generation reaches a database other than the one it was minted for';

  -- The STABLE role's attributes: a connection-free NOLOGIN grant carrier.
  ASSERT (SELECT NOT rolcanlogin AND rolpassword IS NULL AND NOT rolsuper
                 AND NOT rolcreatedb AND NOT rolcreaterole
                 AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls
            FROM pg_authid WHERE rolname = '{DISPATCH_READER_ROLE}'),
    'the stable dispatch reader must be NOLOGIN, credential-free, and otherwise bare';
  -- The GENERATION's: a login that inherits, and nothing else.
  ASSERT (SELECT rolcanlogin AND rolinherit AND NOT rolsuper AND NOT rolcreatedb
                 AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls
            FROM pg_authid WHERE rolname = '{generation}'),
    'the dispatch-reader generation must be an inheriting, non-bypassing LOGIN';
  ASSERT (SELECT count(*) FROM pg_auth_members m
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{generation}')) = 1,
    'the generation must carry exactly the stable-role edge';
  ASSERT (SELECT bool_and(parent.rolname = '{DISPATCH_READER_ROLE}' AND m.inherit_option
                          AND NOT m.admin_option AND NOT m.set_option)
            FROM pg_auth_members m
            JOIN pg_roles parent ON parent.oid = m.roleid
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{generation}')),
    'the generation edge is not exactly the stable role INHERIT TRUE, SET FALSE';
  -- NOINHERIT plus EXACTLY ONE membership: `wamn_platform`, which confers no
  -- grant of its own and exists only to be named by the tenant floor's permissive
  -- arm (`wamn-0h0g.22.17`). The grant inventory above is still the whole story of
  -- what this role may reach; this edge is what stops `effect_attempts` reading
  -- zero rows in silence. `INHERIT TRUE` is spelled, and MUST be: PostgreSQL 16+
  -- takes the edge's default from the member's `rolinherit`, and this role is
  -- NOINHERIT, so a bare GRANT lands `inherit_option = false` and the arm never
  -- matches.
  ASSERT (SELECT count(*) FROM pg_auth_members m
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}')) = 1,
    'the dispatch reader must carry exactly the wamn_platform edge';
  ASSERT (SELECT bool_and(parent.rolname = 'wamn_platform' AND m.inherit_option
                          AND NOT m.admin_option AND NOT m.set_option)
            FROM pg_auth_members m
            JOIN pg_roles parent ON parent.oid = m.roleid
           WHERE m.member = (SELECT oid FROM pg_roles WHERE rolname = '{DISPATCH_READER_ROLE}')),
    'the dispatch reader edge is not exactly wamn_platform INHERIT TRUE';
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
    let reader_url = role_url(&url, &generation, READER_PASSWORD, &database);

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
        format!("{generation}|{database}|false"),
        "the dispatcher's runtime identity is not the scoped, non-bypassing \
         reader GENERATION"
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

    // THE GOVERNED HALF OF THE SURFACE, READ FOR REAL. `run_queue` above proves
    // nothing about the tenant floor: it deliberately KEEPS the host-injected
    // `app.tenant` claim, so it answers identically with or without the platform
    // membership. `effect_attempts` is the relation the reader was actually locked
    // out of, and a count is the only honest probe — the lockout this bead repairs
    // returns zero rows, not an error.
    //
    // TWO, not one. `wamn_dispatch_reader` is PROJECT-ENVIRONMENT grain — its name
    // encodes no tenant and `WorkloadRoleScope::ProjectEnvironment` has no tenant
    // field — so the permissive arm admits every tenant in the database. What
    // narrows a dispatcher statement is the predicate the statement itself carries,
    // exactly as `run_queue` above is narrowed by RLS on a claim the HOST injects.
    let ledger = run_ok(
        &reader_url,
        &format!("{} SELECT count(*) FROM effect_attempts;\n", session()),
    );
    assert_eq!(
        ledger, "2",
        "the dispatch reader must reach the governed half of its own read surface"
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
    // No replay arm: no in-tree principal holds INSERT on the ledger, so there is
    // none to prove the statement legal with. It stays because privilege is
    // checked BEFORE column constraints — a 42501 here can only be the missing
    // grant — and the RLS discrimination in `assert_denied` still applies.
    let insert_ledger =
        format!("INSERT INTO effect_attempts (tenant_id, run_id) VALUES ('{TENANT}','run-a1')");

    // The management admitter is the ONE principal the schema of record grants a
    // queue write to (`grant_management_admitter_surface_sql`: column-scoped
    // INSERT on `tenant_id, run_id, available_at, stream_seq`). It is a NOLOGIN
    // stable ACL role like every other, so the replay dials a GENERATION of it —
    // minted here by the real builder, exactly as the dispatch reader's is.
    let admitter_generation = workload_generation_role(
        WorkloadRoleFamily::ManagementAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("the management admitter takes a project-environment scope");
    run_ok(
        &project_url,
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::ManagementAdmitter,
            &database,
            &admitter_generation,
            READER_PASSWORD,
            "2100-01-01T00:00:00Z",
        ),
    );
    let app_url = role_url(&url, APP_ROLE, APP_ROLE, &database);
    let admitter_url = role_url(&url, &admitter_generation, READER_PASSWORD, &database);
    for (label, plpgsql, replay) in [
        (
            "INSERT into run_queue",
            insert_queue.as_str(),
            Some((admitter_url.as_str(), insert_queue.as_str())),
        ),
        // No replay arm for the three below: since `wamn-0h0g.22.6.3` NOTHING in
        // this tree holds UPDATE or DELETE on `run_queue` — the executor-platform
        // family that will is admitted to the vocabulary but has no grant set yet
        // — so there is no principal to prove them legal with. `assert_denied`
        // still discriminates a privilege refusal from an RLS one, which is the
        // half that could otherwise pass for the wrong reason.
        ("UPDATE run_queue", update_queue.as_str(), None),
        ("DELETE from run_queue", delete_queue.as_str(), None),
        (
            "SELECT FOR UPDATE on run_queue",
            lock_queue_plpgsql.as_str(),
            None,
        ),
        (
            // `wamn_app` DOES still hold SELECT on `runs`, so this arm keeps a
            // real replay principal.
            "SELECT from runs",
            read_runs_plpgsql.as_str(),
            Some((app_url.as_str(), read_runs_sql.as_str())),
        ),
        ("INSERT into effect_attempts", insert_ledger.as_str(), None),
    ] {
        assert_denied(&reader_url, label, plpgsql);
        if let Some((replay_url, replay)) = replay {
            assert_permitted_to(replay_url, "the granted principal", label, replay);
        }
    }
    let _ = lock_queue_sql;

    // Teardown: self-contained; never touches a shared database.
    run_ok(&url, &teardown);
}
