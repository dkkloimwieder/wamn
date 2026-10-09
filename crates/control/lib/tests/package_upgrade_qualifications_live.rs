//! Fresh-install and installed-database coverage for immutable upgrade evidence.

use tokio_postgres::{Client, NoTls, error::SqlState};
use wamn_control::upgrade_schema::{UpgradeSchemaRequest, upgrade_schema};

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Describe the database-enforced contract without database-local object IDs.
/// A column is numbered by its place among the live columns, because a
/// migration that drops a column leaves a gap in `attnum`.
async fn carrier_shape(client: &Client, table: &str) -> Vec<String> {
    client
        .query(
            "WITH carrier AS ( \
               SELECT oid, relrowsecurity, relforcerowsecurity, relacl \
                 FROM pg_class WHERE oid = $1::text::regclass \
             ) SELECT fact FROM ( \
               SELECT 'column ' || (SELECT count(*) FROM pg_attribute AS live \
                        WHERE live.attrelid = a.attrelid AND live.attnum > 0 \
                          AND NOT live.attisdropped AND live.attnum <= a.attnum) \
                      || ' ' || a.attname || ' ' || \
                      format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull || ' ' || \
                      coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS fact \
                 FROM pg_attribute a JOIN carrier c ON c.oid = a.attrelid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                 FROM pg_constraint WHERE conrelid = (SELECT oid FROM carrier) \
               UNION ALL SELECT 'index ' || pg_get_indexdef(indexrelid) FROM pg_index \
                WHERE indrelid = (SELECT oid FROM carrier) \
               UNION ALL SELECT 'policy ' || policyname || ' ' || permissive || ' ' || \
                      roles::text || ' ' || cmd || ' ' || qual || ' ' || with_check \
                 FROM pg_policies WHERE schemaname = 'catalog' \
                  AND tablename = $2 \
               UNION ALL SELECT 'trigger ' || pg_get_triggerdef(oid) FROM pg_trigger \
                WHERE tgrelid = (SELECT oid FROM carrier) AND NOT tgisinternal \
               UNION ALL SELECT 'security ' || relrowsecurity || ' ' || relforcerowsecurity \
                      || ' ' || coalesce(relacl::text, '') FROM carrier \
             ) facts ORDER BY fact COLLATE \"C\"",
            &[&format!("catalog.{table}"), &table],
        )
        .await
        .expect("describe the catalog carrier")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

const INSERT_EVIDENCE: &str = "\
INSERT INTO catalog.package_upgrade_qualifications \
    (tenant_id, package_id, candidate_package_version, canonical_bytes, result_sha256, \
     predecessor_manifest_digest) \
VALUES ('upgrade-live', 'platform_fixture', '2.1.0', 'proof'::bytea, \
        'sha256:' || encode(sha256('proof'::bytea), 'hex'), \
        'sha256:' || encode(sha256('{}'::bytea), 'hex'))";

#[tokio::test]
async fn fresh_and_upgrade_schema_install_the_same_immutable_carrier() {
    let mut server = wamn_test_postgres::start(&[]).expect("start disposable PostgreSQL");
    let admin = connect(server.database("postgres").unwrap().url()).await;
    let database =
        wamn_control_provision::project_env_database_name("upgrade", "carrier", "test", "a1b2c3d4");
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {}",
            wamn_pg_core::quote_ident(&database)
        ))
        .await
        .expect("create the project database");
    admin
        .batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;")
        .await
        .expect("create catalog roles");
    let coordinate = server.database(&database).unwrap();
    let mut client = connect(coordinate.url()).await;
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the fresh catalog");
    let fresh = carrier_shape(&client, "package_upgrade_qualifications").await;
    let fresh_stages = carrier_shape(&client, "package_upgrade_stages").await;
    let fresh_owners = carrier_shape(&client, "package_definition_owners").await;
    assert!(
        fresh
            .iter()
            .any(|fact| fact.starts_with("security true true"))
    );
    assert!(fresh.iter().any(|fact| fact.contains("_immutable")));

    // A pre-change project holds migrations 1–8 without either upgrade carrier,
    // and names its releases by integer id: the release tables, the head, the
    // connection bindings and the run pin that migration 0012 converts.
    // Only this registry projection is needed to register the disposable target,
    // with the permission rows that migration 0011 prunes and the wiring
    // activation tables that migration 0013 drops.
    client
        .batch_execute(
            "DROP TABLE catalog.package_upgrade_qualifications; \
             ALTER TABLE catalog.effective_release_heads \
               DROP CONSTRAINT effective_release_heads_release_fkey, \
               DROP COLUMN manifest_digest, \
               ADD COLUMN effective_release_id int NOT NULL; \
             ALTER TABLE catalog.connection_bindings \
               DROP CONSTRAINT connection_bindings_release_fkey, \
               DROP CONSTRAINT connection_bindings_pkey, \
               DROP COLUMN manifest_digest, \
               ADD COLUMN effective_release_id int NOT NULL, \
               ADD CONSTRAINT connection_bindings_pkey \
                 PRIMARY KEY (tenant_id, effective_release_id, component_digest, store_alias); \
             DROP TABLE catalog.releases; \
             CREATE TABLE catalog.effective_releases ( \
               tenant_id text NOT NULL, effective_release_id int NOT NULL, \
               environment text NOT NULL, verified_publisher_principal text, \
               created_at timestamptz NOT NULL DEFAULT now(), \
               CONSTRAINT effective_releases_pkey PRIMARY KEY (tenant_id, effective_release_id), \
               CONSTRAINT effective_releases_environment_key \
                 UNIQUE (tenant_id, effective_release_id, environment)); \
             CREATE TABLE catalog.effective_release_packages ( \
               tenant_id text NOT NULL, effective_release_id int NOT NULL, \
               package_id text NOT NULL, package_version text NOT NULL, \
               PRIMARY KEY (tenant_id, effective_release_id, package_id)); \
             CREATE TRIGGER effective_release_packages_seal_coordinate \
               BEFORE INSERT ON catalog.effective_release_packages FOR EACH ROW \
               EXECUTE FUNCTION catalog.lock_package_coordinate_for_release_membership(); \
             CREATE TABLE catalog.release_manifest_snapshots ( \
               tenant_id text NOT NULL, effective_release_id int NOT NULL, \
               manifest_digest text NOT NULL, canonical_bytes bytea NOT NULL, \
               PRIMARY KEY (tenant_id, effective_release_id)); \
             CREATE FUNCTION catalog.guard_release_component_insert() RETURNS trigger \
               LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
             CREATE TABLE catalog.release_components ( \
               tenant_id text NOT NULL, effective_release_id int NOT NULL); \
             CREATE TRIGGER release_components_snapshot_seal \
               BEFORE INSERT ON catalog.release_components FOR EACH ROW \
               EXECUTE FUNCTION catalog.guard_release_component_insert(); \
             ALTER TABLE catalog.effective_release_heads \
               ADD CONSTRAINT effective_release_heads_release_fkey \
                 FOREIGN KEY (tenant_id, effective_release_id, environment) \
                 REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment); \
             ALTER TABLE catalog.connection_bindings \
               ADD CONSTRAINT connection_bindings_release_fkey \
                 FOREIGN KEY (tenant_id, effective_release_id, environment) \
                 REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment); \
             CREATE SCHEMA wamn_run; \
             CREATE TABLE wamn_run.runs ( \
               tenant_id text NOT NULL, run_id text NOT NULL, package_id text NOT NULL, \
               effective_release_id int NOT NULL, environment text NOT NULL, \
               flow_id text, flow_version int, capture_mode text, durability_class text, \
               wiring_id text, wiring_version int, wiring_hash text, \
               binding_world_json jsonb, manifest_digest text, service_principal_id uuid, \
               CONSTRAINT runs_check \
                 CHECK (package_id <> '' AND effective_release_id > 0 AND environment <> ''), \
               CONSTRAINT runs_release_fk FOREIGN KEY (tenant_id, effective_release_id) \
                 REFERENCES catalog.effective_releases (tenant_id, effective_release_id)); \
             CREATE INDEX runs_release ON wamn_run.runs (tenant_id, effective_release_id); \
             CREATE FUNCTION wamn_run.guard_run_admission_pins_immutable() RETURNS trigger \
               LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
             CREATE TRIGGER runs_admission_pins_immutable BEFORE UPDATE ON wamn_run.runs \
               FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable(); \
             INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('upgrade-live', 'platform_fixture', '2.0.0', 'sha256:' || repeat('b', 64)); \
             INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
             VALUES ('upgrade-live', 1, 'test'), ('upgrade-live', 2, 'test'); \
             INSERT INTO catalog.release_manifest_snapshots \
               (tenant_id, effective_release_id, manifest_digest, canonical_bytes) \
             VALUES ('upgrade-live', 1, \
               'sha256:' || encode(sha256('{\"release\":1}'::bytea), 'hex'), \
               '{\"release\":1}'::bytea); \
             INSERT INTO catalog.effective_release_heads \
               (tenant_id, environment, effective_release_id) VALUES ('upgrade-live', 'test', 1); \
             INSERT INTO wamn_run.runs (tenant_id, run_id, package_id, effective_release_id, environment) \
             VALUES ('upgrade-live', 'pinned', 'platform_fixture', 1, 'test'), \
                    ('upgrade-live', 'unfrozen', 'platform_fixture', 2, 'test'); \
             DROP TABLE catalog.package_upgrade_stages; \
             DROP FUNCTION catalog.guard_package_upgrade_stage_change(); \
             DROP INDEX catalog.package_definition_owners_synchronization_function; \
             ALTER TABLE catalog.package_definition_owners \
               DROP CONSTRAINT package_definition_owners_definition_type_check, \
               ADD CONSTRAINT package_definition_owners_definition_type_check \
                 CHECK (definition_type IN ('relation', 'field', 'constraint')); \
             CREATE TABLE catalog.wiring_activation (tenant_id text); \
             CREATE TABLE catalog.wiring_activation_events (tenant_id text); \
             CREATE SCHEMA app_system; \
             CREATE TABLE app_system.permissions (permission text, required_by text); \
             CREATE SCHEMA registry; \
             CREATE TABLE registry.project_envs \
               (org text, project text, env text, instance_suffix text); \
             INSERT INTO registry.project_envs \
               VALUES ('upgrade', 'carrier', 'test', 'a1b2c3d4');",
        )
        .await
        .expect("prepare the installed pre-change database");
    let request = UpgradeSchemaRequest {
        system_database_url: coordinate.url().to_owned(),
        admin_database_url: Some(coordinate.url().to_owned()),
        baseline: Some(8),
        confirm: true,
    };
    upgrade_schema(&request)
        .await
        .expect("upgrade the installed project through the production verb");
    assert_eq!(
        carrier_shape(&client, "package_upgrade_qualifications").await,
        fresh
    );
    assert_eq!(
        carrier_shape(&client, "package_upgrade_stages").await,
        fresh_stages
    );
    assert_eq!(
        carrier_shape(&client, "package_definition_owners").await,
        fresh_owners
    );
    let migrations: i64 = client
        .query_one("SELECT count(*) FROM app_system.schema_migrations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(migrations, 13);
    // Migration 0012 keyed the release rows by their digest: the snapshot became
    // the cached release, the head took its digest, and each run took the digest
    // of its frozen release or, with no frozen release, no pin.
    let converted: String = client
        .query_one(
            "SELECT (SELECT string_agg(manifest_digest, ',') FROM catalog.releases) || '|' || \
                    (SELECT manifest_digest FROM catalog.effective_release_heads) || '|' || \
                    (SELECT string_agg(run_id || '=' || coalesce(manifest_digest, 'none'), ',' \
                       ORDER BY run_id) FROM wamn_run.runs)",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let release = "sha256:21621f6bd9bc2769154aa2938726e891ecbc7929cd7be941b60ec776703ed394";
    assert_eq!(
        converted,
        format!("{release}|{release}|pinned={release},unfrozen=none")
    );
    upgrade_schema(&UpgradeSchemaRequest {
        baseline: None,
        ..request
    })
    .await
    .expect("retry an installed schema upgrade without mutation");

    client
        .batch_execute(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ('upgrade-live', 'platform_fixture', '2.1.0', 'sha256:' || repeat('a', 64)); \
             INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) \
             VALUES ('upgrade-live', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea);",
        )
        .await
        .expect("seed the exact candidate and predecessor identities");
    let mismatch =
        INSERT_EVIDENCE.replace("encode(sha256('proof'::bytea), 'hex')", "repeat('0', 64)");
    let error = client.batch_execute(&mismatch).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));

    let transaction = client.transaction().await.unwrap();
    transaction.batch_execute(INSERT_EVIDENCE).await.unwrap();
    transaction.rollback().await.unwrap();
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM catalog.package_upgrade_qualifications",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        count, 0,
        "evidence rolls back with its enclosing transaction"
    );
    client.batch_execute(INSERT_EVIDENCE).await.unwrap();
    let error = client.batch_execute(INSERT_EVIDENCE).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::UNIQUE_VIOLATION));
    for mutation in [
        "UPDATE catalog.package_upgrade_qualifications SET recorded_at = now()",
        "DELETE FROM catalog.package_upgrade_qualifications",
    ] {
        let error = client.batch_execute(mutation).await.unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
        );
    }
    let app_access: bool = client
        .query_one(
            "SELECT has_table_privilege('wamn_app', \
             'catalog.package_upgrade_qualifications', 'SELECT,INSERT,UPDATE,DELETE')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        !app_access,
        "runtime applications cannot access upgrade evidence"
    );
    drop(client);
    drop(admin);
    server.stop().expect("clean up the owned PostgreSQL server");
}
