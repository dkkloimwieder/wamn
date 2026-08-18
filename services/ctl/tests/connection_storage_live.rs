//! Live PostgreSQL proof for PLAN-2B connection storage.

use tokio_postgres::{Client, NoTls};
use wamn_ctl::publish_catalog::ensure_catalog_storage;

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

fn database_message(error: &tokio_postgres::Error) -> &str {
    error
        .as_db_error()
        .expect("statement refusal is a PostgreSQL error")
        .message()
}

#[tokio::test]
async fn connection_storage_enforces_environment_and_immutability_boundaries_live() {
    let Some(url) = std::env::var("WAMN_CONNECTION_STORAGE_PG_URL").ok() else {
        eprintln!(
            "WAMN_CONNECTION_STORAGE_PG_URL unset — skipping the connection-storage live gate"
        );
        return;
    };
    let client = connect(&url).await;
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
             END $$;",
        )
        .await
        .expect("reset disposable catalog schema");
    let migration_start = CATALOG_SCHEMA
        .find("-- BEGIN CONNECTION STORAGE MIGRATION")
        .expect("connection migration start");
    client
        .batch_execute(&CATALOG_SCHEMA[..migration_start])
        .await
        .expect("install pre-connection catalog schema");
    let absent_before_upgrade: bool = client
        .query_one(
            "SELECT to_regclass('catalog.connection_instances') IS NULL",
            &[],
        )
        .await
        .expect("probe pre-upgrade schema")
        .get(0);
    assert!(absent_before_upgrade);
    ensure_catalog_storage(&client)
        .await
        .expect("upgrade through the production async installer");
    ensure_catalog_storage(&client)
        .await
        .expect("connection storage upgrade is idempotent");

    client
        .batch_execute(
            "INSERT INTO catalog.flow_artifacts ( \
               tenant_id, flow_id, flow_version, schema_version, graph_json, graph_hash, \
               artifact_hash \
             ) VALUES ( \
               'tenant-a', 'flow-a', 1, '0.1', '{}'::jsonb, 'graph-a', \
               'artifact-a' \
             ); \
             INSERT INTO catalog.connection_requirements ( \
               tenant_id, artifact_hash, requirement_name, requirement_json, requirement_hash \
             ) VALUES ( \
               'tenant-a', 'artifact-a', 'erp', \
               '{}'::jsonb, \
               'requirement-a' \
             ); \
             INSERT INTO catalog.connection_instances ( \
               tenant_id, environment, instance_id, requirement_type, contract \
             ) VALUES \
               ('tenant-a', 'dev', 'erp-dev', 'http', 'wamn:connection/http@0.1.0'), \
               ('tenant-a', 'prod', 'erp-prod', 'http', 'wamn:connection/http@0.1.0'); \
             INSERT INTO catalog.connection_generations ( \
               tenant_id, environment, instance_id, generation, definition_json, \
               definition_hash, credential_set_handle \
             ) VALUES \
               ('tenant-a', 'dev', 'erp-dev', 1, \
                '{\"primary-authority\":\"https://dev.example\"}'::jsonb, \
                'definition-dev', 'credential-dev'), \
               ('tenant-a', 'prod', 'erp-prod', 1, \
                '{\"primary-authority\":\"https://prod.example\"}'::jsonb, \
                'definition-prod', 'credential-prod'); \
             INSERT INTO catalog.catalogs ( \
               tenant_id, catalog_id, version, environment, schema_version, state \
             ) VALUES \
               ('tenant-a', 'release', 1, 'dev', '0.1', 'applied'), \
               ('tenant-a', 'release', 2, 'prod', '0.1', 'applied'); \
             INSERT INTO catalog.execution_bundles ( \
               tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length \
             ) VALUES ( \
               'tenant-a', \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
               '0.1', decode('7b7d', 'hex'), 2 \
             ); \
             BEGIN; \
             INSERT INTO catalog.release_manifests ( \
               tenant_id, catalog_id, catalog_version \
             ) VALUES \
               ('tenant-a', 'release', 1), \
               ('tenant-a', 'release', 2); \
             INSERT INTO catalog.release_flows ( \
               tenant_id, catalog_id, catalog_version, flow_id, flow_version, \
               execution_bundle_hash \
             ) VALUES \
               ('tenant-a', 'release', 1, 'flow-a', 1, \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'), \
               ('tenant-a', 'release', 2, 'flow-a', 1, \
                'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a'); \
             COMMIT; \
             INSERT INTO catalog.connection_bindings ( \
               tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name, \
               environment, instance_id, validation_status, validation_hash \
             ) VALUES \
               ('tenant-a', 'release', 1, 'artifact-a', 'erp', \
                'dev', 'erp-dev', 'valid', 'validation-dev'), \
               ('tenant-a', 'release', 2, 'artifact-a', 'erp', \
                'prod', 'erp-prod', 'valid', 'validation-prod'); \
             INSERT INTO catalog.connection_generation_retention ( \
               tenant_id, environment, instance_id, generation, reference_kind, reference_id \
             ) VALUES ( \
               'tenant-a', 'dev', 'erp-dev', 1, 'active-attempt', 'attempt-a' \
             );",
        )
        .await
        .expect("seed portable requirement and distinct environment bindings");

    let bindings: Vec<(String, String)> = client
        .query(
            "SELECT environment, instance_id FROM catalog.connection_bindings \
             WHERE tenant_id = 'tenant-a' AND artifact_hash = 'artifact-a' \
             ORDER BY environment",
            &[],
        )
        .await
        .expect("read bindings")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        bindings,
        [
            ("dev".into(), "erp-dev".into()),
            ("prod".into(), "erp-prod".into())
        ]
    );

    let generation_update = client
        .execute(
            "UPDATE catalog.connection_generations SET definition_hash = 'mutated' \
             WHERE tenant_id = 'tenant-a' AND environment = 'dev' \
               AND instance_id = 'erp-dev' AND generation = 1",
            &[],
        )
        .await
        .expect_err("immutable generation update must fail");
    assert!(database_message(&generation_update).contains("immutable"));

    let uncontrolled = client
        .execute(
            "UPDATE catalog.connection_instances SET lifecycle_status = 'disabled' \
             WHERE tenant_id = 'tenant-a' AND environment = 'dev' AND instance_id = 'erp-dev'",
            &[],
        )
        .await
        .expect_err("instance lifecycle update without revision must fail");
    assert!(database_message(&uncontrolled).contains("uncontrolled-update"));
    client
        .execute(
            "UPDATE catalog.connection_instances \
             SET lifecycle_status = 'disabled', revision = revision + 1, \
                 updated_at = updated_at + interval '1 microsecond' \
             WHERE tenant_id = 'tenant-a' AND environment = 'dev' AND instance_id = 'erp-dev'",
            &[],
        )
        .await
        .expect("controlled lifecycle update advances the revision");

    let retained_delete = client
        .execute(
            "DELETE FROM catalog.connection_generations \
             WHERE tenant_id = 'tenant-a' AND environment = 'dev' \
               AND instance_id = 'erp-dev' AND generation = 1",
            &[],
        )
        .await
        .expect_err("referenced generation deletion must fail");
    assert!(database_message(&retained_delete).contains("connection-generation-retained"));

    client
        .batch_execute(
            "BEGIN; SET LOCAL ROLE wamn_app; \
             SELECT set_config('app.tenant', 'other-tenant', true);",
        )
        .await
        .expect("enter tenant-scoped reader transaction");
    let visible: i64 = client
        .query_one("SELECT count(*) FROM catalog.connection_instances", &[])
        .await
        .expect("read through forced RLS")
        .get(0);
    assert_eq!(visible, 0, "forced RLS hides another tenant's connections");
    client
        .batch_execute("ROLLBACK")
        .await
        .expect("leave RLS proof");

    client
        .batch_execute("DROP SCHEMA catalog CASCADE")
        .await
        .expect("zero-residue teardown");
}
