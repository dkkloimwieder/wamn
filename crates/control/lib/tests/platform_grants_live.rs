//! Live test of invariant 1 of `docs/plan/platform-config.md` (wamn-aqd0m.1):
//! in a project database, the application role `wamn_app` holds `SELECT` on
//! the listed `platform` tables, `EXECUTE` on `platform.bump_counter`,
//! `platform.assign_counter` and `platform.set_counter`, the
//! existing grants of `platform.write_log`, the engine's claim record, and
//! nothing else in `platform`. It holds no grant on a history table or on
//! `schema_migrations`, and no `credential`, `target` or `target_application`
//! table exists. The test reads the privileges from the server catalogs, on a
//! fresh install and on a database that `project/0025_platform_configuration.sql`
//! and `project/0028_platform_singular_tables.sql`,
//! `project/0029_number_assignment.sql`,
//! `project/0030_number_counter_set.sql` and `project/0031_org_boundary.sql`
//! upgrade. A last test applies 0031 to a database with rows in the old shape
//! (wamn-83eth.6). It holds the process lock of its server, because the
//! schema files create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0025_platform_configuration.sql");
const SINGULAR_TABLES: &str =
    include_str!("../../../../deploy/sql/migrations/project/0028_platform_singular_tables.sql");
const PLURAL_TABLES: &str = include_str!("fixtures/platform-singular-tables-before-0028.sql");
const NUMBER_ASSIGNMENT: &str =
    include_str!("../../../../deploy/sql/migrations/project/0029_number_assignment.sql");
const NUMBER_SETTING: &str =
    include_str!("../../../../deploy/sql/migrations/project/0030_number_counter_set.sql");
const ORG_BOUNDARY: &str =
    include_str!("../../../../deploy/sql/migrations/project/0031_org_boundary.sql");
const TENANT_FLOOR: &str = include_str!("fixtures/org-boundary-before-0031.sql");

/// The tables of `platform` that `wamn_app` reads, as `regclass` prints them. `environment` is the status
/// that the host reads as `wamn_app` on each request
/// (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`).
const READ_TABLES: [&str; 12] = [
    "connection",
    "environment",
    "external_system",
    "integration_allow",
    "number_counter",
    "number_sequence",
    "permission",
    "project_connection",
    "role",
    "setting",
    "\"user\"",
    "user_role",
];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Every privilege in schema `platform` that `wamn_app` or PUBLIC holds: on
/// the schema, its relations, their columns and its routines.
async fn surface(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH grantee AS (SELECT oid FROM pg_roles WHERE rolname = 'wamn_app' UNION ALL SELECT 0::oid), \
                  platform AS (SELECT oid FROM pg_namespace WHERE nspname = 'platform') \
             SELECT entry FROM ( \
               SELECT 'schema platform ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x \
                WHERE n.oid IN (SELECT oid FROM platform) AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'table ' || c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x \
                WHERE c.relnamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid, aclexplode(a.attacl) x \
                WHERE c.relnamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
               UNION ALL \
               SELECT 'routine ' || p.oid::regprocedure::text || ' ' || x.privilege_type \
                 FROM pg_proc p, \
                      aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) x \
                WHERE p.pronamespace IN (SELECT oid FROM platform) \
                  AND x.grantee IN (SELECT oid FROM grantee) \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the wamn_app surface in platform")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// Invariant 1, as the list of privilege entries `surface` reads.
fn invariant_one() -> Vec<String> {
    let mut expected: Vec<String> = READ_TABLES
        .iter()
        .map(|table| format!("table platform.{table} SELECT"))
        .chain([
            "column platform.write_log.result UPDATE".to_owned(),
            "routine platform.assign_counter(uuid,text,text) EXECUTE".to_owned(),
            "routine platform.bump_counter(uuid,text,text) EXECUTE".to_owned(),
            "routine platform.set_counter(uuid,text,text,bigint) EXECUTE".to_owned(),
            "schema platform USAGE".to_owned(),
            "table platform.write_log DELETE".to_owned(),
            "table platform.write_log INSERT".to_owned(),
            "table platform.write_log SELECT".to_owned(),
        ])
        .collect();
    expected.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    expected
}

/// The control-plane tables that no application database may hold.
async fn control_plane_tables(client: &Client) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
              WHERE table_name IN ('credential', 'target', 'target_application', 'integrations')",
            &[],
        )
        .await
        .expect("read the tables of the database")
        .get(0)
}

/// A project database with the catalog and the application schema of this
/// commit, as provisioning installs them.
async fn install(url: &str, schema: &str) -> Client {
    let client = connect(url).await;
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .expect("create the platform roles");
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the catalog");
    client
        .batch_execute(schema)
        .await
        .expect("install the application schema");
    client
}

#[tokio::test]
async fn a_fresh_install_grants_the_application_role_invariant_one() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = install(&url, wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}

#[tokio::test]
async fn the_migration_grants_the_application_role_invariant_one() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = install(&url, include_str!("fixtures/app-schema-before-0038.sql")).await;
    // The database as 0024 left it: the plural table names, no platform
    // configuration tables, the configurations table with its history and
    // grants, and the api_keys read, and user under the tenant floor of
    // before 0031. 0028 and 0031 render the administration surface, which
    // names the run plane schema.
    client
        .batch_execute(&format!("SET wamn.fixture_tenant = 't1'; {TENANT_FLOOR}"))
        .await
        .expect("give user the tenant floor of before 0031");
    client
        .batch_execute(PLURAL_TABLES)
        .await
        .expect("name the role, user_role, permission and api_key tables as before 0028");
    client
        .batch_execute(
            "CREATE SCHEMA wamn_run; \
             DROP FUNCTION platform.set_counter(uuid, text, text, bigint); \
             DROP FUNCTION platform.assign_counter(uuid, text, text); \
             DROP FUNCTION platform.bump_counter(uuid, text, text); \
             DROP TABLE platform.setting_history, platform.number_sequence_history, \
               platform.project_connection_history, platform.integration_allow_history, \
               platform.connection_history, platform.external_system_history; \
             DROP TABLE platform.setting, platform.number_counter, platform.number_sequence, \
               platform.project_connection, platform.integration_allow, platform.connection, \
               platform.external_system; \
             CREATE TABLE platform.configurations ( \
               tenant_id text NOT NULL CHECK (tenant_id <> ''), \
               config_key text NOT NULL, \
               config_value jsonb NOT NULL, \
               created_at timestamptz NOT NULL, \
               created_by uuid NOT NULL, \
               updated_at timestamptz NOT NULL, \
               updated_by uuid NOT NULL, \
               PRIMARY KEY (tenant_id, config_key)); \
             SELECT wamn_history.create_history_table('platform', 'configurations', true); \
             GRANT SELECT, INSERT, UPDATE, DELETE ON platform.configurations TO wamn_app; \
             GRANT INSERT (tenant_id, row_key, type, operation, changed_by, changed_at, \
               transaction_id, before, after) ON platform.configurations_history TO wamn_app; \
             GRANT SELECT ON platform.api_keys TO wamn_app;",
        )
        .await
        .expect("make the database as 0024 left it");
    let before = surface(&client).await;
    assert!(
        before
            .iter()
            .any(|entry| entry == "table platform.configurations UPDATE"),
        "the old database grants the configurations write: {before:#?}"
    );

    client
        .batch_execute(&format!("BEGIN; {MIGRATION} COMMIT;"))
        .await
        .expect("apply the migration");
    client
        .batch_execute(&format!(
            "BEGIN; {SINGULAR_TABLES} {NUMBER_ASSIGNMENT} {NUMBER_SETTING} COMMIT;"
        ))
        .await
        .expect("apply the plural table renames, the number assignment and the number setting");
    client
        .batch_execute(&format!("BEGIN; {ORG_BOUNDARY} COMMIT;"))
        .await
        .expect("drop the tenant floor");
    client
        .batch_execute(include_str!(
            "../../../../deploy/sql/migrations/project/0038_administration_database_boundary.sql"
        ))
        .await
        .unwrap();
    assert_eq!(surface(&client).await, invariant_one());
    assert_eq!(control_plane_tables(&client).await, 0);
}

/// Project migration 0031 on a database with rows in the old shape: each row
/// stays, loses its tenant, and keeps its history under its new key. The
/// single-column keys hold, and the counter functions run on the sequence id
/// alone (wamn-83eth.6, wamn-9e4ks).
#[tokio::test]
async fn the_org_boundary_migration_keeps_the_rows_of_the_old_shape() {
    const USER: &str = "00000000-0000-4000-8000-000000000031";
    const SYSTEM: &str = "00000000-0000-4000-8000-000000000032";
    const CONNECTION: &str = "00000000-0000-4000-8000-000000000033";
    const SEQUENCE: &str = "00000000-0000-4000-8000-000000000034";
    const TABLES: [&str; 8] = [
        "user",
        "external_system",
        "connection",
        "integration_allow",
        "project_connection",
        "number_sequence",
        "number_counter",
        "setting",
    ];
    let url = locked_database::database(wamn_test_postgres::database);
    let client = install(&url, include_str!("fixtures/app-schema-before-0038.sql")).await;
    client
        .batch_execute(&format!(
            "CREATE SCHEMA wamn_run; SET wamn.fixture_tenant = 't1'; {TENANT_FLOOR}"
        ))
        .await
        .expect("give the tables the tenant floor of before 0031");
    client
        .batch_execute(&format!(
            "SET app.user_id = '{USER}'; SET app.operation = 'admin:org-boundary-fixture'; \
             INSERT INTO platform.\"user\" (tenant_id, id, type, email) \
               VALUES ('t1', '{USER}', 'user', 'clerk@example.invalid'); \
             INSERT INTO platform.role (tenant_id, name) VALUES ('t1', 'clerk'); \
             INSERT INTO platform.user_role (tenant_id, user_id, role_name) \
               VALUES ('t1', '{USER}', 'clerk'); \
             INSERT INTO platform.api_key (tenant_id, user_id, name, key_hash, prefix) \
               VALUES ('t1', '{USER}', 'ci', 'hash-1', 'wk_a'); \
             INSERT INTO platform.external_system (tenant_id, id, integration, name) \
               VALUES ('t1', '{SYSTEM}', 'netsuite', 'erp'); \
             INSERT INTO platform.connection \
               (tenant_id, id, integration, external_system_id, name, transport, exchange) \
               VALUES ('t1', '{CONNECTION}', 'netsuite', '{SYSTEM}', 'erp-rest', 'https', 'request'); \
             INSERT INTO platform.project_connection (tenant_id, project_id, connection_id) \
               VALUES ('t1', 'shop', '{CONNECTION}'); \
             INSERT INTO platform.integration_allow (tenant_id, scope_type, scope_id, integration) \
               VALUES ('t1', 'org', 'acme', 'netsuite'); \
             INSERT INTO platform.number_sequence (tenant_id, id, application, name, pattern) \
               VALUES ('t1', '{SEQUENCE}', 'receiving', 'po', 'P-{{seq:po}}'); \
             INSERT INTO platform.number_counter (tenant_id, sequence_id, counter, key_value, value) \
               VALUES ('t1', '{SEQUENCE}', 'po', '', 7); \
             INSERT INTO platform.setting (tenant_id, scope_type, scope_id, key, value) \
               VALUES ('t1', 'org', 'acme', 'locale', '\"en\"');"
        ))
        .await
        .expect("write one row of each table in the old shape");

    client
        .batch_execute(&format!("BEGIN; {ORG_BOUNDARY} COMMIT;"))
        .await
        .expect("drop the tenant floor");

    let relations: Vec<String> = TABLES
        .iter()
        .flat_map(|table| [(*table).to_owned(), format!("{table}_history")])
        .collect();
    let tenant_columns: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
              WHERE c.relnamespace = 'platform'::regnamespace AND a.attname = 'tenant_id' \
                AND NOT a.attisdropped AND c.relname = ANY($1)",
            &[&relations],
        )
        .await
        .expect("read the tenant columns")
        .get(0);
    assert_eq!(tenant_columns, 0, "no table of the scope keeps tenant_id");
    for table in TABLES {
        let rows: i64 = client
            .query_one(&format!("SELECT count(*) FROM platform.\"{table}\""), &[])
            .await
            .expect("count the rows")
            .get(0);
        assert_eq!(rows, 1, "platform.{table} keeps its row");
    }
    for table in TABLES.iter().filter(|table| **table != "number_counter") {
        let row = client
            .query_one(
                &format!(
                    "SELECT count(*), count(*) FILTER (WHERE row_key ? 'tenant_id') \
                       FROM platform.{table}_history"
                ),
                &[],
            )
            .await
            .expect("read the history entries");
        assert_eq!(
            (row.get::<_, i64>(0), row.get::<_, i64>(1)),
            (1, 0),
            "platform.{table}_history keeps its entry under the new key"
        );
    }
    assert_eq!(surface(&client).await, invariant_one());

    // The function reads no claim: the plugin resolves the sequence id from
    // the application in host state (wamn-9e4ks).
    let bumped: i64 = client
        .query_one(
            &format!("SELECT platform.bump_counter('{SEQUENCE}', 'po', '')"),
            &[],
        )
        .await
        .expect("bump the counter of the sequence")
        .get(0);
    assert_eq!(bumped, 7);
    let refused = client
        .batch_execute(
            "SELECT platform.bump_counter('cccccccc-cccc-4ccc-8ccc-cccccccccccc', 'po', '')",
        )
        .await
        .expect_err("an id that is not a sequence is refused");
    assert_eq!(
        refused.code(),
        Some(&tokio_postgres::error::SqlState::UNDEFINED_OBJECT),
        "{refused}"
    );
    client
        .batch_execute(&format!(
            "DELETE FROM platform.\"user\" WHERE id = '{USER}'"
        ))
        .await
        .expect("remove the user");
    let cascaded: i64 = client
        .query_one(
            "SELECT (SELECT count(*) FROM platform.user_role) \
                  + (SELECT count(*) FROM platform.api_key)",
            &[],
        )
        .await
        .expect("count the rows of the user")
        .get(0);
    assert_eq!(
        cascaded, 0,
        "the user's foreign keys remove its role and key rows"
    );
}

/// Server-owned shape, including keys, policies, indexes and column grants.
async fn administration_shape(client: &Client) -> Vec<String> {
    client.query(
        "WITH relations AS (SELECT oid FROM pg_class WHERE relnamespace = 'platform'::regnamespace
          AND relname IN ('role','user_role','permission','api_key','environment',
          'role_history','user_role_history','permission_history','api_key_history','environment_history'))
        SELECT entry FROM (
          SELECT 'column ' || c.relname || '.' || a.attname || ' ' || format_type(a.atttypid,a.atttypmod)
            || ' ' || a.attnotnull || ' ' || coalesce(pg_get_expr(d.adbin,d.adrelid),'') AS entry
          FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN relations r ON r.oid=c.oid
          LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
          WHERE a.attnum>0 AND NOT a.attisdropped
          UNION ALL SELECT 'constraint ' || conrelid::regclass || ' ' || conname || ' ' || pg_get_constraintdef(c.oid)
          FROM pg_constraint c JOIN relations r ON r.oid=c.conrelid
          UNION ALL SELECT 'index ' || pg_get_indexdef(i.indexrelid) FROM pg_index i JOIN relations r ON r.oid=i.indrelid
          UNION ALL SELECT 'policy ' || polrelid::regclass || ' ' || polname FROM pg_policy p JOIN relations r ON r.oid=p.polrelid
          UNION ALL SELECT 'security ' || c.relname || ' ' || relrowsecurity || ' ' || relforcerowsecurity
          FROM pg_class c JOIN relations r ON r.oid=c.oid
          UNION ALL SELECT 'table grant ' || c.relname || ' ' || x.grantee::regrole || ' ' || x.privilege_type
          FROM pg_class c JOIN relations r ON r.oid=c.oid, aclexplode(c.relacl) x WHERE x.grantee<>c.relowner
          UNION ALL SELECT 'column grant ' || c.relname || '.' || a.attname || ' ' || x.grantee::regrole || ' ' || x.privilege_type
          FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN relations r ON r.oid=c.oid,
          aclexplode(a.attacl) x WHERE NOT a.attisdropped AND x.grantee<>c.relowner
        ) q ORDER BY entry COLLATE \"C\"",
        &[],
    ).await.expect("read administration definitions").iter().map(|r| r.get(0)).collect()
}

#[tokio::test]
async fn administration_boundary_retains_rows_history_and_real_role_access() {
    use wamn_control_provision::schema_migrations::MigrationTarget;
    use wamn_control_provision::sql;
    use wamn_platform_identity::application;
    use wamn_platform_identity::auth_changes::AuthDrops;
    use wamn_platform_identity::org::EnvironmentStatus;

    const RETAINED_IMAGES: &str = "SELECT image FROM (
        SELECT 'role' || position || before::text || after::text AS image FROM platform.role_history
        UNION ALL SELECT 'user_role' || position || before::text || after::text FROM platform.user_role_history
        UNION ALL SELECT 'permission' || position || before::text || after::text FROM platform.permission_history
        UNION ALL SELECT 'api_key' || position || before::text || after::text FROM platform.api_key_history
        UNION ALL SELECT 'environment' || position || before::text || after::text FROM platform.environment_history
        ) entries ORDER BY image COLLATE \"C\"";

    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = install(&url, include_str!("fixtures/app-schema-before-0038.sql")).await;
    client
        .batch_execute(
            "ALTER TABLE catalog.connection_bindings DROP CONSTRAINT connection_bindings_pkey;
             ALTER TABLE catalog.connection_bindings DROP COLUMN mode, DROP COLUMN package_id;
             ALTER TABLE catalog.connection_bindings ADD PRIMARY KEY (tenant_id, manifest_digest, component_digest, store_alias);",
        )
        .await
        .expect("restore the catalog before migration 0040");
    client.batch_execute("CREATE SCHEMA wamn_run;
        SET app.user_id = '00000000-0000-4000-8000-000000000035';
        SET app.operation = 'admin:boundary-fixture';
        INSERT INTO platform.\"user\" (id,type,email) VALUES
          ('00000000-0000-4000-8000-000000000035','user','boundary@example.invalid');
        INSERT INTO platform.role (tenant_id,name) VALUES ('env','reader');
        INSERT INTO platform.user_role (tenant_id,user_id,role_name) VALUES
          ('env','00000000-0000-4000-8000-000000000035','reader');
        INSERT INTO platform.permission (tenant_id,role_name,permission) VALUES ('env','reader','fixture:record/get');
        INSERT INTO platform.api_key (tenant_id,user_id,name,key_hash,prefix) VALUES
          ('env','00000000-0000-4000-8000-000000000035','key','digest','prefix');
        INSERT INTO platform.environment (tenant_id,status) VALUES ('env','inactive');").await.unwrap();
    client
        .batch_execute(MigrationTarget::Project.record_table_sql())
        .await
        .unwrap();
    for migration in MigrationTarget::Project
        .migrations()
        .iter()
        .filter(|m| m.ordinal() < 38)
    {
        client
            .execute(
                &MigrationTarget::Project.record_sql(),
                &[
                    &migration.ordinal(),
                    &migration.relative_path,
                    &migration.sha256(),
                ],
            )
            .await
            .unwrap();
    }
    let prior: Vec<String> = client
        .query(RETAINED_IMAGES, &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    for _ in 0..2 {
        wamn_control::upgrade_schema::upgrade_project(&mut client)
            .await
            .unwrap();
    }
    let entries: Vec<String> = client
        .query(RETAINED_IMAGES, &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(entries, prior, "retained row images stay unchanged");
    for table in ["role", "user_role", "permission", "api_key", "environment"] {
        let row=client.query_one(&format!("SELECT (SELECT count(*) FROM platform.{table}), count(*), bool_and(NOT (row_key ? 'tenant_id')) FROM platform.{table}_history"), &[]).await.unwrap();
        assert_eq!(
            (
                row.get::<_, i64>(0),
                row.get::<_, i64>(1),
                row.get::<_, bool>(2)
            ),
            (1, 1, true)
        );
    }
    client
        .batch_execute(&sql::grant_administration_surface_sql("wamn_run"))
        .await
        .unwrap();
    client
        .batch_execute(&sql::grant_session_role_reader_surface_sql())
        .await
        .unwrap();
    let migrated = administration_shape(&client).await;
    assert!(
        !migrated.iter().any(|s| s.contains("tenant_id")
            || s.contains("tenant_key")
            || s.starts_with("policy "))
    );
    assert_eq!(control_plane_tables(&client).await, 0);
    assert_eq!(surface(&client).await, invariant_one());
    client
        .batch_execute("SET ROLE wamn_administration")
        .await
        .unwrap();
    assert!(
        !client
            .query_one(
                "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert!(!client.query_one("SELECT has_function_privilege(current_user,p.oid,'EXECUTE') FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='wamn_authority' AND p.proname='tenant_key'",&[]).await.unwrap().get::<_,bool>(0));
    let tx = client.transaction().await.unwrap();
    let mut drops = AuthDrops::default();
    wamn_control::role_permission::create_role(&tx, "env", "writer", &mut drops)
        .await
        .unwrap();
    wamn_control::role_permission::grant_permission(
        &tx,
        "env",
        "writer",
        "fixture:record/update",
        &mut drops,
    )
    .await
    .unwrap();
    application::grant_user_role(
        &tx,
        "env",
        "00000000-0000-4000-8000-000000000035",
        "writer",
        &mut drops,
    )
    .await
    .unwrap();
    application::write_environment_status(&tx, "env", EnvironmentStatus::Active, &mut drops)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!drops.is_empty());
    client
        .batch_execute("RESET ROLE; SET ROLE wamn_session_role_reader")
        .await
        .unwrap();
    let roles: Vec<String> = client
        .query(
            "SELECT role_name FROM platform.user_role ORDER BY role_name",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(roles, ["reader", "writer"]);
    client
        .batch_execute("RESET ROLE; SET ROLE wamn_app")
        .await
        .unwrap();
    assert_eq!(
        client
            .query_one("SELECT status FROM platform.environment", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "active"
    );
    assert!(
        client
            .batch_execute("UPDATE platform.environment SET status='inactive'")
            .await
            .is_err()
    );
    client.batch_execute("RESET ROLE").await.unwrap();
    let duplicate = client
        .batch_execute("INSERT INTO platform.environment (status) VALUES ('active')")
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    client.batch_execute("SET ROLE wamn_administration; DELETE FROM platform.role WHERE name='writer'; RESET ROLE").await.unwrap();
    assert_eq!(client.query_one("SELECT (SELECT count(*) FROM platform.user_role WHERE role_name='writer') + (SELECT count(*) FROM platform.permission WHERE role_name='writer')",&[]).await.unwrap().get::<_,i64>(0),0);
    client
        .batch_execute("DROP SCHEMA platform CASCADE")
        .await
        .unwrap();
    client
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await
        .unwrap();
    client
        .batch_execute(&sql::grant_administration_surface_sql("wamn_run"))
        .await
        .unwrap();
    client
        .batch_execute(&sql::grant_session_role_reader_surface_sql())
        .await
        .unwrap();
    assert_eq!(
        administration_shape(&client).await,
        migrated,
        "fresh and migrated definitions agree"
    );
}

#[tokio::test]
async fn integration_and_target_seeds_live_only_in_the_system_database() {
    use wamn_control_provision::schema_migrations::MigrationTarget;
    let mut server = wamn_test_postgres::start(&[]).unwrap();
    let database = server.create_database("wamn_system").unwrap();
    let admin = connect(database.url()).await;
    admin
        .batch_execute(
            "CREATE ROLE wamn_db_owner NOLOGIN; CREATE ROLE wamn_system LOGIN PASSWORD 'c31-system-fixture';
        ALTER DATABASE wamn_system OWNER TO wamn_system; SET ROLE wamn_system",
        )
        .await
        .unwrap();
    admin
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await
        .unwrap();
    wamn_control::upgrade_schema::record_fresh_install(&admin, MigrationTarget::System)
        .await
        .unwrap();
    let expected = vec![
        "gcs:host:false".to_owned(),
        "http:host:false".to_owned(),
        "s3:host:false".to_owned(),
    ];
    let seeds = async || -> Vec<String> {
        admin.query("SELECT code || ':' || form || ':' || exclusive FROM registry.integrations ORDER BY code",&[]).await.unwrap().iter().map(|r|r.get(0)).collect()
    };
    assert_eq!(seeds().await, expected);
    let target: String=admin.query_one("SELECT name || ':' || type || ':' || status FROM registry.target WHERE last_seen_at IS NULL",&[]).await.unwrap().get(0);
    assert_eq!(target, "cloud:cloud:active");
    for relation in [
        "registry.meta",
        "registry.orgs",
        "registry.env_policies",
        "registry.projects",
        "registry.project_envs",
        "registry.retired_project_envs",
        "registry.capture_gap",
        "registry.generation_passwords",
        "identity.session_keys",
        "identity.session_signing_state",
        "identity.password_attempts",
    ] {
        let created = if relation == "registry.capture_gap" {
            "ALTER COLUMN created_at SET DEFAULT now()"
        } else {
            "DROP COLUMN created_at"
        };
        admin.batch_execute(&format!("DROP TRIGGER wamn_record_history_stamp ON {relation}; ALTER TABLE {relation} {created}, DROP COLUMN created_by, DROP COLUMN updated_at, DROP COLUMN updated_by")).await.unwrap();
    }
    admin.batch_execute("DROP TABLE registry.environment_connection_binding; DROP TRIGGER environment_target_immutable ON registry.project_envs; DROP FUNCTION registry.keep_environment_target(); ALTER TABLE registry.project_envs DROP COLUMN target_id; DROP TABLE registry.credential_import, registry.integration_allow, registry.project_connection, registry.connection_generation, identity.credential, registry.connection, registry.external_system CASCADE; DROP FUNCTION registry.reject_connection_generation_change(); DROP FUNCTION registry.connection_integrations(); DROP TABLE registry.integrations,registry.target; DELETE FROM registry.schema_migrations WHERE ordinal>=29").await.unwrap();
    let mut owner_url = url::Url::parse(database.url()).unwrap();
    owner_url.set_username("wamn_system").unwrap();
    owner_url.set_password(Some("c31-system-fixture")).unwrap();
    let request = wamn_control::upgrade_schema::UpgradeSchemaRequest {
        system_database_url: owner_url.into(),
        admin_database_url: None,
        baseline: None,
        confirm: true,
    };
    for _ in 0..2 {
        wamn_control::upgrade_schema::upgrade_schema(&request)
            .await
            .unwrap();
    }
    assert_eq!(seeds().await, expected);
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM registry.target WHERE name='cloud' AND type='cloud'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM registry.schema_migrations WHERE ordinal=29",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let app = server.create_database("c31_application").unwrap();
    let application = install(app.url(), wamn_control_provision::APP_SCHEMA_SQL).await;
    assert_eq!(control_plane_tables(&application).await, 0);
}

#[tokio::test]
async fn administration_migration_refuses_duplicate_keys_without_losing_rows() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = install(&url, include_str!("fixtures/app-schema-before-0038.sql")).await;
    client
        .batch_execute(
            "SET app.user_id='00000000-0000-4000-8000-000000000035';
        SET app.operation='admin:duplicate-fixture';
        INSERT INTO platform.role (tenant_id,name) VALUES ('one','reader'),('two','reader');",
        )
        .await
        .unwrap();
    client.batch_execute("BEGIN").await.unwrap();
    let error = client
        .batch_execute(include_str!(
            "../../../../deploy/sql/migrations/project/0038_administration_database_boundary.sql"
        ))
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    client.batch_execute("ROLLBACK").await.unwrap();
    assert_eq!(
        client
            .query_one(
                "SELECT count(DISTINCT tenant_id) FROM platform.role WHERE name='reader'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM platform.role_history WHERE row_key ? 'tenant_id'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}

/// Effective PostgreSQL privilege checks include inherited roles and PUBLIC.
#[tokio::test]
async fn credentials_are_inaccessible_to_application_roles() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let client = connect(database.url()).await;
    client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$").await.unwrap();
    let exposed: Vec<String> = client.query(
        "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND (c.relname LIKE '%credential%' OR c.relname IN ('pats','password_tokens','password_logins')) AND c.relkind IN ('r','p','v','m','f') AND (has_table_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') OR has_any_column_privilege('wamn_app',c.oid,'SELECT,INSERT,UPDATE,REFERENCES')) ORDER BY 1",
        &[],
    ).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        exposed.is_empty(),
        "credential relations exposed to wamn_app: {exposed:?}"
    );
    let routines: Vec<String> = client.query(
        "SELECT p.oid::regprocedure::text FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_%' AND (p.prosrc ~* '(credential|password|secret|token|identity[.]pats)' OR p.prorettype IN (SELECT reltype FROM pg_class WHERE relname IN ('credential','password_credentials','password_tokens','password_logins','pats'))) AND has_function_privilege('wamn_app',p.oid,'EXECUTE') ORDER BY 1",
        &[],
    ).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert!(
        routines.is_empty(),
        "credential routines exposed through inherited or PUBLIC access: {routines:?}"
    );
}
