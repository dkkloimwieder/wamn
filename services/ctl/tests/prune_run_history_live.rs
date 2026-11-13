//! Live test of the `prune-run-history` verb of `wamn-ctl-ops`.
//!
//! It runs on the PostgreSQL server of its test process and holds the process
//! lock of that server, because it creates roles.
//!
//! The case applies the real `deploy/sql/run-state.sql` and
//! `deploy/sql/run-queue.sql` on the project base grants, mints a real
//! `wamn_run_retention` credential generation, seeds run history for two
//! projects, and runs the real verb through `wamn-ctl-ops`. It tests four things:
//!
//! 1. The verb removes only old terminal runs of the project of the credential,
//!    and the queue row of each removed run goes with it through its cascade.
//! 2. The verb refuses a project that the credential was not minted for, an
//!    unknown project, and the shared `wamn_app` login. The case reads the
//!    post-state, because a refusal must not look like a match of nothing.
//! 3. The generation reads only the three columns of the prune predicate, holds
//!    nothing on `run_queue` and nothing outside `runs`, and the terminal-only
//!    trigger refuses its raw non-terminal delete.
//! 4. The `wamn_platform` arm still lets the generation read those three columns
//!    of another project. No grant can close that, so the case asserts it.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_control_provision::{
    APP_ROLE, WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login,
};
use wamn_test_infrastructure::ctl_process;
use wamn_test_infrastructure::locked_database;

const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const SCHEMA: &str = "wamn_run";
const PROJECT_ENV_KEY: &str = "run-retention-t--dev--fixture";
/// A second project with history of its own. Without it the cross-project
/// refusal could not be told from a match of nothing.
const OTHER_PROJECT_ENV_KEY: &str = "run-retention-other--dev--fixture";
const PACKAGE_ID: &str = "run_retention_fixture";
/// The release every seeded run pins: the digest of the bytes `{}`.
const RELEASE_DIGEST: &str =
    "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const RETENTION_DAYS: &str = "30";
const GENERATION_PASSWORD: &str = "run-retention-test-generation";
const APP_PASSWORD: &str = "run-retention-test-app";

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL");

    client
}

/// Install the run plane on the project base grants, with one effective release for each project.
async fn provision(admin: &mut Client) {
    sqlx::raw_sql(RUN_STATE_SQL)
        .execute(&mut *admin)
        .await
        .map(drop)
        .expect("apply run-state.sql");
    sqlx::raw_sql(RUN_QUEUE_SQL)
        .execute(&mut *admin)
        .await
        .map(drop)
        .expect("apply run-queue.sql");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-catalog-fixture', false); \
             INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ('{PROJECT_ENV_KEY}', '{RELEASE_DIGEST}', '{{}}'), \
                    ('{OTHER_PROJECT_ENV_KEY}', '{RELEASE_DIGEST}', '{{}}');",
            wamn_control_provision::PlatformComponent::Provisioning.principal_id()
        ))).execute(&mut *admin).await.map(drop)
        .expect("seed the releases");
}

async fn drop_generation_role(admin: &mut Client, role: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DO $run_retention$ BEGIN \
               IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $run_retention$;"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("drop the generation role");
}

/// Mint the generation through the production builders: the prepare builder
/// for the login and the platform group builder for the `wamn_platform` edge.
/// Without that edge, FORCE RLS on `runs` gives the generation zero rows.
async fn mint(admin: &mut Client, database: &str, role: &str) {
    drop_generation_role(admin, role).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::Retention,
            database,
            role,
            GENERATION_PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("prepare the run retention generation");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::platform_group_membership_sql(WorkloadRoleFamily::Retention)).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)
    .expect("converge the run retention platform group edge");
    let clean: bool = sqlx::query(
        "SELECT NOT (rolsuper OR rolbypassrls) AND rolcanlogin AND rolinherit \
               FROM pg_catalog.pg_roles WHERE rolname = $1",
    )
    .bind(&role)
    .fetch_one(&mut *admin)
    .await
    .expect("read the generation attributes")
    .get(0);
    assert!(
        clean,
        "the {role} generation is superuser, BYPASSRLS, cannot log in, or does not inherit"
    );
}

/// Insert a run created `age_days` days ago, and its queue row.
async fn seed_run(
    admin: &mut Client,
    project_env_key: &str,
    run_id: &str,
    status: &str,
    age_days: i64,
) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {SCHEMA}.runs ( \
                   project_env_key, run_id, wiring_id,wiring_version,wiring_hash,binding_world_json, package_id, manifest_digest, \
                   environment, status, input_json, created_at \
                 ) VALUES ($1, $2, 'fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb, '{PACKAGE_ID}', '{RELEASE_DIGEST}', 'test', $3, \
                           jsonb_build_object('payload', $1::text), \
                           now() - ($4::bigint * interval '1 day'))"
            ))).bind(&project_env_key).bind(&run_id).bind(&status).bind(&age_days).execute(&mut *admin).await.map(|result| result.rows_affected())
        .expect("seed a run");
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!("INSERT INTO {SCHEMA}.run_queue (project_env_key, run_id) VALUES ($1, $2)"))
            .to_string(),
    ))
    .bind(&project_env_key)
    .bind(&run_id)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())
    .expect("seed a queue row");
}

/// The `project/run` keys of `relation`, in order.
async fn keys(admin: &mut Client, relation: &str) -> Vec<String> {
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT project_env_key || '/' || run_id FROM {SCHEMA}.{relation} \
                  ORDER BY project_env_key, run_id"
        ))
            .to_string(),
    ))
    .fetch_all(&mut *admin)
    .await
    .expect("read the post-state")
    .iter()
    .map(|row| row.get(0))
    .collect()
}

fn prune_argv<'a>(url: &'a str, project_env_key: &'a str) -> [&'a str; 13] {
    [
        "prune-run-history",
        "--database-url",
        url,
        "--schema",
        SCHEMA,
        "--org",
        project_env_key.split("--").next().expect("fixture org"),
        "--project",
        "fixture",
        "--environment",
        "dev",
        "--retention-days",
        RETENTION_DAYS,
    ]
}

/// Run the verb and require the identity refusal, not a success that reports zero.
async fn assert_refused(url: &str, project_env_key: &str, label: &str) {
    ctl_process::require_ops_success(prune_argv(url, project_env_key))
        .await
        .expect_err(label);
}

/// Re-point a connection URL at another role, keeping host, port, and database.
fn replace_role(url: &str, role: &str, password: &str) -> String {
    let mut input = url::Url::parse(url).expect("parse fixture input");
    input.set_username(role).unwrap();
    input.set_password(Some(password)).unwrap();
    input.into()
}

/// Whether the generation may run `statement`.
async fn allowed(credential: &mut Client, statement: &str) -> bool {
    sqlx::query(sqlx::AssertSqlSafe((statement).to_string()))
        .fetch_all(&mut *credential)
        .await
        .is_ok()
}

/// What the `wamn_platform` arm gives the generation, read from the server as
/// the generation. The grants confine it, and the arm is `USING (true)`.
async fn assert_authority(credential: &mut Client) {
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
    let error = sqlx::query(sqlx::AssertSqlSafe(
        (&format!("DELETE FROM {SCHEMA}.runs WHERE project_env_key = $1 AND status = 'running'"))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .execute(&mut *credential)
    .await
    .map(|result| result.rows_affected())
    .expect_err("the trigger refuses a raw non-terminal delete");
    assert_eq!(
        error
            .as_database_error()
            .map(sqlx::error::DatabaseError::message),
        Some("run-delete-nonterminal"),
        "the refusal of the raw non-terminal delete: {error:?}"
    );

    let stray: i64 = sqlx::query(
        "SELECT count(*) FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE', \
                                       'REFERENCES','TRIGGER']) p \
              WHERE c.relkind IN ('r','p') \
                AND n.nspname NOT IN ('pg_catalog','information_schema') \
                AND c.relname <> 'runs' \
                AND pg_catalog.has_table_privilege('wamn_run_retention', c.oid, p)",
    )
    .fetch_one(&mut *credential)
    .await
    .expect("sweep the stable run retention role for table privileges")
    .get(0);
    assert_eq!(
        stray, 0,
        "the run retention role holds privileges outside runs"
    );

    // The residual: the platform arm still shows the other project's three columns.
    let foreign: i64 = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {SCHEMA}.runs WHERE project_env_key = $1"
    )))
    .bind(&OTHER_PROJECT_ENV_KEY)
    .fetch_one(&mut *credential)
    .await
    .expect("read the other project's rows under the platform arm")
    .get(0);
    assert!(
        foreign > 0,
        "the platform arm no longer exposes the other project's predicate columns"
    );
}

#[tokio::test]
async fn prune_removes_only_old_terminal_runs_of_the_credential_project_env_key() {
    let url = locked_database::database(wamn_catalog::test_database::project);
    let mut admin = connect(&url).await;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await
        .expect("read the target database name")
        .get(0);
    provision(&mut admin).await;

    // An old completed run, a recent completed run, an old running run, and an
    // old completed run of the other project.
    seed_run(&mut admin, PROJECT_ENV_KEY, "old-done", "completed", 40).await;
    seed_run(&mut admin, PROJECT_ENV_KEY, "recent-done", "completed", 1).await;
    seed_run(&mut admin, PROJECT_ENV_KEY, "old-running", "running", 40).await;
    seed_run(
        &mut admin,
        OTHER_PROJECT_ENV_KEY,
        "foreign-done",
        "completed",
        40,
    )
    .await;
    let seeded_runs = keys(&mut admin, "runs").await;
    let seeded_queue = keys(&mut admin, "run_queue").await;

    let generation_role = workload_login(
        WorkloadRoleFamily::Retention,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database: &database,
        },
    )
    .expect("derive the run retention generation identity");
    mint(&mut admin, &database, &generation_role).await;
    let credential_url = replace_role(&url, &generation_role, GENERATION_PASSWORD);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER ROLE {APP_ROLE} LOGIN PASSWORD '{APP_PASSWORD}'"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("let the shared wamn_app role log in");
    let app_url = replace_role(&url, APP_ROLE, APP_PASSWORD);

    // The refusals run first, against the populated store.
    assert_refused(
        &credential_url,
        OTHER_PROJECT_ENV_KEY,
        "foreign environment coordinate",
    )
    .await;
    assert_refused(
        &credential_url,
        "no-such-project",
        "unknown environment coordinate",
    )
    .await;
    assert_refused(&app_url, PROJECT_ENV_KEY, "wamn_app login").await;
    assert_eq!(
        (
            keys(&mut admin, "runs").await,
            keys(&mut admin, "run_queue").await
        ),
        (seeded_runs, seeded_queue),
        "the refusals left the run history untouched"
    );

    let mut credential = connect(&credential_url).await;
    assert_authority(&mut credential).await;
    drop(credential);

    let pruned = ctl_process::require_ops_success(prune_argv(&credential_url, PROJECT_ENV_KEY))
        .await
        .expect("prune run history through wamn-ctl-ops");
    let stdout = String::from_utf8(pruned.stdout).expect("prune output is UTF-8");
    print!("{stdout}");
    assert!(
        stdout.contains("pruned 1 terminal run(s)"),
        "the verb did not report one pruned run: {stdout}"
    );
    let kept = vec![
        format!("{OTHER_PROJECT_ENV_KEY}/foreign-done"),
        format!("{PROJECT_ENV_KEY}/old-running"),
        format!("{PROJECT_ENV_KEY}/recent-done"),
    ];
    assert_eq!(keys(&mut admin, "runs").await, kept, "the retained runs");
    assert_eq!(
        keys(&mut admin, "run_queue").await,
        kept,
        "the queue row of the pruned run cascaded, and the others stayed"
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER ROLE {APP_ROLE} NOLOGIN PASSWORD NULL"
    )))
    .execute(&mut admin)
    .await
    .map(drop)
    .expect("restore the shared wamn_app role");
    drop_generation_role(&mut admin, &generation_role).await;
}
