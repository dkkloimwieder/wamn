//! Live proof that the executor-platform, event-materializer, and callable-HTTP
//! admitter families
//! hold EXACTLY their measured production surfaces (`wamn-0h0g.22.37`).
//!
//! The proof is the SERVER'S OWN ANSWER — `aclexplode` over `pg_class.relacl`,
//! `pg_attribute.attacl`, `pg_namespace.nspacl` and `pg_proc.proacl`, plus
//! `has_schema_privilege` / `has_table_privilege` / `has_function_privilege` —
//! never the text of a statement. This crate EMITS that text, so a text
//! assertion would be a builder checked against itself. The byte-exact string
//! pins live beside the builders in `src/sql.rs`, where they belong: a mutant
//! that dies only here ships green in the ordinary sweep.
//!
//! Every denial is measured from a session authenticated AS the family's own
//! generation login, and that session first asserts it is neither `rolsuper`
//! nor `rolbypassrls` — a superuser fixture satisfies every probe below while
//! proving nothing.
//!
//! # The column-exactness arm
//!
//! `wamn_run.runs` is UPDATEd by SIX distinct executor statements and the grant
//! is the UNION of the columns they write, so the ACL inventory alone could pass
//! a column list that is exact but unusable. The last arm therefore connects as
//! the generation and asserts the POST-STATE both ways: a granted column moves
//! the row, an ungranted one raises `42501` AND leaves the row unmoved. Under
//! FORCE RLS an "it failed" assertion cannot tell REFUSED from MATCHED-NOTHING,
//! which is why the post-state is read back in both directions.
//!
//! Set `WAMN_FAMILY_SURFACE_PG_URL` to a throwaway superuser URL to arm it; both
//! gates print a skip line and return when it is unset.
//!
//! ```bash
//! docker run -d --name wamn-family-pg -e POSTGRES_PASSWORD=probe \
//!   -p 127.0.0.1:5437:5432 postgres:18
//! until psql postgres://postgres:probe@localhost:5437/postgres -Atqc 'select 1'; do :; done
//! WAMN_FAMILY_SURFACE_PG_URL=postgres://postgres:probe@localhost:5437/postgres \
//!   cargo test -p wamn-control-provision --test family_surface_grants
//! docker rm -f wamn-family-pg      # BY EXPLICIT NAME. Never prune.
//! ```

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;

use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{WorkloadRoleScope, workload_generation_role};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};

const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

const ORG: &str = "acme";
const PROJECT: &str = "billing";
const ENV: &str = "dev";
const GENERATION_PW: &str = "wamn_family_surface_pw";

/// All gates rebuild the SAME schemas and the SAME cluster-global roles, so
/// they must not interleave. Tests inside one binary run in parallel threads by
/// default; this makes each one's reset a reset rather than a race.
static SCHEMA: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal is asserted on `42501` rather than on an error string.
fn psql(url: &str, script: &str) -> (bool, String, String) {
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
    workload_generation_role(
        family,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENV,
            database,
        },
        CredentialGeneration::A,
    )
    .expect("these families take a project-environment scope")
}

/// The admin URL with its userinfo replaced by one role's login.
fn role_url(admin_url: &str, role: &str, password: &str) -> String {
    let mut url = Url::parse(admin_url).expect("WAMN_FAMILY_SURFACE_PG_URL is a URL");
    url.set_username(role).expect("set the probe role");
    url.set_password(Some(password)).expect("set the password");
    url.into()
}

/// Rebuild the two schemas and every cluster-global role they and the families
/// name, so nothing this gate measures survives from a previous run.
///
/// `DROP OWNED BY` before `DROP ROLE`, inside an existence check: a leftover
/// HEALTHY role satisfies `ensure_workload_acl_role_sql`'s `IF NOT EXISTS` and
/// would mask a mutated builder — which is the exact failure the mutant arms of
/// this bead depend on being impossible.
fn reset(admin_url: &str, database: &str) {
    let mut roles = vec![
        generation(WorkloadRoleFamily::ExecutorPlatform, database),
        generation(WorkloadRoleFamily::EventMaterializer, database),
        generation(WorkloadRoleFamily::HttpAdmitter, database),
        WorkloadRoleFamily::ExecutorPlatform.acl_role().to_owned(),
        WorkloadRoleFamily::EventMaterializer.acl_role().to_owned(),
        WorkloadRoleFamily::HttpAdmitter.acl_role().to_owned(),
    ];
    // The roles the two artifacts GRANT to or CREATE. A `GRANT … TO` a missing
    // role fails the whole apply, so the ones the files do not create themselves
    // are re-minted below.
    roles.extend(
        [
            "wamn_app",
            "wamn_scenario_author",
            "wamn_control_author",
            "wamn_effect_writer",
            "wamn_run_retention",
            "wamn_platform",
        ]
        .map(str::to_owned),
    );
    let mut script = String::from(
        "DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS app_system CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n",
    );
    for role in roles {
        script.push_str(&format!(
            "DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $reset$;\n"
        ));
    }
    script.push_str(
        "CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;\n\
         CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
         CREATE ROLE wamn_control_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
         CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS;\n",
    );
    run_admin(admin_url, "reset the family-surface fixture", &script);
    for artifact in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        run_admin(admin_url, "apply a run-plane artifact", artifact);
    }
}

#[test]
fn the_event_materializer_role_holds_exactly_its_two_catalog_reads() {
    let Ok(admin) = std::env::var("WAMN_FAMILY_SURFACE_PG_URL") else {
        eprintln!(
            "skipping the_event_materializer_role_holds_exactly_its_two_catalog_reads \
             (set WAMN_FAMILY_SURFACE_PG_URL to run)"
        );
        return;
    };
    let _serialized = SCHEMA.lock().unwrap_or_else(|poison| poison.into_inner());
    let database = query(&admin, "SELECT current_database()");
    reset(&admin, &database);

    let stable = WorkloadRoleFamily::EventMaterializer.acl_role();
    let login = generation(WorkloadRoleFamily::EventMaterializer, &database);
    let surface = sql::grant_event_materializer_surface_sql("wamn_run");
    run_admin(&admin, "converge the event-materializer surface", &surface);
    run_admin(&admin, "replay the convergent surface", &surface);
    run_admin(
        &admin,
        "prepare the event-materializer generation",
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::EventMaterializer,
            &database,
            &login,
            GENERATION_PW,
            "2099-01-01T00:00:00Z",
        ),
    );

    let expected = vec![
        "relation|catalog|event_registrations|SELECT".to_owned(),
        "relation|catalog|packages|SELECT".to_owned(),
        "schema|catalog|catalog|USAGE".to_owned(),
    ];
    assert_eq!(inventory(&admin, stable), expected);
    assert_eq!(
        inventory(&admin, &login),
        vec![format!("database||{database}|CONNECT")],
        "the materializer generation carries a direct grant beyond CONNECT"
    );

    run_admin(
        &admin,
        "widen the materializer role out of band",
        &format!(
            "GRANT SELECT ON TABLE catalog.package_migrations TO {stable}; \
             GRANT SELECT ON TABLE app_system.permissions TO {stable}; \
             GRANT USAGE ON SCHEMA wamn_run TO {stable}; \
             GRANT SELECT ON TABLE wamn_run.runs TO {stable};"
        ),
    );
    run_admin(
        &admin,
        "re-converge the event-materializer surface",
        &surface,
    );
    assert_eq!(
        inventory(&admin, stable),
        expected,
        "an adjacent catalog or run-plane grant survived convergence"
    );

    run_admin(
        &admin,
        "seed materializer registration inputs",
        "INSERT INTO catalog.packages \
           (tenant_id, package_id, package_version, manifest_sha256) VALUES \
           ('t1', 'receiving', '1.0.0', \
            'sha256:0000000000000000000000000000000000000000000000000000000000000000'), \
           ('t2', 'foreign', '1.0.0', \
            'sha256:1111111111111111111111111111111111111111111111111111111111111111'); \
         INSERT INTO catalog.event_registrations \
           (tenant_id, package_id, registration_id, entity_id, registration) VALUES \
           ('t1', 'overlay', 'quality.create_inspection', 'receipt', '{}'), \
           ('t2', 'foreign', 'foreign', 'receipt', '{}');",
    );
    let as_materializer = role_url(&admin, &login, GENERATION_PW);
    assert_eq!(
        query(
            &as_materializer,
            "WITH claim AS (SELECT set_config('app.tenant', 't1', false)) \
             SELECT string_agg(package_id || '@' || package_version, ',' ORDER BY package_id) \
               FROM catalog.packages, claim \
              WHERE tenant_id = current_setting('app.tenant', true)"
        ),
        "receiving@1.0.0"
    );
    assert_eq!(
        query(
            &as_materializer,
            "WITH claim AS (SELECT set_config('app.tenant', 't1', false)) \
             SELECT string_agg(package_id || '::' || registration_id, ',' ORDER BY package_id) \
               FROM catalog.event_registrations, claim \
              WHERE tenant_id = current_setting('app.tenant', true)"
        ),
        "overlay::quality.create_inspection"
    );
    assert_eq!(
        sqlstate(
            &as_materializer,
            "INSERT INTO catalog.event_registrations \
               (tenant_id, package_id, registration_id, entity_id, registration) \
             VALUES ('t1', 'overlay', 'forged', 'receipt', '{}');"
        )
        .as_deref(),
        Some("42501")
    );
}

/// The whole ACL the named role holds in this database, as the server reports
/// it — relation, column, schema and ROUTINE entries alike, so a grant of any
/// grain lands in the comparison.
fn inventory(admin_url: &str, role: &str) -> Vec<String> {
    query(
        admin_url,
        &format!(
            "SELECT kind || '|' || sch || '|' || obj || '|' || priv FROM ( \
               SELECT 'relation' AS kind, n.nspname::text AS sch, c.relname::text AS obj, \
                      x.privilege_type::text AS priv \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(c.relacl) x \
                WHERE x.grantee = '{role}'::regrole \
               UNION ALL \
               SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                      x.privilege_type::text \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(a.attacl) x \
                WHERE x.grantee = '{role}'::regrole \
               UNION ALL \
               SELECT 'schema', n.nspname::text, n.nspname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
                WHERE x.grantee = '{role}'::regrole \
               UNION ALL \
               SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_proc p \
                 JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                 CROSS JOIN LATERAL aclexplode(p.proacl) x \
                WHERE x.grantee = '{role}'::regrole \
               UNION ALL \
               SELECT 'database', '', d.datname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
                WHERE x.grantee = '{role}'::regrole \
             ) q ORDER BY 1"
        ),
    )
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

/// The expected inventory, spelled OUT rather than derived from the builder.
///
/// Deriving it from `sql::EXECUTOR_PLATFORM_*` would move both sides together
/// when a column is added, which is the tautology this branch has paid for
/// before. The relation and column names are the ones read out of the six
/// `UPDATE runs` statements and the four wiring-resolution `SELECT`s; if one of
/// those statements changes, this list has to change with it, deliberately.
fn expected_executor_inventory() -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for column in [
        "caller_http_status",
        "caller_outcome_hash",
        "caller_outcome_json",
        "caller_outcome_kind",
        "caller_release_node_id",
        "caller_released_at",
        "fail_kind",
        "manifest_digest",
        "result_json",
        "state_json",
        "status",
        "terminal_reason",
        "updated_at",
    ] {
        rows.push(format!("column|wamn_run|runs.{column}|UPDATE"));
    }
    for column in [
        "attempts",
        "lease_expires_at",
        "lease_generation",
        "lease_owner",
    ] {
        rows.push(format!("column|wamn_run|run_queue.{column}|UPDATE"));
    }
    for relation in [
        "component_library",
        "connection_bindings",
        "connection_generations",
        "connection_instances",
        "connection_requirements",
        "effective_release_heads",
        "effective_release_packages",
        "release_components",
        "release_manifest_v3_snapshots",
        "wiring_activation",
        "wiring_tombstones",
        "wirings",
    ] {
        rows.push(format!("relation|catalog|{relation}|SELECT"));
    }
    rows.push("relation|wamn_run|effect_attempts|SELECT".to_owned());
    rows.push("relation|wamn_run|run_queue|DELETE".to_owned());
    rows.push("relation|wamn_run|run_queue|SELECT".to_owned());
    rows.push("relation|wamn_run|runs|SELECT".to_owned());
    rows.push("routine|wamn_authority|tenant_key|EXECUTE".to_owned());
    rows.push("routine|wamn_run|require_executor_platform_authority|EXECUTE".to_owned());
    rows.push("schema|catalog|catalog|USAGE".to_owned());
    rows.push("schema|wamn_run|wamn_run|USAGE".to_owned());
    rows.sort();
    rows
}

/// THE EXECUTOR-PLATFORM SURFACE, AS THE SERVER SEES IT.
#[test]
fn the_executor_platform_role_holds_exactly_its_measured_claim_surface() {
    let Ok(admin) = std::env::var("WAMN_FAMILY_SURFACE_PG_URL") else {
        eprintln!(
            "skipping the_executor_platform_role_holds_exactly_its_measured_claim_surface \
             (set WAMN_FAMILY_SURFACE_PG_URL to run)"
        );
        return;
    };
    let _serialized = SCHEMA.lock().unwrap_or_else(|poison| poison.into_inner());
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
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::ExecutorPlatform,
            &database,
            &login,
            GENERATION_PW,
            "2099-01-01T00:00:00Z",
        ),
    );
    run_admin(
        &admin,
        "member the family into the platform floor arm",
        &sql::platform_group_membership_sql(WorkloadRoleFamily::ExecutorPlatform),
    );

    // --- 1. THE WHOLE ACL, BY EQUALITY --------------------------------------
    //
    // This is also the ONLY place the `require_executor_platform_authority`
    // EXECUTE is provable. `deploy/sql/run-state.sql` never revokes PUBLIC on
    // that guard, so `has_function_privilege` answers TRUE for every role in the
    // cluster and a probe over it would pass with the grant deleted. The
    // `routine|wamn_run|require_executor_platform_authority|EXECUTE` row below
    // comes from `pg_proc.proacl` and names this family explicitly, so deleting
    // the grant fails HERE.
    assert_eq!(
        inventory(&admin, stable),
        expected_executor_inventory(),
        "the stable executor-platform role's aclexplode inventory is not the \
         exact measured claim surface"
    );
    // The generation itself holds only CONNECT, directly: its authority is
    // INHERITED, so a direct grant here would be a second, unmanaged path.
    assert_eq!(
        inventory(&admin, &login),
        vec![format!("database||{database}|CONNECT")],
        "the executor-platform generation carries a direct grant beyond CONNECT"
    );

    // --- 2. CONVERGENCE: a widening applied out of band is REMOVED ----------
    run_admin(
        &admin,
        "widen the role out of band",
        &format!(
            "GRANT INSERT ON TABLE wamn_run.runs TO {stable};\n\
             GRANT UPDATE (input_json) ON TABLE wamn_run.runs TO {stable};\n\
             GRANT SELECT ON TABLE wamn_run.environment_policies TO {stable};\n\
             GRANT EXECUTE ON FUNCTION \
               wamn_run.require_management_admission_authority() TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    assert_eq!(
        inventory(&admin, stable),
        expected_executor_inventory(),
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
           ASSERT has_function_privilege(r, 'wamn_authority.tenant_key(text)', 'EXECUTE'), \
             'the runs_tkey expression index makes this EXECUTE load bearing for UPDATE'; \n"
    );
    for relation in sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        probes.push_str(&format!(
            "  ASSERT has_table_privilege(r, 'catalog.{relation}', 'SELECT'), \
               'cannot read catalog.{relation}'; \
             ASSERT NOT has_table_privilege(r, 'catalog.{relation}', 'INSERT') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'UPDATE') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'DELETE'), \
               'the executor writes nothing in the catalog plane'; \n"
        ));
    }
    // The catalog relations it must NOT reach at all, and the run-plane
    // relations that belong to other families.
    for relation in [
        "catalog.packages",
        "catalog.package_migrations",
        "catalog.effective_releases",
        "catalog.event_registrations",
        "wamn_run.environment_policies",
        "wamn_run.operator_run_actions",
        "wamn_run.effect_attempt_dispatches",
        "wamn_run.effect_attempt_outcomes",
    ] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            probes.push_str(&format!(
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the executor holds {privilege} on {relation}'; \n"
            ));
        }
    }
    probes.push_str(
        "  ASSERT has_table_privilege(r, 'wamn_run.runs', 'SELECT'), \
             'the fence locks the run with SELECT r.*'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.runs', 'INSERT'), \
           'admission is the management admitter''s, never the executor''s'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.runs', 'DELETE'), \
           'run history pruning is retention''s'; \
         ASSERT has_table_privilege(r, 'wamn_run.run_queue', 'SELECT'), 'no queue read'; \
         ASSERT has_table_privilege(r, 'wamn_run.run_queue', 'DELETE'), 'no dequeue'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.run_queue', 'INSERT'), \
           'enqueue is admission''s'; \
         ASSERT has_table_privilege(r, 'wamn_run.effect_attempts', 'SELECT'), \
           'the claim asks whether an effect attempt exists'; \
         ASSERT NOT has_table_privilege(r, 'wamn_run.effect_attempts', 'INSERT'), \
           'the ledger is the effect writer''s to append to'; \n",
    );
    // COLUMN GRAIN, from the server, both ways. `has_column_privilege` answers
    // TRUE for a column reachable through a TABLE-level grant, so the FALSE arms
    // are what prove the UPDATE never became blanket.
    for column in sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS {
        probes.push_str(&format!(
            "  ASSERT has_column_privilege(r, 'wamn_run.runs', '{column}', 'UPDATE'), \
               'the executor cannot write runs.{column}, which one of its six \
                UPDATE statements sets'; \n"
        ));
    }
    for column in [
        "input_json",
        "binding_world_json",
        "durability_class",
        "wiring_id",
        "wiring_version",
        "wiring_hash",
        "package_id",
        "effective_release_id",
        "environment",
        "tenant_id",
        "run_id",
        "capture_mode",
        "idempotency_key",
        "invocation_context",
        "run_deadline_at",
    ] {
        probes.push_str(&format!(
            "  ASSERT NOT has_column_privilege(r, 'wamn_run.runs', '{column}', 'UPDATE'), \
               'the executor can rewrite runs.{column}: the admission pins and the \
                frozen wiring identity are not a claim''s to move'; \n"
        ));
    }
    for column in sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS {
        probes.push_str(&format!(
            "  ASSERT has_column_privilege(r, 'wamn_run.run_queue', '{column}', 'UPDATE'), \
               'the executor cannot write run_queue.{column}'; \n"
        ));
    }
    for column in [
        "available_at",
        "priority",
        "stream_seq",
        "max_attempts",
        "enqueued_at",
    ] {
        probes.push_str(&format!(
            "  ASSERT NOT has_column_privilege(r, 'wamn_run.run_queue', '{column}', 'UPDATE'), \
               'the executor can move run_queue.{column}: the FIFO position and the \
                crash budget belong to admission'; \n"
        ));
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
        "seed one run the executor may claim",
        &format!(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('t1', 'receiving', '1.0.0', 'sha256:{package_hash}');\n\
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment) \
             VALUES ('t1', 1, 'dev');\n\
             INSERT INTO wamn_run.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             VALUES ('t1', 'dev', 'standard');\n\
             INSERT INTO wamn_run.runs \
               (tenant_id, run_id, package_id, effective_release_id, environment, status, \
                trigger_source, wiring_id, wiring_version, wiring_hash, \
                binding_world_json, input_json) \
             VALUES ('t1', 'r1', 'receiving', 1, 'dev', 'dispatched', 'internal', \
                     'w', 1, 'sha256:{hash}', '[]', '{{\"a\":1}}');\n",
            hash = "c".repeat(64),
            package_hash = "d".repeat(64),
        ),
    );
    let as_login = role_url(&admin, &login, GENERATION_PW);
    assert_eq!(
        sqlstate(
            &as_login,
            "BEGIN; SET LOCAL app.tenant = 't1'; \
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
            "BEGIN; SET LOCAL app.tenant = 't1'; \
             UPDATE wamn_run.runs SET input_json = '{\"b\":2}' WHERE run_id = 'r1'; COMMIT;\n",
        )
        .as_deref(),
        Some("42501"),
        "the executor rewrote the authoritative input: column-grain UPDATE is the \
         only bound available, because the permissive platform floor arm is \
         USING (true) and reaches every tenant"
    );
    assert_eq!(
        query(
            &admin,
            "SELECT input_json::text FROM wamn_run.runs WHERE run_id = 'r1'"
        ),
        "{\"a\": 1}",
        "the refused UPDATE still moved the row"
    );
}

/// THE CALLABLE-HTTP ADMITTER SURFACE, AS THE SERVER SEES IT.
///
/// Seven catalog relations, the exact operation-grant relation, and no run plane
/// — asserted by EQUALITY over the whole inventory, so acquiring any adjacent
/// authority fails here rather than at the next incident.
#[test]
fn the_http_admitter_role_adds_exactly_the_operation_grant_read() {
    let Ok(admin) = std::env::var("WAMN_FAMILY_SURFACE_PG_URL") else {
        eprintln!(
            "skipping the_http_admitter_role_adds_exactly_the_operation_grant_read \
             (set WAMN_FAMILY_SURFACE_PG_URL to run)"
        );
        return;
    };
    let _serialized = SCHEMA.lock().unwrap_or_else(|poison| poison.into_inner());
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
        &sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::HttpAdmitter,
            &database,
            &login,
            GENERATION_PW,
            "2099-01-01T00:00:00Z",
        ),
    );

    let mut expected: Vec<String> = sql::HTTP_ADMITTER_CATALOG_RELATIONS
        .iter()
        .map(|relation| format!("relation|catalog|{relation}|SELECT"))
        .collect();
    expected.push("relation|app_system|permissions|SELECT".to_owned());
    expected.push("schema|app_system|app_system|USAGE".to_owned());
    expected.push("schema|catalog|catalog|USAGE".to_owned());
    expected.sort();
    assert_eq!(
        inventory(&admin, stable),
        expected,
        "the callable-HTTP admitter's aclexplode inventory is not exactly USAGE \
         on catalog and app_system plus the eight SELECTs its host reads"
    );
    // MEASURED, and the reason the probe block below cannot assert a NEGATIVE
    // for `require_executor_platform_authority`: `deploy/sql/run-state.sql`
    // creates the two `require_*_authority` guards and never revokes PUBLIC, so
    // `has_function_privilege` answers TRUE for every role in the cluster. The
    // inventory equality above is what proves this family holds no EXPLICIT
    // routine grant; if PUBLIC is ever revoked, this assertion fails and the
    // negative becomes assertable.
    assert_eq!(
        query(
            &admin,
            "SELECT proacl IS NULL OR EXISTS ( \
               SELECT 1 FROM aclexplode(proacl) x WHERE x.grantee = 0 \
                 AND x.privilege_type = 'EXECUTE') \
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_run' \
                AND p.proname = 'require_executor_platform_authority'"
        ),
        "t",
        "PUBLIC no longer holds EXECUTE on the executor guard: the surface's \
         explicit grant is now load bearing, and this family's lack of one is \
         assertable through has_function_privilege"
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
             GRANT SELECT ON TABLE app_system.roles TO {stable};\n"
        ),
    );
    run_admin(&admin, "re-converge after the widening", &surface);
    assert_eq!(
        inventory(&admin, stable),
        expected,
        "a hand-granted adjacent privilege survived a re-apply: this credential \
         must never read role state or perform admission or claim work"
    );

    let mut probes = format!(
        "DO $probe$ DECLARE r text := '{login}'; BEGIN \
           ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = r), \
             'the probe role is superuser or bypasses RLS — that masks every denial below'; \
           ASSERT (SELECT rolcanlogin FROM pg_roles WHERE rolname = r), \
             'the prepared generation must be able to authenticate'; \
           ASSERT has_schema_privilege(r, 'catalog', 'USAGE'), 'no catalog USAGE'; \
           ASSERT has_schema_privilege(r, 'app_system', 'USAGE'), 'no app_system USAGE'; \
           ASSERT has_table_privilege(r, 'app_system.permissions', 'SELECT'), \
             'cannot read the exact operation grants'; \
           ASSERT NOT has_table_privilege(r, 'app_system.permissions', 'INSERT') \
             AND NOT has_table_privilege(r, 'app_system.permissions', 'UPDATE') \
             AND NOT has_table_privilege(r, 'app_system.permissions', 'DELETE'), \
             'the callable-HTTP admitter writes operation grants'; \
           ASSERT NOT has_table_privilege(r, 'app_system.roles', 'SELECT'), \
             'the callable-HTTP admitter reads adjacent role state'; \
           ASSERT NOT has_schema_privilege(r, 'wamn_run', 'USAGE'), \
             'the admitter authorizes an effect; it never touches the run plane'; \
           ASSERT NOT has_function_privilege(r, 'wamn_authority.tenant_key(text)', 'EXECUTE'), \
             'a pure reader forms no index entry, so the tkey derivation is not its'; \n"
    );
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        probes.push_str(&format!(
            "  ASSERT has_table_privilege(r, 'catalog.{relation}', 'SELECT'), \
               'cannot read catalog.{relation}, which its one statement joins'; \
             ASSERT NOT has_table_privilege(r, 'catalog.{relation}', 'INSERT') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'UPDATE') \
               AND NOT has_table_privilege(r, 'catalog.{relation}', 'DELETE'), \
               'the callable-HTTP admitter writes nothing anywhere'; \n"
        ));
    }
    for relation in [
        "wamn_run.runs",
        "wamn_run.run_queue",
        "wamn_run.effect_attempts",
        "wamn_run.environment_policies",
        "catalog.packages",
        "catalog.effective_release_heads",
        "catalog.wiring_activation",
        "catalog.release_manifest_v3_snapshots",
    ] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            probes.push_str(&format!(
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the callable-HTTP admitter holds {privilege} on {relation}'; \n"
            ));
        }
    }
    probes.push_str("END $probe$;\n");
    run_admin(&admin, "the effective-privilege probes", &probes);

    // --- THE POST-STATE, IN THE GENERATION'S OWN SESSION --------------------
    //
    // Every probe above is a privilege-catalog question, and the catalog is
    // BLIND TO RLS: `has_table_privilege` answers TRUE for a role whose every
    // SELECT returns zero rows because no policy matched it. All seven relations
    // carry FORCE RLS with a restrictive tenant floor keyed on the GUEST login
    // pattern, so a non-guest family reads through the permissive
    // `TO wamn_platform` arm or it reads nothing at all — and reading nothing
    // RAISES NOTHING. This arm is therefore the only thing here that separates
    // an ISOLATED credential from a SILENTLY BLIND one (`wamn-0h0g.22.11`).
    //
    // Unlike the executor arm above, this one does NOT re-apply
    // `platform_group_membership_sql`. The edge is converged by
    // `prepare_workload_generation_sql` itself, through
    // `normalize_workload_generation_membership_sql`, so re-applying it here
    // would MASK the day that stops being true — which is precisely the
    // regression the counts below exist to catch.
    let digest = format!("sha256:{}", "a".repeat(64));
    run_admin(
        &admin,
        "seed one readable fact in each of the seven relations",
        &format!(
            // `connection_instances.active_generation` and
            // `connection_generations` reference each other, and the instance's
            // BEFORE UPDATE guard rejects any revision that is not a controlled
            // bump — so the cycle is closed with the DEFERRABLE arm inside one
            // transaction rather than with an UPDATE the schema forbids.
            "BEGIN;\nSET CONSTRAINTS ALL DEFERRED;\n\
             INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('tenant-a', 'orders', '1.0.0', '{digest}');\n\
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment) \
             VALUES ('tenant-a', 1, 'prod');\n\
             INSERT INTO catalog.effective_release_packages \
               (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ('tenant-a', 1, 'orders', '1.0.0');\n\
             INSERT INTO catalog.component_library \
               (tenant_id, package_id, package_version, component, interface_version, \
                operation, component_digest, projection_hash, imports, imports_fingerprint, effects, \
                input_ports, output_ports, parameters, admitted_at) \
             VALUES ('tenant-a', 'orders', '1.0.0', 'http-request', '0.1.0', \
                     'run', '{digest}', '{digest}', '[]', '{digest}', \
                     '[]', '[]', '[]', '[]', now());\n\
             INSERT INTO catalog.wirings \
               (tenant_id, package_id, package_version, wiring_id, version, \
                graph_json, wiring_hash, created_at) \
             VALUES ('tenant-a', 'orders', '1.0.0', 'hot-route', 1, \
                     '{{}}', '{digest}', now());\n\
             INSERT INTO catalog.connection_instances \
               (tenant_id, environment, instance_id, requirement_type, contract, \
                lifecycle_status, active_generation, revision, created_at, updated_at) \
             VALUES ('tenant-a', 'prod', 'upstream', 'http', \
                     'wamn:connection/http@0.1.0', 'enabled', 1, 1, now(), now());\n\
             INSERT INTO catalog.connection_generations \
               (tenant_id, environment, instance_id, generation, definition_json, \
                definition_hash, credential_set_handle, created_at) \
             VALUES ('tenant-a', 'prod', 'upstream', 1, '{{}}', '{digest}', \
                     'upstream-v1', now());\n\
             INSERT INTO catalog.connection_requirements \
               (tenant_id, component_digest, store_alias, requirement_json, \
                requirement_hash) \
             VALUES ('tenant-a', '{digest}', 'upstream', '{{}}', '{digest}');\n\
             INSERT INTO catalog.connection_bindings \
               (tenant_id, effective_release_id, component_digest, store_alias, \
                environment, instance_id, binding_status, validation_status, \
                validation_hash) \
             VALUES ('tenant-a', 1, '{digest}', 'upstream', 'prod', 'upstream', \
                     'active', 'valid', '{digest}');\n\
             INSERT INTO app_system.roles (tenant_id, name, is_system) \
             VALUES ('tenant-a', 'route-caller', true);\n\
             INSERT INTO app_system.permissions (tenant_id, role_name, permission) \
             VALUES ('tenant-a', 'route-caller', \
                     'wamn_receiving@1.0.0::purchase_order.get');\n\
             COMMIT;\n"
        ),
    );

    let as_login = role_url(&admin, &login, GENERATION_PW);
    assert_eq!(
        query(
            &as_login,
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user"
        ),
        "f",
        "the reading session is superuser or bypasses RLS, which satisfies every \
         count below while proving nothing"
    );
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        assert_eq!(
            query(
                &as_login,
                &format!("SELECT count(*) FROM catalog.{relation}")
            ),
            "1",
            "the callable-HTTP generation read the WRONG NUMBER of rows from \
             catalog.{relation}. Zero is the failure this arm exists to catch: the \
             row is seeded and the SELECT is granted, so zero means the tenant floor \
             admitted nothing and the credential is isolated into blindness rather \
             than into authority"
        );
    }
    assert_eq!(
        query(
            &as_login,
            "SELECT count(*) FROM app_system.permissions \
              WHERE role_name = 'route-caller' \
                AND permission = 'wamn_receiving@1.0.0::purchase_order.get'"
        ),
        "1",
        "the callable-HTTP generation cannot read the exact operation grant"
    );
}
