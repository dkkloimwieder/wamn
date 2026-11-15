//! Provisioned credentials permit their operations and refuse adjacent authority.
//!
//! Every denial is measured from a session authenticated AS the family's own
//! generation login, and that session first asserts it is neither `rolsuper`
//! nor `rolbypassrls` — a superuser fixture satisfies every probe below while
//! showing nothing.
//!
//! # The column-exactness arm
//!
//! `wamn_run.runs` is UPDATEd by SIX distinct executor statements and the grant
//! is the UNION of the columns they write, so the direct grants alone could pass
//! a column list that is exact but unusable. The last arm therefore connects as
//! the generation and asserts the POST-STATE both ways: a granted column moves
//! the row, an ungranted one raises `42501` AND leaves the row unmoved. Under
//! FORCE RLS an "it failed" assertion cannot tell REFUSED from MATCHED-NOTHING,
//! which is why the post-state is read back in both directions.
//!
//! Each condition takes its own test database on the test PostgreSQL server and holds
//! the process lock, because it rebuilds cluster-wide roles.

use std::fmt::Write as _;

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;

use wamn_control_provision::WorkloadRoleFamily;
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{WorkloadRoleIdentity, workload_login};

const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

const ORG: &str = "acme";
const PROJECT: &str = "billing";
const ENV: &str = "dev";
const GENERATION_PW: &str = "wamn_family_surface_pw";

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal is asserted on `42501` rather than on an error string.
fn psql(url: &str, script: &str) -> (bool, String, String) {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let script =
        format!("SET app.user_id='{actor}'; SET app.operation='admin:provision-fixture'; {script}");
    let mut child = Command::new("psql")
        .arg(url)
        .args([
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
            "-q",
            "-f",
            "-",
        ])
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
        .expect("write the script to psql");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_admin(url: &str, what: &str, script: &str) -> String {
    let (ok, out, err) = psql(url, script);
    assert!(ok, "{what} failed:\nstdout:\n{out}\nstderr:\n{err}");
    out
}

fn query(url: &str, sql: &str) -> String {
    let output = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-tAq", "-c", sql])
        .output()
        .expect("spawn psql");
    assert!(
        output.status.success(),
        "query failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The SQLSTATE one statement raised as the given login, or `None` on success.
fn sqlstate(url: &str, statement: &str) -> Option<String> {
    let (ok, _, err) = psql(url, statement);
    if ok {
        return None;
    }
    // `VERBOSITY=verbose` prints `ERROR:  42501: permission denied for …`. A
    // SQLSTATE is five uppercase alphanumerics carrying at least one digit,
    // which is what separates it from the `ERROR:` label beside it.
    err.split_whitespace()
        .filter_map(|token| token.strip_suffix(':'))
        .find(|code| {
            code.len() == 5
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && code.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .or_else(|| panic!("no SQLSTATE in psql stderr:\n{err}"))
}

fn generation(family: WorkloadRoleFamily, database: &str) -> String {
    workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENV,
            database,
        },
    )
    .expect("these families take a project-environment target")
}

/// The admin URL with its userinfo replaced by one role's login.
fn role_url(admin_url: &str, role: &str, password: &str) -> String {
    let mut url = Url::parse(admin_url).expect("the test database URL is a URL");
    url.set_username(role).expect("set the probe role");
    url.set_password(Some(password)).expect("set the password");
    url.into()
}

/// Rebuild the two schemas and every cluster-global role they and the families
/// name, so nothing this acceptance test measures survives from a previous run.
///
/// `DROP OWNED BY` before `DROP ROLE`, inside an existence test: a leftover
/// HEALTHY role satisfies `ensure_workload_acl_role_sql`'s `IF NOT EXISTS` and
/// would mask a mutated builder — which is the exact failure the mutant arms of
/// this bead depend on being impossible.
fn reset(admin_url: &str, database: &str) {
    let mut roles = vec![
        generation(WorkloadRoleFamily::ExecutorPlatform, database),
        generation(WorkloadRoleFamily::HttpAdmitter, database),
        WorkloadRoleFamily::ExecutorPlatform.acl_role().to_owned(),
        WorkloadRoleFamily::HttpAdmitter.acl_role().to_owned(),
    ];
    // The roles the two artifacts GRANT to or CREATE. A `GRANT … TO` a missing
    // role fails the whole apply, so the ones the files do not create themselves
    // are re-minted below.
    roles.extend(["wamn_app", "wamn_run_retention", "wamn_platform"].map(str::to_owned));
    let mut script = String::from(
        "DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS platform CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n",
    );
    for role in roles {
        writeln!(
            script,
            "DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $reset$;"
        )
        .expect("writing to a String cannot fail");
    }
    script.push_str(
        "CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;\n\
         \n\
         \n",
    );
    run_admin(admin_url, "reset the family-surface fixture", &script);
    for artifact in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        run_admin(admin_url, "apply a run-plane artifact", artifact);
    }
}

/// The whole ACL the named role holds in this database, as the server reports
/// it — relation, column, schema and ROUTINE entries alike, so a grant of any
/// grain lands in the comparison.
async fn load_role_grants(admin_url: &str, role: &str) -> Vec<(String, String, String, String)> {
    let mut connection = wamn_postgres::connect(admin_url).await.unwrap();
    sqlx::query_as(
        "SELECT type, sch, obj, priv FROM ( \
               SELECT 'relation' AS type, n.nspname::text AS sch, c.relname::text AS obj, \
                      x.privilege_type::text AS priv \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(c.relacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                      x.privilege_type::text \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(a.attacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'schema', n.nspname::text, n.nspname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_proc p \
                 JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                 CROSS JOIN LATERAL aclexplode(p.proacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'database', '', d.datname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
                WHERE x.grantee = $1::text::regrole \
             ) q ORDER BY 1, 2, 3, 4",
    )
    .bind(role)
    .fetch_all(&mut connection)
    .await
    .unwrap()
}

/// THE EXECUTOR-PLATFORM SURFACE, AS THE SERVER SEES IT.
#[sqlx::test(migrations = false)]
async fn the_executor_platform_role_holds_exactly_its_measured_lease_surface(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str().to_owned();
    let database = query(&admin, "SELECT current_database()");
    reset(&admin, &database);

    let stable = WorkloadRoleFamily::ExecutorPlatform.acl_role();
    let login = generation(WorkloadRoleFamily::ExecutorPlatform, &database);
    // The convergent grant batch, applied TWICE: a replay against an already
    // correct database must be a no-op, not a widening and not an error.
    let surface = sql::grant_executor_platform_surface_sql("wamn_run");
    run_admin(&admin, "converge the executor-platform surface", &surface);
    run_admin(&admin, "replay the convergent surface", &surface);
    run_admin(
        &admin,
        "prepare the executor-platform generation",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ExecutorPlatform,
            &database,
            &login,
            GENERATION_PW,
        ),
    );
    run_admin(
        &admin,
        "member the family into the platform base grants arm",
        &sql::platform_group_membership_sql(WorkloadRoleFamily::ExecutorPlatform),
    );

    let expected = load_role_grants(&admin, stable).await;
    let direct: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (
          SELECT FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_namespace c CROSS JOIN LATERAL aclexplode(c.nspacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_proc c CROSS JOIN LATERAL aclexplode(c.proacl) a WHERE a.grantee=$1::text::regrole
          UNION ALL SELECT FROM pg_database c CROSS JOIN LATERAL aclexplode(c.datacl) a WHERE a.grantee=$1::text::regrole
            AND (c.datname<>current_database() OR a.privilege_type<>'CONNECT' OR a.is_grantable))"
    ).bind(&login).fetch_one(&pool).await.unwrap();
    assert!(
        direct,
        "the login has only CONNECT directly and inherits its operation privileges"
    );

    // --- 2. CONVERGENCE: a widening applied out of band is REMOVED ----------
    run_admin(
        &admin,
        "widen the role out of band",
        &format!(
            "GRANT INSERT ON TABLE wamn_run.runs TO {stable};\n\
             GRANT UPDATE (input_json) ON TABLE wamn_run.runs TO {stable};\n\
             GRANT SELECT ON TABLE catalog.wiring_tombstones TO {stable};\n\
             GRANT EXECUTE ON FUNCTION \
               wamn_run.guard_event_lineage_immutable() TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    assert_eq!(
        load_role_grants(&admin, stable).await,
        expected,
        "the surface ADDS but does not NARROW: a hand-granted table, column and \
         routine privilege survived a re-apply"
    );

    // --- 3. THE EFFECTIVE PRIVILEGE, AS THE GENERATION -----------------------
    let mut probes = format!(
        "DO $probe$ DECLARE r text := '{login}'; BEGIN \
           ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = r), \
             'the probe role is superuser or bypasses RLS — that masks every denial below'; \
           ASSERT (SELECT rolcanlogin FROM pg_roles WHERE rolname = r), \
             'the prepared generation must be able to authenticate'; \
           ASSERT has_schema_privilege(r, 'catalog', 'USAGE'), 'no catalog USAGE'; \
           ASSERT has_schema_privilege(r, 'wamn_run', 'USAGE'), 'no run-plane USAGE'; \
           ASSERT NOT has_schema_privilege(r, 'wamn_authority', 'USAGE'), \
             'an index expression is a resolved node tree: schema USAGE is not needed'; \
           ASSERT has_function_privilege(r, 'wamn_authority.project_key(text)', 'EXECUTE'), \
             'the runs_tkey expression index makes this EXECUTE load bearing for UPDATE'; \n"
    );
    for relation in sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        writeln!(
            probes,
            "  ASSERT has_table_privilege(r, 'catalog.{relation}', 'SELECT'), \
               'cannot read catalog.{relation}'; \
             ASSERT NOT has_table_privilege(r, 'catalog.{relation}', 'INSERT') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'UPDATE') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'DELETE'), \
               'the executor writes nothing in the catalog plane'; "
        )
        .expect("writing to a String cannot fail");
    }
    // The catalog relations it must NOT reach at all, and the run-plane
    // relations that belong to other families.
    for relation in ["catalog.packages", "catalog.wiring_tombstones"] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            writeln!(
                probes,
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the executor holds {privilege} on {relation}'; "
            )
            .expect("writing to a String cannot fail");
        }
    }
    probes.push_str(
        "  ASSERT has_table_privilege(r, 'wamn_run.runs', 'SELECT'), \
             'the fence locks the run with SELECT r.*'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.runs', 'INSERT'), \
           'the executor admits only the event run columns, never a whole row'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.runs', 'DELETE'), \
           'run history pruning is retention''s'; \
         ASSERT has_table_privilege(r, 'wamn_run.run_queue', 'SELECT'), 'no queue read'; \
         ASSERT has_table_privilege(r, 'wamn_run.run_queue', 'DELETE'), 'no dequeue'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.run_queue', 'INSERT'), \
           'an event admission enqueues by project and run id only'; \n",
    );
    for column in sql::EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS {
        writeln!(
            probes,
            "  ASSERT has_column_privilege(r, 'wamn_run.runs', '{column}', 'INSERT'), \
               'the executor cannot admit an event run: runs.{column}'; "
        )
        .expect("writing to a String cannot fail");
    }
    let columns = query(
        &admin,
        "SELECT attname FROM pg_attribute WHERE attrelid='wamn_run.runs'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum",
    );
    for column in columns
        .lines()
        .filter(|column| !sql::EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS.contains(column))
    {
        writeln!(
            probes,
            "  ASSERT NOT has_column_privilege(r, 'wamn_run.runs', '{column}', 'INSERT'), \
               'the executor can admit runs.{column}, which no event run carries'; "
        )
        .expect("writing to a String cannot fail");
    }
    // COLUMN GRAIN, from the server, both ways. `has_column_privilege` answers
    // TRUE for a column reachable through a TABLE-level grant, so the FALSE arms
    // are what show the UPDATE never became blanket.
    for column in sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS {
        writeln!(
            probes,
            "  ASSERT has_column_privilege(r, 'wamn_run.runs', '{column}', 'UPDATE'), \
               'the executor cannot write runs.{column}, which one of its six \
                UPDATE statements sets'; "
        )
        .expect("writing to a String cannot fail");
    }
    for column in columns
        .lines()
        .filter(|column| !sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS.contains(column))
    {
        writeln!(
            probes,
            "  ASSERT NOT has_column_privilege(r, 'wamn_run.runs', '{column}', 'UPDATE'), \
               'the executor can rewrite runs.{column}: the admission pins and the \
                frozen wiring identity are not a lease''s to move'; "
        )
        .expect("writing to a String cannot fail");
    }
    for column in sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS {
        writeln!(
            probes,
            "  ASSERT has_column_privilege(r, 'wamn_run.run_queue', '{column}', 'UPDATE'), \
               'the executor cannot write run_queue.{column}'; "
        )
        .expect("writing to a String cannot fail");
    }
    let columns = query(
        &admin,
        "SELECT attname FROM pg_attribute WHERE attrelid='wamn_run.run_queue'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum",
    );
    for column in columns
        .lines()
        .filter(|column| !sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS.contains(column))
    {
        writeln!(
            probes,
            "  ASSERT NOT has_column_privilege(r, 'wamn_run.run_queue', '{column}', 'UPDATE'), \
               'the executor can move run_queue.{column}: the FIFO position and the \
                crash budget belong to admission'; "
        )
        .expect("writing to a String cannot fail");
    }
    probes.push_str("END $probe$;\n");
    run_admin(&admin, "the effective-privilege probes", &probes);

    // --- 4. THE POST-STATE, IN THE GENERATION'S OWN SESSION -----------------
    //
    // A privilege catalog says what PostgreSQL believes; this says what it does.
    // Both directions are read back, because under FORCE RLS a statement that
    // matched nothing and a statement that was refused look identical from the
    // outside.
    run_admin(
        &admin,
        "seed one run the executor may lease",
        &format!(
            "INSERT INTO catalog.packages ( project_env_key, package_id, package_version, artifact_digest) VALUES ( 't1', 'widgets', '1.0.0', 'sha256:{package_hash}');\n\
             \n\
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}') ON CONFLICT DO NOTHING; INSERT INTO wamn_run.runs (project_env_key, run_id, package_id, environment, status, trigger_source, wiring_id, wiring_version, wiring_hash, input_json, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json, manifest_digest) VALUES ('t1', 'r1', 'widgets', 'dev', 'dispatched', 'manual', 'w', 1, 'sha256:{hash}', '{{\"a\":1}}', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{{}}', 'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a');\n",
            hash = "c".repeat(64),
            package_hash = "d".repeat(64),
        ),
    );
    let as_login = role_url(&admin, &login, GENERATION_PW);
    assert_eq!(
        sqlstate(
            &as_login,
            "BEGIN; SET LOCAL app.project_env_key = 't1'; \
             UPDATE wamn_run.runs SET status = 'running' WHERE run_id = 'r1'; COMMIT;\n",
        ),
        None,
        "a granted column must be writable by the generation itself"
    );
    assert_eq!(
        query(
            &admin,
            "SELECT status FROM wamn_run.runs WHERE run_id = 'r1'"
        ),
        "running",
        "the granted UPDATE reported success and moved nothing: the post-state is \
         the only thing that separates a write from a no-op under FORCE RLS"
    );
    assert_eq!(
        sqlstate(
            &as_login,
            "BEGIN; SET LOCAL app.project_env_key = 't1'; \
             UPDATE wamn_run.runs SET input_json = '{\"b\":2}' WHERE run_id = 'r1'; COMMIT;\n",
        )
        .as_deref(),
        Some("42501"),
        "the executor rewrote the authoritative input: column-grain UPDATE is the \
         only bound available, because the permissive platform base grants arm is \
         USING (true) and reaches every project"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&query(
            &admin,
            "SELECT input_json FROM wamn_run.runs WHERE run_id='r1'"
        ))
        .unwrap()["a"],
        1
    );
}

/// THE CALLABLE-HTTP ADMITTER SURFACE, AS THE SERVER SEES IT.
///
/// Catalog reads, projected connection reads, permission reads, and no run plane
/// stay unchanged when provisioning removes an unauthorized privilege.
#[sqlx::test(migrations = false)]
async fn the_http_admitter_role_adds_exactly_the_fresh_permission_reads(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str().to_owned();
    let database = query(&admin, "SELECT current_database()");
    reset(&admin, &database);

    let stable = WorkloadRoleFamily::HttpAdmitter.acl_role();
    let login = generation(WorkloadRoleFamily::HttpAdmitter, &database);
    let surface = sql::grant_http_admitter_surface_sql("wamn_run");
    run_admin(&admin, "converge the callable-HTTP surface", &surface);
    run_admin(&admin, "replay the convergent surface", &surface);
    run_admin(
        &admin,
        "prepare the callable-HTTP generation",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            &database,
            &login,
            GENERATION_PW,
        ),
    );

    // Convergence, same as the executor arm: a run-plane privilege handed to
    // this family out of band is REMOVED by a re-apply, not merely unmentioned.
    run_admin(
        &admin,
        "widen the role onto adjacent authority",
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run TO {stable};\n\
             GRANT SELECT ON TABLE wamn_run.runs TO {stable};\n\
             GRANT UPDATE (status) ON TABLE wamn_run.runs TO {stable};\n\
             GRANT SELECT ON TABLE platform.role TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    // The installed objects supply the observations. Read declarations supply
    // the allowed SELECTs; no observed grant defines its own expected result.
    let (can_login, elevated): (bool, bool) = sqlx::query_as(
        "SELECT rolcanlogin,rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=$1",
    )
    .bind(&login)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(can_login && !elevated);
    let allowed: std::collections::BTreeSet<_> = sql::http_admitter_relations().collect();
    let tables: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,p.privilege_type,has_table_privilege($1,c.oid,p.privilege_type)
         FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         CROSS JOIN LATERAL aclexplode(acldefault('r',c.relowner)) p
         WHERE c.relkind IN ('r','p','v','m','f')
           AND n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (schema, relation, privilege, permitted) in tables {
        assert_eq!(
            permitted,
            privilege == "SELECT" && allowed.contains(&(schema.as_str(), relation.as_str())),
            "{privilege} on {schema}.{relation}"
        );
    }
    let columns: Vec<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,a.attname,p.privilege,
                has_column_privilege($1,c.oid,a.attnum,p.privilege)
         FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid
         JOIN pg_namespace n ON n.oid=c.relnamespace
         CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','REFERENCES']) p(privilege)
         WHERE a.attnum>0 AND NOT a.attisdropped AND c.relkind IN ('r','p','v','m','f')
           AND n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (schema, relation, column, privilege, permitted) in columns {
        let readable = allowed.contains(&(schema.as_str(), relation.as_str()))
            || (schema == "catalog"
                && relation == "workflows"
                && sql::HTTP_ADMITTER_WORKFLOW_COLUMNS.contains(&column.as_str()));
        assert_eq!(
            permitted,
            privilege == "SELECT" && readable,
            "{privilege} on {schema}.{relation}.{column}"
        );
    }
    let schemas: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT nspname,has_schema_privilege($1,oid,'USAGE'),has_schema_privilege($1,oid,'CREATE')
         FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname<>'information_schema'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (schema, usage, create) in schemas {
        assert_eq!(
            usage,
            schema == "public" || sql::HTTP_ADMITTER_SCHEMAS.contains(&schema.as_str()),
            "schema access: {schema}"
        );
        assert!(!create, "can create objects in {schema}");
    }
    let sequences: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,has_sequence_privilege($1,c.oid,'USAGE,SELECT,UPDATE')
         FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE c.relkind='S' AND n.nspname NOT LIKE 'pg_%'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (schema, sequence, permitted) in sequences {
        assert!(!permitted, "reaches sequence {schema}.{sequence}");
    }
    let functions: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT p.oid::regprocedure::text,has_function_privilege($1,p.oid,'EXECUTE'),
                EXISTS(SELECT FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a
                       WHERE a.grantee=0 AND a.privilege_type='EXECUTE')
         FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
         WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'",
    )
    .bind(&login)
    .fetch_all(&pool)
    .await
    .unwrap();
    for (function, permitted, public) in functions {
        assert_eq!(
            permitted, public,
            "routine authority beyond PUBLIC: {function}"
        );
    }

    // --- THE POST-STATE, IN THE GENERATION'S OWN SESSION --------------------
    //
    // Every probe above is a privilege-catalog question, and the catalog is
    // BLIND TO RLS: `has_table_privilege` answers TRUE for a role whose every
    // SELECT returns zero rows because no policy matched it. All eight relations
    // carry FORCE RLS with a restrictive project base grants keyed on the GUEST login
    // pattern, so a non-guest family reads through the permissive
    // `TO wamn_platform` arm or it reads nothing at all — and reading nothing
    // RAISES NOTHING. This arm is therefore the only thing here that separates
    // an ISOLATED credential from a SILENTLY BLIND one (`wamn-0h0g.22.11`).
    //
    // Unlike the executor arm above, this one does NOT re-apply
    // `platform_group_membership_sql`. The edge is converged by
    // `test_database::login_sql` itself, through
    // `normalize_workload_generation_membership_sql`, so re-applying it here
    // would MASK the day that stops being true — which is precisely the
    // regression the counts below exist to catch.
    let digest = format!("sha256:{}", "a".repeat(64));
    run_admin(
        &admin,
        "seed one readable fact in each catalog and fresh permission relation",
        &format!(
            "BEGIN;\n\
             SELECT set_config('app.user_id', '00000000-0000-4000-8000-000000000001', true), \
                    set_config('app.operation', 'admin:seed-family-surface-fixture', true);\n\
             INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( 'org-a','orders','1.0.0','{digest}');\n\
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) \
             VALUES ('org-a','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}');\n\
             INSERT INTO platform.integration (id,integration_type,name,status) \
             VALUES ('00000000-0000-4000-8000-000000000011','http','upstream','active');\n\
             INSERT INTO platform.connection (id,integration_id,integration_type,status) \
             VALUES ('00000000-0000-4000-8000-000000000012','00000000-0000-4000-8000-000000000011','http','active');\n\
             INSERT INTO catalog.connection_requirement (project_env_key,component_digest,package_id,name,interface,mode) \
             VALUES ('org-a','{digest}','orders','upstream','wamn:connection/http@0.1.0','write');\n\
             INSERT INTO catalog.release_connection (project_env_key,manifest_digest,connection_id,version) \
             VALUES ('org-a','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','00000000-0000-4000-8000-000000000012',1);\n\
             INSERT INTO catalog.connection_binding (project_env_key,manifest_digest,component_digest,package_id,name,connection_id) \
             VALUES ('org-a','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{digest}','orders','upstream','00000000-0000-4000-8000-000000000012');\n\
             INSERT INTO catalog.workflows \
               (project_env_key, environment, application, workflow_id, version, graph_json, \
                wiring_hash, trigger, execution_type, execution_role, created_by) \
             VALUES ('org-a', 'prod', 'orders', 'send', 1, '{{}}', '{digest}', \
                     'manual', 'role', 'widget-reader', \
                     '00000000-0000-4000-8000-000000000001');\n\
             SELECT set_config('app.user_id', '00000000-0000-4000-8000-000000000001', true), \
                    set_config('app.operation', 'admin:seed-family-surface-fixture', true);\n\
             INSERT INTO platform.role (name) \
             VALUES ('widget-reader');\n\
             INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ('00000000-0000-4000-8000-000000000001', 'user', 'caller@example.test');\n\
             INSERT INTO platform.user_role (user_id, role_name) \
             VALUES ('00000000-0000-4000-8000-000000000001', 'widget-reader');\n\
             INSERT INTO platform.permission (role_name, permission) \
             VALUES ('widget-reader', 'wamn-widgets:widget/get');\n\
             COMMIT;\n"
        ),
    );

    let as_login = role_url(&admin, &login, GENERATION_PW);
    assert!(
        !query(
            &as_login,
            "SELECT (rolsuper OR rolbypassrls)::text FROM pg_roles WHERE rolname = current_user"
        )
        .parse::<bool>()
        .unwrap(),
        "the reading session is superuser or bypasses RLS, which satisfies every \
         count below while establishing nothing"
    );
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        assert_eq!(
            query(
                &as_login,
                &format!("SELECT count(*) FROM catalog.{relation}")
            )
            .parse::<i64>()
            .unwrap(),
            1,
            "the callable-HTTP generation read the WRONG NUMBER of rows from \
             catalog.{relation}. Zero is the failure this arm exists to catch: the \
             row is seeded and the SELECT is granted, so zero means the project base grants \
             admitted nothing and the credential is isolated into blindness rather \
             than into authority"
        );
    }
    assert_eq!(
        query(
            &as_login,
            "SELECT count(graph_json) FROM catalog.workflows \
              WHERE project_env_key = 'org-a' AND environment = 'prod' \
                AND application = 'orders' AND workflow_id = 'send' AND version = 1"
        )
        .parse::<i64>()
        .unwrap(),
        1,
        "the callable-HTTP generation cannot read the workflow graph through its \
         column grant, or the project base grants admitted nothing"
    );
    assert_eq!(
        query(
            &as_login,
            "SELECT count(*) FROM platform.permission \
              WHERE role_name = 'widget-reader' \
                AND permission = 'wamn-widgets:widget/get'"
        )
        .parse::<i64>()
        .unwrap(),
        1,
        "the callable-HTTP generation cannot read the exact operation grant"
    );
    assert_eq!(
        query(
            &as_login,
            "SELECT p.permission FROM platform.\"user\" u \
             JOIN platform.user_role ur ON ur.user_id = u.id \
             JOIN platform.permission p ON p.role_name = ur.role_name \
             WHERE u.id = '00000000-0000-4000-8000-000000000001' \
               AND u.status = 'active'"
        ),
        "wamn-widgets:widget/get",
        "the callable-HTTP generation cannot read the user's fresh permission"
    );
}

#[sqlx::test(migrations = false)]
async fn control_routes_cannot_write_release_connections_or_bindings(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    run_admin(database.as_str(),"release connection grants",&format!(
        "{} DO $roles$ BEGIN IF to_regrole('wamn_app') IS NULL THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; IF to_regrole('wamn_control') IS NULL THEN CREATE ROLE wamn_control NOLOGIN NOINHERIT; END IF;  END $roles$; CREATE SCHEMA wamn_run; {} {} {} DO $$ DECLARE role_name text; relation text; privilege text; BEGIN
          FOREACH role_name IN ARRAY ARRAY['wamn_control','wamn_administration'] LOOP
            FOREACH relation IN ARRAY ARRAY['release_connection','connection_binding'] LOOP
              FOREACH privilege IN ARRAY ARRAY['INSERT','UPDATE','DELETE','TRUNCATE'] LOOP
                ASSERT NOT has_table_privilege(role_name,'catalog.'||relation,privilege),role_name||':'||relation||':'||privilege;
              END LOOP;
              ASSERT NOT has_any_column_privilege(role_name,'catalog.'||relation,'INSERT,UPDATE'),role_name||':'||relation;
            END LOOP;
          END LOOP;
        END $$;",
        sql::ensure_db_owner_role_sql(),CATALOG_SCHEMA,APP_SCHEMA,sql::grant_administration_surface_sql("wamn_run"),
    ));
}
