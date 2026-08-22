//! Live PostgreSQL proof for PLAN-2B connection storage.

use tokio_postgres::error::SqlState;
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

fn assert_check_refusal(error: &tokio_postgres::Error, constraint: &str) {
    let database = error
        .as_db_error()
        .expect("statement refusal is a PostgreSQL error");
    assert_eq!(database.code(), &SqlState::CHECK_VIOLATION);
    assert_eq!(database.constraint(), Some(constraint));
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
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $$;",
        )
        .await
        .expect("reset disposable catalog schema");
    client
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install current catalog schema before legacy-shape simulation");
    client
        .batch_execute(
            "DROP TRIGGER connection_bindings_require_requirement \
               ON catalog.connection_bindings; \
             DROP FUNCTION catalog.require_connection_binding_requirement(); \
             DROP INDEX catalog.connection_bindings_component_key; \
             DROP INDEX catalog.connection_bindings_legacy_key; \
             ALTER TABLE catalog.connection_bindings \
               DROP CONSTRAINT connection_bindings_complete_grain, \
               DROP CONSTRAINT connection_bindings_component_digest_check, \
               DROP CONSTRAINT connection_bindings_store_alias_check, \
               DROP COLUMN component_digest, \
               DROP COLUMN store_alias, \
               ALTER COLUMN artifact_hash SET NOT NULL, \
               ALTER COLUMN requirement_name SET NOT NULL; \
             DROP INDEX catalog.connection_requirements_component_key; \
             DROP INDEX catalog.connection_requirements_legacy_key; \
             ALTER TABLE catalog.connection_requirements \
               DROP CONSTRAINT connection_requirements_complete_grain, \
               DROP CONSTRAINT connection_requirements_component_digest_check, \
               DROP CONSTRAINT connection_requirements_store_alias_check, \
               DROP COLUMN component_digest, \
               DROP COLUMN store_alias, \
               ALTER COLUMN artifact_hash SET NOT NULL, \
               ALTER COLUMN requirement_name SET NOT NULL, \
               ADD PRIMARY KEY (tenant_id, artifact_hash, requirement_name); \
             ALTER TABLE catalog.connection_bindings \
               ADD PRIMARY KEY ( \
                 tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name \
               ), \
               ADD FOREIGN KEY (tenant_id, artifact_hash, requirement_name) \
                 REFERENCES catalog.connection_requirements \
                   (tenant_id, artifact_hash, requirement_name); \
             CREATE FUNCTION catalog.require_connection_artifact() \
             RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
               IF NOT EXISTS ( \
                 SELECT 1 FROM catalog.flow_artifacts artifact \
                 WHERE artifact.tenant_id = NEW.tenant_id \
                   AND artifact.artifact_hash = NEW.artifact_hash \
               ) THEN \
                 RAISE EXCEPTION USING ERRCODE = '23503', \
                   MESSAGE = 'connection-requirement-artifact-missing'; \
               END IF; \
               RETURN NEW; \
             END \
             $$; \
             CREATE TRIGGER connection_requirements_require_artifact \
             BEFORE INSERT ON catalog.connection_requirements \
             FOR EACH ROW EXECUTE FUNCTION catalog.require_connection_artifact(); \
             INSERT INTO catalog.flow_artifacts ( \
               tenant_id, flow_id, flow_version, schema_version, graph_json, graph_hash, \
               artifact_hash \
             ) VALUES ( \
               'tenant-a', 'flow-a', 1, '0.1', '{}'::jsonb, 'graph-a', 'artifact-a' \
             ); \
             INSERT INTO catalog.connection_requirements ( \
               tenant_id, artifact_hash, requirement_name, requirement_json, requirement_hash \
             ) VALUES ( \
               'tenant-a', 'artifact-a', 'erp', '{}'::jsonb, 'requirement-a' \
             );",
        )
        .await
        .expect("simulate the truthful legacy connection grain");
    let component_columns_absent: bool = client
        .query_one(
            "SELECT NOT EXISTS ( \
               SELECT 1 FROM information_schema.columns \
               WHERE table_schema = 'catalog' \
                 AND table_name = 'connection_requirements' \
                 AND column_name IN ('component_digest', 'store_alias') \
             )",
            &[],
        )
        .await
        .expect("probe simulated legacy schema")
        .get(0);
    assert!(component_columns_absent);
    ensure_catalog_storage(&client)
        .await
        .expect("migrate legacy rows through the production async installer");
    ensure_catalog_storage(&client)
        .await
        .expect("component grain migration is idempotent");

    let legacy_row = client
        .query_one(
            "SELECT artifact_hash, requirement_name, component_digest, store_alias \
             FROM catalog.connection_requirements \
             WHERE tenant_id = 'tenant-a' AND artifact_hash = 'artifact-a'",
            &[],
        )
        .await
        .expect("read preserved legacy requirement");
    let legacy_coordinates: (String, String, Option<String>, Option<String>) = (
        legacy_row.get(0),
        legacy_row.get(1),
        legacy_row.get(2),
        legacy_row.get(3),
    );
    assert_eq!(legacy_coordinates.0, "artifact-a");
    assert_eq!(legacy_coordinates.1, "erp");
    assert_eq!(legacy_coordinates.2, None);
    assert_eq!(legacy_coordinates.3, None);

    client
        .batch_execute(
            "INSERT INTO catalog.connection_instances ( \
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

    // Four coordinates have sixteen NULL/non-NULL shapes. Exactly the complete
    // legacy pair and the complete component pair are representable.
    for mask in 0_u8..16 {
        if matches!(mask, 0b0011 | 0b1100) {
            continue;
        }
        let artifact_hash = (mask & 0b0001 != 0).then_some("artifact-a");
        let requirement_name = (mask & 0b0010 != 0).then_some("mask-requirement");
        let component_digest = (mask & 0b0100 != 0).then_some("sha256:component-a");
        let store_alias = (mask & 0b1000 != 0).then_some("mask-store");
        let error = client
            .execute(
                "INSERT INTO catalog.connection_requirements ( \
                   tenant_id, artifact_hash, requirement_name, component_digest, store_alias, \
                   requirement_json, requirement_hash \
                 ) VALUES ('tenant-a', $1, $2, $3, $4, '{}'::jsonb, 'invalid')",
                &[
                    &artifact_hash,
                    &requirement_name,
                    &component_digest,
                    &store_alias,
                ],
            )
            .await
            .expect_err("partial and mixed requirement coordinates must fail");
        assert_check_refusal(&error, "connection_requirements_complete_grain");

        let error = client
            .execute(
                "INSERT INTO catalog.connection_bindings ( \
                   tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name, \
                   component_digest, store_alias, environment, instance_id, \
                   validation_status, validation_hash \
                 ) VALUES ( \
                   'tenant-a', 'release', 1, $1, $2, $3, $4, 'dev', 'erp-dev', \
                   'valid', 'invalid' \
                 )",
                &[
                    &artifact_hash,
                    &requirement_name,
                    &component_digest,
                    &store_alias,
                ],
            )
            .await
            .expect_err("partial and mixed binding coordinates must fail");
        assert_check_refusal(&error, "connection_bindings_complete_grain");
    }

    let legacy_cannot_satisfy_component = client
        .execute(
            "INSERT INTO catalog.connection_bindings ( \
               tenant_id, catalog_id, catalog_version, component_digest, store_alias, \
               environment, instance_id, validation_status, validation_hash \
             ) VALUES ( \
               'tenant-a', 'release', 1, 'sha256:component-a', 'erp', \
               'dev', 'erp-dev', 'valid', 'component-validation' \
             )",
            &[],
        )
        .await
        .expect_err("a legacy requirement cannot satisfy a component binding");
    let database = legacy_cannot_satisfy_component
        .as_db_error()
        .expect("missing component requirement is a PostgreSQL error");
    assert_eq!(database.code(), &SqlState::FOREIGN_KEY_VIOLATION);
    assert_eq!(database.message(), "connection-binding-requirement-missing");

    client
        .batch_execute(
            "INSERT INTO catalog.connection_requirements ( \
               tenant_id, component_digest, store_alias, requirement_json, requirement_hash \
             ) VALUES ( \
               'tenant-a', 'sha256:component-a', 'erp', '{}'::jsonb, \
               'component-requirement' \
             ); \
             INSERT INTO catalog.connection_bindings ( \
               tenant_id, catalog_id, catalog_version, component_digest, store_alias, \
               environment, instance_id, validation_status, validation_hash \
             ) VALUES ( \
               'tenant-a', 'release', 1, 'sha256:component-a', 'erp', \
               'dev', 'erp-dev', 'valid', 'component-validation' \
             );",
        )
        .await
        .expect("component requirement and binding use only component coordinates");

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
