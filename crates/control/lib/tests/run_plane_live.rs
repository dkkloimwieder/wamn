//! Live tests for current run-plane creation, security, and convergence.
//! Tests share PostgreSQL roles and hold the database process lock.

#[path = "run_plane_live/catalog.rs"]
mod catalog;
#[path = "run_plane_live/effect_tables.rs"]
mod effect_tables;
#[path = "run_plane_live/partition.rs"]
mod partition;
#[path = "run_plane_live/platform_configuration.rs"]
mod platform_configuration;
#[path = "run_plane_live/run_history.rs"]
mod run_history;
#[path = "run_plane_live/target_identity.rs"]
mod target_identity;

use wamn_platform_identity::auth_changes::AuthChanges;

use catalog::{capture_mode_security_leg, two_plane_residency_leg};
use run_history::persisted_literal_leg;

use tokio_postgres::{Client, NoTls};

use wamn_control::reconcile_run_plane::{
    self, RECONCILE_TARGET_REFUSAL_PREFIX, ReconcileRunPlaneRequest, ReconcileTargetError,
    ReconcileTargetErrorType,
};
use wamn_control::verification_policy::project_environment_policy;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, project_env_database_name,
    sql as provision_sql, workload_generation_role,
};
use wamn_schema_control::{BareSchemaName, RunPlaneActionKind, rewrite_schema};
use wamn_test_infrastructure::locked_database;

const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
/// The CONTROL plane's canonical record. Read here only to lift the co-resident
/// relation's declaration text, never installed as a whole.
const CONTROL_PORTABLE_STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../../test-support/fixtures/sql/current-database-public-connect.sql");

const SCHEMA: &str = "rp_live";
const GUEST_GENERATION_PASSWORD: &str = "guest-generation-run-plane-probe";
const EMPTY_EXECUTION_BUNDLE_HASH: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const CLI_ORG: &str = "acme";
const CLI_PROJECT: &str = "billing";
const CLI_ENV: &str = "dev";
const CLI_INSTANCE: &str = "k3m9x2p7";

async fn seed_system_env_policy(su: &Client, durability_class: &str) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS registry CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
           THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; \
         CREATE SCHEMA registry AUTHORIZATION wamn_system; \
         SET ROLE wamn_system; \
         CREATE TABLE registry.env_policies ( \
           org text NOT NULL, name text NOT NULL, recovery_domain jsonb NOT NULL, \
           promotion_rank int NOT NULL, instances int NOT NULL, storage text NOT NULL, \
           cpu text NOT NULL, memory text NOT NULL, image text NOT NULL, \
           backup_cadence text NOT NULL, wal_retention text NOT NULL, \
           hibernation text NOT NULL, durability_class text NOT NULL, \
           PRIMARY KEY (org, name)); \
         CREATE TABLE registry.project_envs ( \
           org text NOT NULL, project text NOT NULL, env text NOT NULL, \
           secret_name text NOT NULL, secret_namespace text, \
           instance_suffix text NOT NULL, policy_name text NOT NULL, \
           PRIMARY KEY (org, project, env)); \
         RESET ROLE",
    )
    .await
    .expect("create system env-policy fixture");
    su.execute(
        "INSERT INTO registry.env_policies \
           (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image, \
            backup_cadence,wal_retention,hibernation,durability_class) \
         VALUES ('acme','dev','\"own\"',0,1,'1Gi','100m','128Mi','postgres','','','off',$1)",
        &[&durability_class],
    )
    .await
    .expect("seed system env policy");
    su.execute(
        "INSERT INTO registry.project_envs \
           (org,project,env,secret_name,instance_suffix,policy_name) \
         VALUES ($1,$2,$3,'wamn-db-acme--billing--dev',$4,$3)",
        &[&CLI_ORG, &CLI_PROJECT, &CLI_ENV, &CLI_INSTANCE],
    )
    .await
    .expect("seed recorded project-env target");
    su.batch_execute(SYSTEM_IDENTITY_FIXTURE_SQL)
        .await
        .expect("create the system identity fixture");
}

/// The deployment platform domain and the identity relations the reconciler
/// reads to build a tenant's `platform."user"` rows (`wamn-0h0g.9.15`).
///
/// The shapes are the columns this reader uses, in the simplified style the
/// rest of this fixture keeps. `deploy/sql/system-schema.sql` owns the real
/// relations, with their record-history columns and their CHECKs.
const SYSTEM_IDENTITY_FIXTURE_SQL: &str = "DROP SCHEMA IF EXISTS identity CASCADE; \
     CREATE SCHEMA identity AUTHORIZATION wamn_system; \
     SET ROLE wamn_system; \
     CREATE TABLE registry.meta ( \
       id boolean PRIMARY KEY DEFAULT true CHECK (id), \
       schema_version text NOT NULL, platform_domain text); \
     INSERT INTO registry.meta (schema_version, platform_domain) \
       VALUES ('0.1', 'example.invalid'); \
     CREATE TABLE identity.principals ( \
       id uuid PRIMARY KEY, type text NOT NULL, subject text NOT NULL, \
       email text, display_name text NOT NULL, \
       status text NOT NULL DEFAULT 'active'); \
     CREATE TABLE identity.project_roles ( \
       principal_id uuid NOT NULL REFERENCES identity.principals (id), \
       org text NOT NULL, project text NOT NULL, role text NOT NULL, \
       PRIMARY KEY (principal_id, org, project, role)); \
     CREATE TABLE identity.project_env_memberships ( \
       principal_id uuid NOT NULL REFERENCES identity.principals (id), \
       org text NOT NULL, project text NOT NULL, env text NOT NULL, \
       PRIMARY KEY (principal_id, org, project, env)); \
     INSERT INTO identity.principals (id, type, subject, email, display_name) VALUES \
       ('11111111-1111-4111-8111-111111111111', 'service', \
        'wamn-management-author-acme--billing--dev', NULL, \
        'WAMN management author acme/billing/dev'), \
       ('22222222-2222-4222-8222-222222222222', 'user', \
        'person@example.invalid', 'person@example.invalid', 'A user'), \
       ('33333333-3333-4333-8333-333333333333', 'user', \
        'other-env@example.invalid', 'other-env@example.invalid', \
        'A user in another environment'); \
     INSERT INTO identity.project_roles (principal_id, org, project, role) VALUES \
       ('11111111-1111-4111-8111-111111111111', 'acme', 'billing', 'project-author'), \
       ('22222222-2222-4222-8222-222222222222', 'acme', 'billing', 'project-author'), \
       ('33333333-3333-4333-8333-333333333333', 'acme', 'billing', 'project-author'); \
     INSERT INTO identity.project_env_memberships (principal_id, org, project, env) VALUES \
       ('22222222-2222-4222-8222-222222222222', 'acme', 'billing', 'dev'), \
       ('33333333-3333-4333-8333-333333333333', 'acme', 'billing', 'prod'); \
     RESET ROLE";

/// The one service principal the fixture gives project `acme/billing`.
const FIXTURE_SERVICE_ID: &str = "11111111-1111-4111-8111-111111111111";

/// The one user the fixture admits to `acme/billing/dev` (`wamn-0h0g.9.18`).
const FIXTURE_USER_ID: &str = "22222222-2222-4222-8222-222222222222";

/// The tenant identity rows one reconcile installs (`wamn-0h0g.9.15`).
///
/// `reconcile-run-plane` is the production application of
/// `deploy/sql/app-schema.sql` and of the platform principal rows, which
/// `docs/operations/deployment.md` told an operator to pipe into `psql` by
/// hand. Every row is written as `wamn:provisioning`, and the
/// `wamn:provisioning` row stamps itself.
///
/// The fixture's first user holds a membership in `acme/billing/dev` and gets
/// a user row carrying its own `identity.principals.email`
/// (`wamn-0h0g.9.18`). The second user holds the same project role but its
/// membership names `prod`, so this tenant gives it NO row: the membership
/// admits a user, and it admits one environment.
///
/// Called after each apply in this leg, so a second run writing a second time
/// fails here.
async fn env_key_identity_leg(su: &Client) {
    let rows: Vec<(String, String, String, String)> = su
        .query(
            "SELECT id::text, type, email, created_by::text FROM platform.\"user\" \
              ORDER BY email",
            &[],
        )
        .await
        .expect("read the tenant identity rows")
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect();
    let provisioning = "770df186-ac15-579e-b46b-c297cae2011b";
    assert_eq!(
        rows,
        vec![
            (
                "7695180f-4b9a-581f-84ef-d7e9cdbd2b77".to_string(),
                "platform".to_string(),
                "apply-package@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                "34cbd151-990e-5800-93be-4fba8878b943".to_string(),
                "platform".to_string(),
                "audit-retention@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                "d318d033-29ea-5cb0-ab56-24340413fbcc".to_string(),
                "platform".to_string(),
                "executor@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                "968bd0cc-e612-5d29-9d6c-af1993b8df0a".to_string(),
                "platform".to_string(),
                "materializer@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                FIXTURE_USER_ID.to_string(),
                "user".to_string(),
                "person@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                provisioning.to_string(),
                "platform".to_string(),
                "provisioning@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                FIXTURE_SERVICE_ID.to_string(),
                "service".to_string(),
                "wamn-management-author-acme--billing--dev@example.invalid".to_string(),
                provisioning.to_string(),
            ),
            (
                wamn_project_state::workflow_user_id().to_string(),
                "service".to_string(),
                "workflow@example.invalid".to_string(),
                provisioning.to_string(),
            ),
        ],
        "the tenant carries its five platform rows, the workflow user, one service row and one \
         user row, each stamped by wamn:provisioning, and no row for the member of another \
         environment",
    );
    let user_display_name: String = su
        .query_one(
            "SELECT display_name FROM platform.\"user\" WHERE id = $1::text::uuid",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("read the user row display name")
        .get(0);
    assert_eq!(
        user_display_name, "A user",
        "a user row carries the display name of its identity.principals row",
    );
}

/// A user row whose membership is gone goes at the next reconcile, with its
/// role assignments, and the service and platform rows stay
/// (docs/architecture/overview.md#organization-membership-and-administration).
/// The membership then comes back, and the row with it.
async fn stale_user_leg(su: &Client, system_su: &Client, system_url: &str, target_url: &str) {
    let reconcile = async || {
        reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
            changes: AuthChanges::default(),
            system_database_url: system_url.to_string(),
            admin_database_url: target_url.to_string(),
            org: "acme".to_string(),
            project: "billing".to_string(),
            env: "dev".to_string(),
            schema: SCHEMA.to_string(),
            dry_run: false,
        })
        .await
        .expect("reconcile-run-plane applies")
    };
    su.execute(
        "SELECT set_config('app.user_id', $1, false), \
                set_config('app.operation', 'admin:seed-stale-user-fixture', false)",
        &[&FIXTURE_USER_ID],
    )
    .await
    .expect("bind the fixture actor");
    su.batch_execute(&format!(
        "INSERT INTO platform.role (name) VALUES ('stale-reader'); \
         INSERT INTO platform.user_role (user_id, role_name) \
           VALUES ('{FIXTURE_USER_ID}', 'stale-reader')"
    ))
    .await
    .expect("give the fixture user a role");
    system_su
        .execute(
            "DELETE FROM identity.project_env_memberships WHERE principal_id = $1::text::uuid",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("revoke the fixture user's membership");
    let outcome = reconcile().await;
    assert_eq!(outcome.env_key_identity.user_rows_removed, 1);
    let rows: Vec<(String, i64)> = su
        .query(
            "SELECT type, count(*) FROM platform.\"user\" \
             GROUP BY type ORDER BY type",
            &[],
        )
        .await
        .expect("count the tenant identity rows")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        rows,
        [("platform".to_owned(), 5), ("service".to_owned(), 2)],
        "the user row goes, and the platform and service rows stay"
    );
    let roles: i64 = su
        .query_one("SELECT count(*) FROM platform.user_role ", &[])
        .await
        .expect("count the role assignments")
        .get(0);
    assert_eq!(roles, 0, "the role assignment goes with the user row");

    system_su
        .execute(
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ($1::text::uuid, 'acme', 'billing', 'dev')",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("grant the membership again");
    assert_eq!(reconcile().await.env_key_identity.user_rows_written, 1);
    su.batch_execute("DELETE FROM platform.role WHERE name = 'stale-reader'")
        .await
        .expect("remove the fixture role");

    // A project admin of the project gets its admin row from the reconcile,
    // which repairs an interrupted grant
    // (docs/architecture/overview.md#organization-membership-and-administration),
    // and a second reconcile writes none.
    system_su
        .execute(
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES ($1::text::uuid, 'acme', 'billing', 'project-admin')",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("make the fixture user a project admin");
    assert_eq!(reconcile().await.env_key_identity.admin_rows_written, 1);
    let admin: i64 = su
        .query_one(
            "SELECT count(*) FROM platform.user_role \
             WHERE user_id = $1::text::uuid AND role_name = 'admin'",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("count the admin rows")
        .get(0);
    assert_eq!(admin, 1);
    assert_eq!(reconcile().await.env_key_identity.admin_rows_written, 0);
    system_su
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid",
            &[&FIXTURE_USER_ID],
        )
        .await
        .expect("remove the fixture project admin");
    su.execute(
        "DELETE FROM platform.user_role WHERE user_id = $1::text::uuid",
        &[&FIXTURE_USER_ID],
    )
    .await
    .expect("remove the fixture admin row");
}

fn schema() -> BareSchemaName {
    BareSchemaName::new(SCHEMA).expect("live-test schema is valid")
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:run-plane-fixture', false)", &[&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()]).await.unwrap();
    client
}

fn database_url(base_url: &str, database: &str) -> String {
    let mut url = url::Url::parse(base_url).expect("parse Postgres fixture URL");
    url.set_path(&format!("/{database}"));
    url.to_string()
}

async fn recreate_database(su: &Client, database: &str) {
    su.batch_execute(&provision_sql::drop_database_named_sql(database))
        .await
        .expect("drop stale target database");
    su.batch_execute(&provision_sql::create_database_named_sql(database))
        .await
        .expect("create target database");
}

async fn drop_database(su: &Client, database: &str) {
    su.batch_execute(&provision_sql::drop_database_named_sql(database))
        .await
        .expect("drop target database");
}

async fn connect_as(url: &str, role: &str, password: &str) -> Client {
    let mut config: tokio_postgres::Config = url.parse().expect("parse Postgres URL");
    config.user(role).password(password);
    let (client, conn) = config.connect(NoTls).await.expect("connect as role");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

async fn seed_run_admission_facts(
    su: &Client,
    env_key: &str,
    package_id: &str,
    environment: &str,
    durability_class: &str,
) {
    su.execute(
        &format!(
            "INSERT INTO {SCHEMA}.environment_policies \
               (env_key,expected_environment,durability_class) \
             VALUES ($1,$2,$3)"
        ),
        &[&env_key, &environment, &durability_class],
    )
    .await
    .expect("seed the project-local environment policy");
    su.execute(
        "INSERT INTO catalog.packages \
           (env_key,package_id,package_version,artifact_digest) \
         VALUES ($1,$2,'1.0.0',$3)",
        &[&env_key, &package_id, &EMPTY_EXECUTION_BUNDLE_HASH],
    )
    .await
    .expect("seed run-pin package");
}

/// Hermetic reset: drop the target schema + the shared `catalog` schema and
/// ensure the `wamn_app` role, so every leg builds its own starting state.
/// The runtime roles match the current run-plane grants.
async fn reset(su: &Client) {
    su.batch_execute(&provision_sql::ensure_app_acl_role_sql())
        .await
        .expect("harden stable app ACL role");
    su.batch_execute(&format!(
        "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
         DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
         DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         REVOKE wamn_scenario_author FROM wamn_app; \
         DO $$ BEGIN \
           EXECUTE format( \
             'REVOKE CONNECT ON DATABASE %I FROM wamn_app', current_database() \
           ); \
         END $$;"
    ))
    .await
    .expect("hermetic reset");
}

async fn table_exists(su: &Client, schema: &str, table: &str) -> bool {
    su.query_one(
        "SELECT EXISTS ( SELECT FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = $2 )",
        &[&schema, &table],
    )
    .await
    .expect("probe table")
    .get(0)
}

/// Probe a column in the run-plane schema.
async fn column_exists(su: &Client, table: &str, column: &str) -> bool {
    su.query_one(
        "SELECT EXISTS ( SELECT FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 AND column_name = $3 )",
        &[&SCHEMA, &table, &column],
    )
    .await
    .expect("probe column")
    .get(0)
}

async fn indexdef(su: &Client, name: &str) -> Option<String> {
    su.query_opt(
        "SELECT indexdef FROM pg_indexes WHERE schemaname = $1 AND indexname = $2",
        &[&SCHEMA, &name],
    )
    .await
    .expect("read indexdef")
    .map(|r| r.get(0))
}

async fn install_current_run_plane(su: &Client) {
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply current catalog schema");
    for ddl in [RUN_STATE_SQL, RUN_QUEUE_SQL] {
        su.batch_execute(&rewrite_schema(ddl, &schema))
            .await
            .expect("apply current run-plane schema");
    }
}

fn assert_db_code(error: &tokio_postgres::Error, expected: &str, context: &str) {
    let actual = error
        .as_db_error()
        .map_or("non-database-error", |database| database.code().code());
    assert_eq!(actual, expected, "{context}: {error}");
}

#[tokio::test]
async fn run_plane_reconcile_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    from_zero_leg(&su, &url).await;
    current_noop_leg(&su).await;
    two_plane_residency_leg(&su).await;
    forced_rls_owner_refusal_leg(&su).await;
    effect_tables::effect_table_privileges_leg(&su).await;
    partition::global_fifo_queue_leg(&su).await;
    capture_mode_security_leg(&su, &url).await;
    environment_policy_row_security_leg(&su, &url).await;
    persisted_literal_leg(&su).await;
    let cli_database = project_env_database_name(CLI_ORG, CLI_PROJECT, CLI_ENV, CLI_INSTANCE);
    recreate_database(&su, &cli_database).await;
    let cli_url = database_url(&url, &cli_database);
    let cli_su = connect(&cli_url).await;
    environment_identity_leg(&cli_su, &su, &url, &cli_url).await;
    drop(cli_su);
    drop_database(&su, &cli_database).await;
}

/// Run the row-security assertions independently under the database process lock.
#[tokio::test]
async fn environment_policy_row_security_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    environment_policy_row_security_leg(&su, &url).await;
}

/// A table owner remains subject to `FORCE ROW LEVEL SECURITY`. Letting that
/// role reconciliation would hide tenant history, so refuse before even a
/// dry-run observation can claim completeness.
async fn forced_rls_owner_refusal_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    install_current_run_plane(su).await;
    seed_run_admission_facts(su, "hidden", "owner", "dev", "standard").await;
    su.batch_execute(&format!(
        "DO $role$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='rp_owner_no_bypass') THEN \
             CREATE ROLE rp_owner_no_bypass NOSUPERUSER NOBYPASSRLS; \
           END IF; \
         END $role$; \
         ALTER ROLE rp_owner_no_bypass NOSUPERUSER NOBYPASSRLS; \
         DO $temporary$ BEGIN EXECUTE format( \
           'GRANT TEMPORARY ON DATABASE %I TO rp_owner_no_bypass', current_database()); \
         END $temporary$; \
         INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,flow_id,flow_version,package_id,environment,status) VALUES \
           ('hidden','owner','f',1,'owner','dev','running'); \
         ALTER TABLE {SCHEMA}.runs OWNER TO rp_owner_no_bypass; \
         SET ROLE rp_owner_no_bypass; \
         CREATE TEMP TABLE pg_roles \
             (rolname text, rolsuper boolean, rolbypassrls boolean); \
         INSERT INTO pg_temp.pg_roles \
             VALUES ('rp_owner_no_bypass',false,true);"
    ))
    .await
    .expect("seed a forced-RLS row owned by a non-bypass role");

    for apply in [false, true] {
        let error = reconcile_run_plane::reconcile(su, &schema, apply)
            .await
            .expect_err("plain owner must not reconcile forced-RLS history");
        assert!(
            error.to_string().contains("SUPERUSER or BYPASSRLS"),
            "explicit forced-RLS refusal: {error:#}"
        );
    }
    su.batch_execute(
        "RESET ROLE; DROP TABLE pg_temp.pg_roles; \
         DO $temporary$ BEGIN EXECUTE format( \
           'REVOKE TEMPORARY ON DATABASE %I FROM rp_owner_no_bypass', current_database()); \
         END $temporary$;",
    )
    .await
    .expect("restore superuser after owner refusal");

    assert_eq!(
        su.query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
            .await
            .expect("hidden row remains")
            .get::<_, i64>(0),
        1
    );
}

/// Remove one disposable generation role.
///
/// `DROP OWNED BY` reaches only the CURRENT database (plus the shared-object
/// grants, which is where this leg's `CONNECT` lives), and it must run before
/// `DROP ROLE` or the drop is refused. Guarded by an existence check so a leg
/// can call it both before and after minting.
async fn drop_generation_role(su: &Client, role: &str) {
    su.batch_execute(&format!(
        "DO $drop_generation$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
             EXECUTE 'DROP OWNED BY {role}'; \
             EXECUTE 'DROP ROLE {role}'; \
           END IF; \
         END $drop_generation$;"
    ))
    .await
    .expect("drop the disposable generation role");
}

/// Mint one guest-SQL generation LOGIN for `env_key` and dial it
/// (`wamn-0h0g.22.36`).
///
/// # THE BARE `wamn_app` ACL ROLE CANNOT STAND IN FOR THIS
///
/// After `wamn-0h0g.22.6` the tenant floor is
/// `wamn_authority.tenant_key(env_key) = wamn_authority.current_tenant_key()`,
/// and `current_tenant_key` recovers a key ONLY from the guest generation
/// pattern. Under the ACL role it derives NULL, a NULL-compared predicate
/// matches nothing, and the read returns ZERO ROWS WITH NO ERROR — an assertion
/// dialing `wamn_app` cannot tell REFUSED from MATCHED-NOTHING, which is the
/// whole point of a FORCE-RLS probe.
///
/// The login name is DERIVED by the production builder, never spelled, so the
/// digest under test is the digest `provision-project-env` would mint. The
/// production prepare builder also supplies the exact membership edge, direct
/// database `CONNECT`, finite expiry and stable-role hardening. No test
/// authenticates as the stable `wamn_app` ACL role.
async fn mint_guest_generation(su: &Client, url: &str, env_key: &str) -> (String, Client) {
    let database: String = su
        .query_one("SELECT current_database()", &[])
        .await
        .expect("read the reconciled database")
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::EnvKey {
            env_key,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("the guest family takes a tenant scope");
    drop_generation_role(su, &generation).await;
    su.batch_execute(&provision_sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        &database,
        &generation,
        GUEST_GENERATION_PASSWORD,
        "2100-01-01T00:00:00Z",
    ))
    .await
    .expect("mint the guest generation login through the production builder");
    // A SUPERUSER (or BYPASSRLS) FIXTURE MASKS RLS ENTIRELY, so the probe role
    // is asserted unprivileged from `pg_roles` — across everything it inherits,
    // because the attribute is recovered through the whole membership chain.
    let masking: Vec<String> = su
        .query(
            "SELECT rolname FROM pg_catalog.pg_roles \
              WHERE (rolsuper OR rolbypassrls) AND pg_has_role($1, oid, 'USAGE')",
            &[&generation],
        )
        .await
        .expect("probe the generation's superuser/bypassrls reach")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(
        masking.is_empty(),
        "the guest generation reaches an RLS-masking role: {masking:?}"
    );
    let client = connect_as(url, &generation, GUEST_GENERATION_PASSWORD).await;
    (generation, client)
}

/// Project policy and identity rows converge without changing run history.
async fn environment_identity_leg(
    su: &Client,
    system_su: &Client,
    system_url: &str,
    target_url: &str,
) {
    reset(su).await;
    seed_system_env_policy(system_su, "standard").await;
    let schema = schema();

    install_current_run_plane(su).await;

    // The REAL CLI path (arg validation + connect + apply).
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile-run-plane applies");

    env_key_identity_leg(su).await;

    let projected: String = su
        .query_one(
            &format!(
                "SELECT durability_class FROM {SCHEMA}.environment_policies \
                 WHERE env_key='acme--billing--dev'"
            ),
            &[],
        )
        .await
        .expect("read projected standard policy")
        .get(0);
    assert_eq!(projected, "standard");
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,flow_id,flow_version,package_id, \
            environment,durability_class) \
         VALUES ('acme--billing--dev','r-policy-before','f',1,'cat','dev','standard')"
    ))
    .await
    .expect("seed a run before reconciliation");

    su.batch_execute(&format!(
        "DELETE FROM {SCHEMA}.environment_policies WHERE env_key='acme--billing--dev'"
    ))
    .await
    .expect("remove the local policy before testing projection repair");
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: true,
    })
    .await
    .expect("dry-run observes the missing local policy");
    let after_dry_run: i64 = su
        .query_one(
            &format!(
                "SELECT count(*) FROM {SCHEMA}.environment_policies \
                 WHERE env_key='acme--billing--dev'"
            ),
            &[],
        )
        .await
        .expect("read policy after dry-run")
        .get(0);
    assert_eq!(
        after_dry_run, 0,
        "dry-run projected the missing local policy"
    );
    reconcile_run_plane::reconcile_run_plane(ReconcileRunPlaneRequest {
        changes: AuthChanges::default(),
        system_database_url: system_url.to_string(),
        admin_database_url: target_url.to_string(),
        org: "acme".to_string(),
        project: "billing".to_string(),
        env: "dev".to_string(),
        schema: SCHEMA.to_string(),
        dry_run: false,
    })
    .await
    .expect("reconcile restores the missing local policy");
    // A second apply writes no second copy of any identity row.
    env_key_identity_leg(su).await;
    stale_user_leg(su, system_su, system_url, target_url).await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,flow_id,flow_version,package_id, \
            environment,durability_class) \
         VALUES ('acme--billing--dev','r-policy-after','f',1,'cat','dev','standard')"
    ))
    .await
    .expect("seed a run after reconciliation");
    let classes: Vec<(String, String)> = su
        .query(
            &format!(
                "SELECT run_id,durability_class FROM {SCHEMA}.runs \
                 WHERE run_id IN ('r-policy-before','r-policy-after') ORDER BY run_id DESC"
            ),
            &[],
        )
        .await
        .expect("read frozen run classes")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        classes,
        [
            ("r-policy-before".to_string(), "standard".to_string()),
            ("r-policy-after".to_string(), "standard".to_string()),
        ],
        "policy reconciliation must not rewrite stored run classes"
    );

    // Idempotence: a second reconcile plans nothing.
    let again = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("re-plan");
    assert!(again.is_noop(), "re-run is a no-op: {:#?}", again.actions);
}

/// Manifestations 3 + 5 + 6 (the ephemeral-fixture wipe): a database with no
/// project schemas. The fixed runtime roles are cluster-scoped and shared, so
/// this leg must preserve them even when another database has an object owned
/// by `wamn_app`. Dry-run first (strictly read-only), then apply provisions run
/// plane + `catalog`, and a functional smoke as `wamn_app` shows grants + RLS
/// isolation from the applied sections.
async fn from_zero_leg(su: &Client, base_url: &str) {
    reset(su).await;
    let schema = schema();
    let sentinel_database = "wamn_run_plane_from_zero_role_sentinel";
    recreate_database(su, sentinel_database).await;
    let sentinel_url = database_url(base_url, sentinel_database);
    let sentinel = connect(&sentinel_url).await;
    sentinel
        .batch_execute(
            "CREATE TABLE public.wamn_app_role_sentinel (id int PRIMARY KEY); \
             ALTER TABLE public.wamn_app_role_sentinel OWNER TO wamn_app; \
             CREATE TABLE public.wamn_scenario_author_role_sentinel \
               (id int PRIMARY KEY); \
             ALTER TABLE public.wamn_scenario_author_role_sentinel \
               OWNER TO wamn_scenario_author;",
        )
        .await
        .expect("create cross-database runtime-role ownership sentinel");
    drop(sentinel);

    let role_oids_before = su
        .query_one(
            "SELECT (SELECT oid::bigint FROM pg_roles WHERE rolname = 'wamn_app'), \
                    (SELECT oid::bigint FROM pg_roles \
                      WHERE rolname = 'wamn_scenario_author')",
            &[],
        )
        .await
        .expect("snapshot shared runtime roles");
    let role_oids_before = (
        role_oids_before.get::<_, i64>(0),
        role_oids_before.get::<_, i64>(1),
    );

    // --dry-run is STRICTLY read-only: it changes neither shared roles nor tables.
    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("dry-run plans");
    assert!(!dry.is_noop());
    let role_oids_after = su
        .query_one(
            "SELECT (SELECT oid::bigint FROM pg_roles WHERE rolname = 'wamn_app'), \
                    (SELECT oid::bigint FROM pg_roles \
                      WHERE rolname = 'wamn_scenario_author')",
            &[],
        )
        .await
        .expect("re-read shared runtime roles");
    assert_eq!(
        (
            role_oids_after.get::<_, i64>(0),
            role_oids_after.get::<_, i64>(1),
        ),
        role_oids_before,
        "dry-run preserves the shared cluster roles"
    );
    assert!(
        !table_exists(su, SCHEMA, "runs").await,
        "dry-run creates nothing"
    );

    let plan = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("from-zero reconcile applies");
    assert!(!plan.is_noop());

    // Exactly the run-plane record: `run-state.sql` + `run-queue.sql`. The three
    // authoring-test relations left this roster with wamn-0h0g.9.11.2
    // (38860fab) — `deploy/sql/control-portable-store.sql` provisions them into
    // the same schema, and this verb never applies that file.
    for t in [
        "runs",
        "environment_policies",
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "operator_run_actions",
        "run_queue",
    ] {
        assert!(
            table_exists(su, SCHEMA, t).await,
            "run-plane table {t} provisioned"
        );
    }
    for t in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(
            !table_exists(su, SCHEMA, t).await,
            "the portable store's relation {t} is not this verb's to provision"
        );
    }
    for column in [
        "replay_of",
        "root_run_id",
        "parent_run_id",
        "parent_node_id",
        "parent_occurrence",
        "waiting_child_run_id",
        "waiting_child_occurrence",
        "wait_generation",
        "invoke_depth",
        "invoke_root_run_id",
    ] {
        assert!(
            !column_exists(su, "runs", column).await,
            "from-zero schema retains retired child-run column: {column}"
        );
    }
    assert!(indexdef(su, "runs_root").await.is_none());
    assert!(
        !table_exists(su, "catalog", "event_registrations").await,
        "the retired registration relation is absent"
    );
    // The project-authoring relations left `catalog-schema.sql` with
    // wamn-0h0g.9.11.3 (805701ec); `deploy/sql/control-portable-store.sql` owns
    // them and `control_portable_store` pins them. This verb applies the
    // catalog record only, so from-zero must NOT produce them.
    for table in ["authoring_command_audit"] {
        assert!(
            !table_exists(su, "catalog", table).await,
            "the portable store's relation catalog.{table} is not this verb's to provision"
        );
    }

    // Functional smoke as the runtime principal: the sections' grants + RLS
    // hold. wamn-0h0g.22.7 (b1d42599) took every run-plane WRITE away from
    // `wamn_app` — table SELECT and DELETE on `runs`, nothing at all on
    // `run_queue` — so both tenants' rows are seeded as superuser and the guest
    // is shown to READ its own under RLS and to be REFUSED on write.
    //
    // *** THE PRINCIPAL IS A MINTED GENERATION LOGIN, NOT `SET ROLE wamn_app`
    // (wamn-0h0g.22.36). *** After wamn-0h0g.22.6 the floor is
    // `wamn_authority.tenant_key(env_key) = wamn_authority.current_tenant_key()`
    // and `current_tenant_key` recovers a key ONLY from the guest generation
    // pattern. Under the BARE ACL role it derives NULL, a NULL-compared
    // predicate matches nothing, and PostgreSQL returns ZERO ROWS WITH NO
    // ERROR — so the retired probe could not tell REFUSED from MATCHED-NOTHING
    // and read 0 where it demanded 1. The login name is DERIVED by the
    // production builder, never spelled, so the digest under test is the digest
    // `provision-project-env` would mint.
    seed_run_admission_facts(su, "acme--billing--dev", "cat", "dev", "standard").await;
    seed_run_admission_facts(su, "t2", "cat", "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
             (env_key,run_id,flow_id,flow_version,package_id, \
              environment) \
             VALUES ('acme--billing--dev','r1','f',1,'cat','dev'), \
                    ('t2','r2','f',1,'cat','dev'); \
         INSERT INTO {SCHEMA}.run_queue (env_key, run_id) VALUES ('acme--billing--dev', 'r1');"
    ))
    .await
    .expect("seed both tenants' run-plane rows");
    let seeded: i64 = su
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.runs"), &[])
        .await
        .expect("superuser sees the whole table")
        .get(0);
    assert_eq!(seeded, 2, "both tenants' rows exist before the guest reads");

    let (guest_generation, guest) = mint_guest_generation(su, base_url, "acme--billing--dev").await;
    for refused in [
        format!(
            "INSERT INTO {SCHEMA}.runs \
               (env_key,run_id,flow_id,flow_version,package_id,environment) \
             VALUES ('acme--billing--dev','r3','f',1,'cat','dev')"
        ),
        format!(
            "INSERT INTO {SCHEMA}.run_queue (env_key, run_id) VALUES ('acme--billing--dev','r3')"
        ),
    ] {
        let denied = guest
            .batch_execute(&refused)
            .await
            .expect_err("the runtime role writes no run-plane row");
        assert_db_code(&denied, "42501", "runtime-role write refusal");
    }
    // THE POST-STATE, not merely a count: the row the guest reads is ITS OWN,
    // and the foreign tenant's row — shown present above — is absent from the
    // result. One query settles admission and isolation together, so a
    // regression to matched-nothing fails on the left half and a regression to
    // a `USING (true)` floor fails on the right.
    let visible = guest
        .query(
            &format!("SELECT env_key, run_id FROM {SCHEMA}.runs ORDER BY run_id"),
            &[],
        )
        .await
        .expect("tenant read");
    let visible: Vec<(String, String)> =
        visible.iter().map(|row| (row.get(0), row.get(1))).collect();
    assert_eq!(
        visible,
        vec![("acme--billing--dev".to_string(), "r1".to_string())],
        "the guest generation sees exactly its own tenant's row"
    );
    // `runs` is the only run-plane relation the guest role can still read;
    // wamn-0h0g.22.7 (b1d42599) left it nothing at all on `run_queue`.
    let queue_denied = guest
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.run_queue"), &[])
        .await
        .expect_err("the runtime role cannot read the queue at all");
    assert_db_code(&queue_denied, "42501", "runtime-role queue read refusal");
    drop(guest);
    // Roles are CLUSTER-wide: leave none behind for the legs that follow.
    drop_generation_role(su, &guest_generation).await;

    let sentinel = connect(&sentinel_url).await;
    let sentinel_owners = sentinel
        .query_one(
            "SELECT pg_get_userbyid((SELECT relowner FROM pg_class \
                                      WHERE oid = \
                                        'public.wamn_app_role_sentinel'::regclass)), \
                    pg_get_userbyid((SELECT relowner FROM pg_class \
                                      WHERE oid = \
                                        'public.wamn_scenario_author_role_sentinel'::regclass))",
            &[],
        )
        .await
        .expect("read cross-database runtime-role ownership sentinels");
    let app_owner: String = sentinel_owners.get(0);
    let author_owner: String = sentinel_owners.get(1);
    assert_eq!(
        app_owner, "wamn_app",
        "from-zero reconciliation preserves sibling-database runtime ownership"
    );
    assert_eq!(
        author_owner, "wamn_scenario_author",
        "from-zero reconciliation preserves sibling-database author ownership"
    );
    drop(sentinel);
    drop_database(su, sentinel_database).await;
}

async fn visible_environment_policy_rows(app: &Client, env_key: &str) -> i64 {
    app.query_one("SELECT set_config('app.env_key', $1, false)", &[&env_key])
        .await
        .expect("set app tenant for environment-policy probe");
    app.query_one(
        &format!("SELECT count(*) FROM {SCHEMA}.environment_policies"),
        &[],
    )
    .await
    .expect("read visible environment policies")
    .get(0)
}

/// Named mutants for the four independent ways an existing env-policy table
/// can lose tenant confinement. Each repair is followed by a fresh observation
/// showing the catalog converged, not merely that the SQL happened to run.
async fn environment_policy_row_security_leg(su: &Client, url: &str) {
    reset(su).await;
    install_current_run_plane(su).await;
    let schema = schema();
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.environment_policies \
           (env_key, expected_environment, durability_class) \
         VALUES ('acme--billing--dev', 'dev', 'standard')"
    ))
    .await
    .expect("seed projected environment policy");
    let (guest_generation, guest) = mint_guest_generation(su, url, "t2").await;

    let mutants = [
        (
            "disabled RLS",
            format!("ALTER TABLE {SCHEMA}.environment_policies DISABLE ROW LEVEL SECURITY"),
        ),
        (
            "unforced RLS",
            format!("ALTER TABLE {SCHEMA}.environment_policies NO FORCE ROW LEVEL SECURITY"),
        ),
        (
            "missing tenant policy",
            format!(
                "DROP POLICY environment_policies_env_key \
                   ON {SCHEMA}.environment_policies"
            ),
        ),
        (
            "widened tenant policy",
            format!(
                "DROP POLICY environment_policies_env_key \
                   ON {SCHEMA}.environment_policies; \
                 CREATE POLICY environment_policies_env_key \
                   ON {SCHEMA}.environment_policies AS PERMISSIVE \
                   FOR ALL TO wamn_app USING (true) WITH CHECK (true); \
                 CREATE POLICY environment_policies_extra \
                   ON {SCHEMA}.environment_policies FOR SELECT USING (true)"
            ),
        ),
    ];

    for (mutant, mutation) in mutants {
        su.batch_execute(&mutation)
            .await
            .unwrap_or_else(|error| panic!("install {mutant} mutant: {error}"));
        if mutant == "disabled RLS" {
            assert_eq!(
                visible_environment_policy_rows(&guest, "t2").await,
                1,
                "the disabled-RLS mutant must expose the foreign tenant row"
            );
        }

        let dry = reconcile_run_plane::reconcile(su, &schema, false)
            .await
            .unwrap_or_else(|error| panic!("observe {mutant} mutant: {error}"));
        assert_eq!(dry.actions.len(), 1, "{mutant}: {:#?}", dry.actions);
        assert_eq!(dry.actions[0].kind, RunPlaneActionKind::RepairRowSecurity);
        assert_eq!(dry.actions[0].target, "environment_policies.row-security");

        let applied = reconcile_run_plane::reconcile(su, &schema, true)
            .await
            .unwrap_or_else(|error| panic!("repair {mutant} mutant: {error}"));
        assert_eq!(applied.actions, dry.actions, "{mutant} plan changed");
        if mutant == "disabled RLS" {
            assert_eq!(
                visible_environment_policy_rows(&guest, "t2").await,
                0,
                "the repaired policy must hide the foreign tenant row"
            );
        }

        let again = reconcile_run_plane::reconcile(su, &schema, false)
            .await
            .unwrap_or_else(|error| panic!("re-observe repaired {mutant} mutant: {error}"));
        assert!(
            again.is_noop(),
            "{mutant} repair did not converge: {:#?}",
            again.actions
        );
    }
    drop(guest);
    drop_generation_role(su, &guest_generation).await;
}

/// The idempotence contract: a schema AT the schema of record plans nothing —
/// dry-run and apply mode alike.
async fn current_noop_leg(su: &Client) {
    reset(su).await;
    let schema = schema();
    su.batch_execute(CATALOG_SCHEMA_SQL)
        .await
        .expect("apply catalog-schema");
    su.batch_execute(&rewrite_schema(RUN_STATE_SQL, &schema))
        .await
        .expect("apply run-state");
    su.batch_execute(&rewrite_schema(RUN_QUEUE_SQL, &schema))
        .await
        .expect("apply run-queue");

    let dry = reconcile_run_plane::reconcile(su, &schema, false)
        .await
        .expect("dry-run plans");
    assert!(
        dry.is_noop(),
        "current schema dry-run is a no-op: {:#?}",
        dry.actions
    );
    // The record is the six `run-state.sql` tables plus `run_queue`, pinned by
    // `record_tables_are_pinned`. The authoring-test orchestration relations that
    // used to make this twelve left the record with wamn-0h0g.9.11.3 (805701ec);
    // this assertion had been unreachable behind an earlier leg's abort ever since.
    assert_eq!(
        dry.at_target.len(),
        7,
        "all seven run-plane record tables at target: {:?}",
        dry.at_target
    );

    let apply = reconcile_run_plane::reconcile(su, &schema, true)
        .await
        .expect("apply-mode reconcile");
    assert!(
        apply.is_noop(),
        "current schema apply is a no-op: {:#?}",
        apply.actions
    );
}
