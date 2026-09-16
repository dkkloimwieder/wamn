//! Live test of the `prune-run-history` verb of `wamn-ctl-ops`.
//!
//! It runs on the PostgreSQL server of its test process and holds the process
//! lock of that server, because it creates roles.
//!
//! The case applies the real `deploy/sql/run-state.sql` and
//! `deploy/sql/run-queue.sql` on the tenant floor, mints a real
//! `wamn_run_retention` credential generation, seeds run history for two
//! tenants, and runs the real verb through `wamn-ctl-ops`. It tests four things:
//!
//! 1. The verb removes only old terminal runs of the tenant of the credential,
//!    and the queue row of each removed run goes with it through its cascade.
//! 2. The verb refuses a tenant that the credential was not minted for, an
//!    unknown tenant, and the shared `wamn_app` login. The case reads the
//!    post-state, because a refusal must not look like a match of nothing.
//! 3. The generation reads only the three columns of the prune predicate, holds
//!    nothing on `run_queue` and nothing outside `runs`, and the terminal-only
//!    trigger refuses its raw non-terminal delete.
//! 4. The `wamn_platform` arm still lets the generation read those three columns
//!    of another tenant. No grant can close that, so the case asserts it.

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, compose_url, sql,
    workload_generation_role,
};
use wamn_test_infrastructure::ctl_process;
use wamn_test_infrastructure::locked_database;

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const SCHEMA: &str = "wamn_run";
const TENANT: &str = "run-retention-t";
/// A second tenant with history of its own. Without it the cross-tenant
/// refusal could not be told from a match of nothing.
const OTHER_TENANT: &str = "run-retention-other";
const PACKAGE_ID: &str = "run_retention_fixture";
const EFFECTIVE_RELEASE_ID: i32 = 1;
const RETENTION_DAYS: &str = "30";
const GENERATION_PASSWORD: &str = "run-retention-test-generation";
const APP_PASSWORD: &str = "run-retention-test-app";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Install the run plane on the tenant floor, with one effective release for each tenant.
async fn provision(admin: &Client) {
    admin
        .batch_execute(RUN_STATE_SQL)
        .await
        .expect("apply run-state.sql");
    admin
        .batch_execute(RUN_QUEUE_SQL)
        .await
        .expect("apply run-queue.sql");
    admin
        .batch_execute(&format!(
            "INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
             VALUES ('{TENANT}', {EFFECTIVE_RELEASE_ID}, 'test'), \
                    ('{OTHER_TENANT}', {EFFECTIVE_RELEASE_ID}, 'test');"
        ))
        .await
        .expect("seed the effective releases");
}

async fn drop_generation_role(admin: &Client, role: &str) {
    admin
        .batch_execute(&format!(
            "DO $run_retention$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $run_retention$;"
        ))
        .await
        .expect("drop the generation role");
}

/// Mint the generation through the production builders: the prepare builder
/// for the login and the platform group builder for the `wamn_platform` edge.
/// Without that edge, FORCE RLS on `runs` gives the generation zero rows.
async fn mint(admin: &Client, database: &str, role: &str) {
    drop_generation_role(admin, role).await;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Retention,
            database,
            role,
            GENERATION_PASSWORD,
            "2100-01-01T00:00:00Z",
        ))
        .await
        .expect("prepare the run retention generation");
    admin
        .batch_execute(&sql::platform_group_membership_sql(
            WorkloadRoleFamily::Retention,
        ))
        .await
        .expect("converge the run retention platform group edge");
    let clean: bool = admin
        .query_one(
            "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
            &[&role],
        )
        .await
        .expect("read the generation attributes")
        .get(0);
    assert!(
        clean,
        "the {role} generation is superuser, BYPASSRLS, cannot log in, or does not inherit"
    );
}

/// Insert a run created `age_days` days ago, and its queue row.
async fn seed_run(admin: &Client, tenant: &str, run_id: &str, status: &str, age_days: i64) {
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs ( \
                   tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, \
                   environment, status, input_json, created_at \
                 ) VALUES ($1, $2, 'f', 1, '{PACKAGE_ID}', {EFFECTIVE_RELEASE_ID}, 'test', $3, \
                           jsonb_build_object('payload', $1::text), \
                           now() - ($4::bigint * interval '1 day'))"
            ),
            &[&tenant, &run_id, &status, &age_days],
        )
        .await
        .expect("seed a run");
    admin
        .execute(
            &format!("INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id) VALUES ($1, $2)"),
            &[&tenant, &run_id],
        )
        .await
        .expect("seed a queue row");
}

/// The `tenant/run` keys of `relation`, in order.
async fn keys(admin: &Client, relation: &str) -> Vec<String> {
    admin
        .query(
            &format!(
                "SELECT tenant_id || '/' || run_id FROM {SCHEMA}.{relation} \
                  ORDER BY tenant_id, run_id"
            ),
            &[],
        )
        .await
        .expect("read the post-state")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

fn prune_argv<'a>(url: &'a str, tenant: &'a str) -> [&'a str; 9] {
    [
        "prune-run-history",
        "--database-url",
        url,
        "--schema",
        SCHEMA,
        "--tenant",
        tenant,
        "--retention-days",
        RETENTION_DAYS,
    ]
}

/// Run the verb and require the identity refusal, not a success that reports zero.
async fn assert_refused(url: &str, tenant: &str, label: &str) {
    let error = ctl_process::run_ops_checked(prune_argv(url, tenant))
        .await
        .expect_err(label);
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("refusing to prune"),
        "{label}: the failure was not the refusal: {rendered}"
    );
    assert!(
        !rendered.contains("pruned 0 terminal run(s)"),
        "{label}: the verb reported zero pruned runs: {rendered}"
    );
}

/// Re-point a connection URL at another role, keeping host, port, and database.
fn replace_role(url: &str, role: &str, password: &str) -> String {
    let config: tokio_postgres::Config = url.parse().expect("parse the connection URL");
    let host = match config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)] => host.clone(),
        hosts => panic!("the run history retention test needs one TCP host, got {hosts:?}"),
    };
    let port = *config
        .get_ports()
        .first()
        .expect("the connection URL names a port");
    let database = config
        .get_dbname()
        .expect("the connection URL names a database");
    compose_url(role, password, &host, port, database)
}

/// Whether the generation may run `statement`.
async fn allowed(credential: &Client, statement: &str) -> bool {
    credential.query(statement, &[]).await.is_ok()
}

/// What the `wamn_platform` arm gives the generation, read from the server as
/// the generation. The grants confine it, and the arm is `USING (true)`.
async fn assert_authority(credential: &Client) {
    assert!(
        allowed(
            credential,
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status = 'completed'")
        )
        .await,
        "the generation reads the three predicate columns"
    );
    for statement in [
        format!("SELECT input_json FROM {SCHEMA}.runs LIMIT 1"),
        format!("SELECT * FROM {SCHEMA}.runs LIMIT 1"),
        format!("SELECT count(*) FROM {SCHEMA}.run_queue"),
    ] {
        assert!(
            !allowed(credential, &statement).await,
            "the generation was not refused: {statement}"
        );
    }

    // The grant bounds who may delete, and the trigger bounds what.
    let error = credential
        .execute(
            &format!("DELETE FROM {SCHEMA}.runs WHERE tenant_id = $1 AND status = 'running'"),
            &[&TENANT],
        )
        .await
        .expect_err("the trigger refuses a raw non-terminal delete");
    assert_eq!(
        error.as_db_error().map(tokio_postgres::error::DbError::message),
        Some("run-delete-nonterminal"),
        "the refusal of the raw non-terminal delete: {error:?}"
    );

    let stray: i64 = credential
        .query_one(
            "SELECT count(*) FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE', \
                                       'REFERENCES','TRIGGER']) p \
              WHERE c.relkind IN ('r','p') \
                AND n.nspname NOT IN ('pg_catalog','information_schema') \
                AND c.relname <> 'runs' \
                AND pg_catalog.has_table_privilege('wamn_run_retention', c.oid, p)",
            &[],
        )
        .await
        .expect("sweep the stable run retention role for table privileges")
        .get(0);
    assert_eq!(
        stray, 0,
        "the run retention role holds privileges outside runs"
    );

    // The residual: the platform arm still shows the other tenant's three columns.
    let foreign: i64 = credential
        .query_one(
            &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE tenant_id = $1"),
            &[&OTHER_TENANT],
        )
        .await
        .expect("read the other tenant's rows under the platform arm")
        .get(0);
    assert!(
        foreign > 0,
        "the platform arm no longer exposes the other tenant's predicate columns"
    );
}

#[tokio::test]
async fn prune_removes_only_old_terminal_runs_of_the_credential_tenant() {
    let url = locked_database::database(wamn_catalog::test_database::tenant);
    let admin = connect(&url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read the target database name")
        .get(0);
    provision(&admin).await;

    // An old completed run, a recent completed run, an old running run, and an
    // old completed run of the other tenant.
    seed_run(&admin, TENANT, "old-done", "completed", 40).await;
    seed_run(&admin, TENANT, "recent-done", "completed", 1).await;
    seed_run(&admin, TENANT, "old-running", "running", 40).await;
    seed_run(&admin, OTHER_TENANT, "foreign-done", "completed", 40).await;
    let seeded_runs = keys(&admin, "runs").await;
    let seeded_queue = keys(&admin, "run_queue").await;

    let generation_role = workload_generation_role(
        WorkloadRoleFamily::Retention,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("derive the run retention generation identity");
    mint(&admin, &database, &generation_role).await;
    let credential_url = replace_role(&url, &generation_role, GENERATION_PASSWORD);
    admin
        .batch_execute(&format!(
            "ALTER ROLE {APP_ROLE} LOGIN PASSWORD '{APP_PASSWORD}'"
        ))
        .await
        .expect("let the shared wamn_app role log in");
    let app_url = replace_role(&url, APP_ROLE, APP_PASSWORD);

    // The refusals run first, against the populated store.
    assert_refused(&credential_url, OTHER_TENANT, "foreign --tenant").await;
    assert_refused(&credential_url, "no-such-tenant", "unknown --tenant").await;
    assert_refused(&app_url, TENANT, "wamn_app login").await;
    assert_eq!(
        (keys(&admin, "runs").await, keys(&admin, "run_queue").await),
        (seeded_runs, seeded_queue),
        "the refusals left the run history untouched"
    );

    let credential = connect(&credential_url).await;
    assert_authority(&credential).await;
    drop(credential);

    let pruned = ctl_process::run_ops_checked(prune_argv(&credential_url, TENANT))
        .await
        .expect("prune run history through wamn-ctl-ops");
    let stdout = String::from_utf8(pruned.stdout).expect("prune output is UTF-8");
    print!("{stdout}");
    assert!(
        stdout.contains("pruned 1 terminal run(s)"),
        "the verb did not report one pruned run: {stdout}"
    );
    let kept = vec![
        format!("{OTHER_TENANT}/foreign-done"),
        format!("{TENANT}/old-running"),
        format!("{TENANT}/recent-done"),
    ];
    assert_eq!(keys(&admin, "runs").await, kept, "the retained runs");
    assert_eq!(
        keys(&admin, "run_queue").await,
        kept,
        "the queue row of the pruned run cascaded, and the others stayed"
    );

    admin
        .batch_execute(&format!("ALTER ROLE {APP_ROLE} NOLOGIN PASSWORD NULL"))
        .await
        .expect("restore the shared wamn_app role");
    drop_generation_role(&admin, &generation_role).await;
}
