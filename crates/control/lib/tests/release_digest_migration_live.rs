//! Live test of `project/0012_release_digest.sql` and
//! `system/0019_release_digest.sql` (wamn-snz0.5): on databases shaped as the
//! migrations before them left them, with release rows keyed by the integer
//! release id, the migrations key every release row by its manifest digest as
//! a fresh install has it, and convert the rows rather than drop them. The
//! tests hold the process lock of their server, because the installers create
//! cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const PROJECT_MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/project/0012_release_digest.sql");
const SYSTEM_MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0019_release_digest.sql");
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");

/// The bytes of the one frozen release snapshot, and their digest.
const SNAPSHOT: &str =
    r#"{"release":{"packages":[{"package-id":"widgets","package-version":"1.0.0"}]}}"#;

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns, constraints, indexes, policies, triggers, RLS flags and
/// grants of `tables`, and the definitions of `functions`.
async fn definition(client: &Client, tables: &[&str], functions: &[&str]) -> Vec<String> {
    let tables: Vec<String> = tables.iter().map(|table| (*table).to_owned()).collect();
    let functions: Vec<String> = functions.iter().map(|name| (*name).to_owned()).collect();
    client
        .query(
            "WITH t(oid) AS (SELECT to_regclass(name) FROM unnest($1::text[]) AS name) \
             SELECT entry FROM ( \
               SELECT a.attrelid::regclass::text || ' column ' || a.attname || ' ' \
                      || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text \
                      || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 JOIN t ON t.oid = a.attrelid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT conrelid::regclass::text || ' constraint ' || conname || ' ' \
                      || pg_get_constraintdef(c.oid) \
                 FROM pg_constraint c JOIN t ON t.oid = c.conrelid \
               UNION ALL \
               SELECT 'index ' || pg_get_indexdef(i.indexrelid) \
                 FROM pg_index i JOIN t ON t.oid = i.indrelid \
               UNION ALL \
               SELECT p.polrelid::regclass::text || ' policy ' || p.polname || ' ' \
                      || coalesce(pg_get_expr(p.polqual, p.polrelid), '') || ' ' \
                      || coalesce(pg_get_expr(p.polwithcheck, p.polrelid), '') \
                 FROM pg_policy p JOIN t ON t.oid = p.polrelid \
               UNION ALL \
               SELECT 'trigger ' || pg_get_triggerdef(g.oid) || ' ' || g.tgenabled::text \
                 FROM pg_trigger g JOIN t ON t.oid = g.tgrelid WHERE NOT g.tgisinternal \
               UNION ALL \
               SELECT c.oid::regclass::text || ' table ' || c.relrowsecurity::text || ' ' \
                      || c.relforcerowsecurity::text || ' ' || coalesce(c.relacl::text, '') \
                 FROM pg_class c JOIN t ON t.oid = c.oid \
               UNION ALL \
               SELECT a.attrelid::regclass::text || ' column-acl ' || a.attname || ' ' \
                      || a.attacl::text \
                 FROM pg_attribute a JOIN t ON t.oid = a.attrelid \
                WHERE a.attacl IS NOT NULL AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'function ' || pg_get_functiondef(p.oid) \
                 FROM pg_proc p \
                WHERE p.oid::regproc::text = ANY($2::text[]) \
             ) q ORDER BY entry COLLATE \"C\"",
            &[&tables, &functions],
        )
        .await
        .expect("read the definitions")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

const PROJECT_TABLES: [&str; 5] = [
    "catalog.releases",
    "catalog.effective_release_heads",
    "catalog.connection_bindings",
    "catalog.package_upgrade_qualifications",
    "wamn_run.runs",
];

const PROJECT_FUNCTIONS: [&str; 3] = [
    "catalog.lock_package_coordinate_for_release_membership",
    "catalog.reject_package_migration_after_release_membership",
    "wamn_run.guard_run_admission_pins_immutable",
];

/// The project release tables as `project/0011` left them: releases keyed by
/// the integer id, with the membership, component and snapshot tables, and
/// every pinning row keyed by the id.
const PROJECT_BEFORE_SQL: &str = r"
ALTER TABLE wamn_run.runs DROP CONSTRAINT runs_release_fk;
ALTER TABLE catalog.effective_release_heads DROP CONSTRAINT effective_release_heads_release_fkey;
ALTER TABLE catalog.connection_bindings DROP CONSTRAINT connection_bindings_release_fkey;
ALTER TABLE catalog.package_upgrade_qualifications
    DROP CONSTRAINT package_upgrade_qualifications_predecessor_fkey;
DROP TABLE catalog.releases;

CREATE TABLE catalog.effective_releases (
    tenant_id                   text        NOT NULL CHECK (tenant_id <> ''),
    effective_release_id        int         NOT NULL CHECK (effective_release_id > 0),
    environment                 text        NOT NULL CHECK (environment <> ''),
    verified_publisher_principal text,
    created_at                  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT effective_releases_pkey PRIMARY KEY (tenant_id, effective_release_id),
    CONSTRAINT effective_releases_environment_key
        UNIQUE (tenant_id, effective_release_id, environment)
);
CREATE TABLE catalog.effective_release_packages (
    tenant_id            text NOT NULL,
    effective_release_id int  NOT NULL,
    package_id           text NOT NULL,
    package_version      text NOT NULL,
    CONSTRAINT effective_release_packages_pkey
        PRIMARY KEY (tenant_id, effective_release_id, package_id),
    CONSTRAINT effective_release_packages_exact_pair_key
        UNIQUE (tenant_id, effective_release_id, package_id, package_version),
    CONSTRAINT effective_release_packages_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id)
);
CREATE OR REPLACE FUNCTION catalog.lock_package_coordinate_for_release_membership()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM catalog.packages
     WHERE tenant_id = NEW.tenant_id AND package_id = NEW.package_id
       AND package_version = NEW.package_version FOR UPDATE;
    RETURN NEW;
END
$$;
CREATE TRIGGER effective_release_packages_seal_coordinate
    BEFORE INSERT ON catalog.effective_release_packages
    FOR EACH ROW EXECUTE FUNCTION catalog.lock_package_coordinate_for_release_membership();
CREATE TABLE catalog.release_components (
    tenant_id            text NOT NULL,
    effective_release_id int  NOT NULL,
    package_id           text NOT NULL,
    package_version      text NOT NULL,
    component_digest     text NOT NULL,
    CONSTRAINT release_components_component_membership_fkey
        FOREIGN KEY (tenant_id, effective_release_id, package_id, package_version)
        REFERENCES catalog.effective_release_packages
            (tenant_id, effective_release_id, package_id, package_version)
);
CREATE FUNCTION catalog.guard_release_component_insert()
RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TRIGGER release_components_snapshot_seal
    BEFORE INSERT ON catalog.release_components
    FOR EACH ROW EXECUTE FUNCTION catalog.guard_release_component_insert();
CREATE TABLE catalog.release_manifest_snapshots (
    tenant_id            text  NOT NULL,
    effective_release_id int   NOT NULL,
    manifest_digest      text  NOT NULL,
    canonical_bytes      bytea NOT NULL,
    CONSTRAINT release_manifest_snapshots_pkey PRIMARY KEY (tenant_id, effective_release_id),
    CONSTRAINT release_manifest_snapshots_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id),
    CONSTRAINT release_manifest_snapshots_exact_hash
        CHECK (manifest_digest = 'sha256:' || encode(sha256(canonical_bytes), 'hex'))
);
CREATE OR REPLACE FUNCTION catalog.reject_package_migration_after_release_membership()
RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;

ALTER TABLE catalog.effective_release_heads
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0),
    ADD CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment);
ALTER TABLE catalog.connection_bindings
    DROP CONSTRAINT connection_bindings_pkey,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0),
    ADD CONSTRAINT connection_bindings_pkey
        PRIMARY KEY (tenant_id, effective_release_id, component_digest, store_alias),
    ADD CONSTRAINT connection_bindings_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment);
ALTER TABLE catalog.package_upgrade_qualifications
    ADD COLUMN predecessor_release_id int NOT NULL CHECK (predecessor_release_id > 0),
    ADD CONSTRAINT package_upgrade_qualifications_predecessor_fkey
        FOREIGN KEY (tenant_id, predecessor_release_id)
        REFERENCES catalog.release_manifest_snapshots (tenant_id, effective_release_id);

DROP TRIGGER runs_admission_pins_immutable ON wamn_run.runs;
ALTER TABLE wamn_run.runs
    DROP CONSTRAINT runs_check,
    ADD COLUMN effective_release_id int NOT NULL,
    ADD CONSTRAINT runs_check
        CHECK (package_id <> '' AND effective_release_id > 0 AND environment <> ''),
    ADD CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id);
CREATE INDEX runs_release ON wamn_run.runs (tenant_id, effective_release_id);
CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.effective_release_id IS DISTINCT FROM OLD.effective_release_id THEN
        RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = 'run-admission-pin-immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER runs_admission_pins_immutable
BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment,
                 capture_mode, durability_class, wiring_id, wiring_version,
                 wiring_hash, binding_world_json, manifest_digest, service_principal_id
ON wamn_run.runs
FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable();
REVOKE INSERT (manifest_digest) ON wamn_run.runs FROM wamn_executor_platform;
GRANT UPDATE (manifest_digest) ON wamn_run.runs TO wamn_executor_platform;
";

/// One release with a frozen snapshot, and one without, each pinned.
fn project_rows(snapshot_digest: &str) -> String {
    let hash = format!("sha256:{}", "a".repeat(64));
    format!(
        "SET app.tenant = 't1';
INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256)
VALUES ('t1', 'widgets', '1.0.0', '{hash}');
INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment)
VALUES ('t1', 1, 'dev'), ('t1', 2, 'dev');
INSERT INTO catalog.effective_release_packages
VALUES ('t1', 1, 'widgets', '1.0.0'), ('t1', 2, 'widgets', '1.0.0');
INSERT INTO catalog.release_components VALUES ('t1', 1, 'widgets', '1.0.0', '{hash}');
INSERT INTO catalog.release_manifest_snapshots
VALUES ('t1', 1, '{snapshot_digest}', convert_to('{SNAPSHOT}', 'UTF8'));
INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id)
VALUES ('t1', 'dev', 1);
INSERT INTO catalog.component_digest_owners (tenant_id, component_digest, package_id)
VALUES ('t1', '{hash}', 'widgets');
INSERT INTO catalog.connection_requirements
  (tenant_id, component_digest, store_alias, requirement_json, requirement_hash)
VALUES ('t1', '{hash}', 'db', '{{}}', '{hash}');
INSERT INTO catalog.connection_instances
  (tenant_id, environment, instance_id, requirement_type, contract)
VALUES ('t1', 'dev', 'db-1', 'postgres', 'wamn:postgres');
INSERT INTO catalog.connection_bindings
  (tenant_id, effective_release_id, component_digest, store_alias, environment, instance_id,
   binding_status, validation_status, validation_hash)
VALUES ('t1', 1, '{hash}', 'db', 'dev', 'db-1', 'active', 'valid', '{hash}');
INSERT INTO catalog.package_upgrade_qualifications
  (tenant_id, package_id, candidate_package_version, canonical_bytes, result_sha256,
   predecessor_release_id, predecessor_manifest_digest)
VALUES ('t1', 'widgets', '1.0.0', '\\x7b7d', 'sha256:' || encode(sha256('\\x7b7d'), 'hex'),
        1, '{snapshot_digest}');
INSERT INTO wamn_run.runs
  (tenant_id, run_id, flow_id, flow_version, package_id, effective_release_id, environment,
   wiring_id, wiring_version, status, wiring_hash, binding_world_json)
VALUES ('t1', 'released', 'f', 1, 'widgets', 1, 'dev', NULL, NULL, 'completed', NULL, NULL),
       ('t1', 'unfrozen', 'f', 1, 'widgets', 2, 'dev', NULL, NULL, 'completed', NULL, NULL),
       ('t1', 'candidate', NULL, NULL, 'widgets', 1, 'dev', 'w', 1, 'completed',
        '{hash}', '[]');
RESET app.tenant;"
    )
}

#[tokio::test]
async fn the_project_migration_keys_every_release_row_by_its_digest() {
    let url = locked_database::database(wamn_catalog::test_database::tenant);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles \
                                        WHERE rolname = 'wamn_executor_platform') THEN \
               CREATE ROLE wamn_executor_platform NOLOGIN; \
             END IF; END $$;",
        )
        .await
        .expect("create the executor family role");
    client
        .batch_execute(&format!("{RUN_STATE_SQL}\n{RUN_QUEUE_SQL}"))
        .await
        .expect("install the run plane");
    client
        .batch_execute(
            &wamn_control_provision::sql::grant_executor_platform_surface_sql("wamn_run"),
        )
        .await
        .expect("grant the executor surface");
    // The callable-HTTP family reads the release too. Its role is
    // cluster-wide, so another test may have created it; the migration grants
    // it the read exactly when it exists, as its surface does.
    client
        .batch_execute(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_http_admitter') \
             THEN GRANT SELECT ON catalog.releases TO wamn_http_admitter; END IF; END $$;",
        )
        .await
        .expect("grant the callable-HTTP read");
    let fresh = definition(&client, &PROJECT_TABLES, &PROJECT_FUNCTIONS).await;
    assert!(
        fresh.iter().any(|entry| entry
            == "wamn_run.runs constraint runs_release_fk FOREIGN KEY (tenant_id, manifest_digest) \
                REFERENCES catalog.releases(tenant_id, manifest_digest)"),
        "a fresh install pins a run to its release digest: {fresh:?}"
    );

    client
        .batch_execute(PROJECT_BEFORE_SQL)
        .await
        .expect("make the release tables as project/0011 left them");
    let snapshot_digest: String = client
        .query_one(
            "SELECT 'sha256:' || encode(sha256(convert_to($1, 'UTF8')), 'hex')",
            &[&SNAPSHOT],
        )
        .await
        .expect("hash the snapshot")
        .get(0);
    client
        .batch_execute(&project_rows(&snapshot_digest))
        .await
        .expect("seed release rows keyed by the integer id");
    client
        .batch_execute(&format!("BEGIN; {PROJECT_MIGRATION} COMMIT;"))
        .await
        .expect("apply project/0012");

    assert_eq!(
        definition(&client, &PROJECT_TABLES, &PROJECT_FUNCTIONS).await,
        fresh,
        "the migration keys the release rows as a fresh install has them"
    );
    for table in [
        "effective_releases",
        "effective_release_packages",
        "release_components",
        "release_manifest_snapshots",
    ] {
        let present: bool = client
            .query_one(
                "SELECT to_regclass('catalog.' || $1) IS NOT NULL",
                &[&table],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!present, "catalog.{table} survived the migration");
    }
    client.batch_execute("SET app.tenant = 't1'").await.unwrap();
    let digests: Vec<Option<String>> = client
        .query(
            "SELECT manifest_digest FROM catalog.releases \
             UNION ALL SELECT manifest_digest FROM catalog.effective_release_heads \
             UNION ALL SELECT manifest_digest FROM catalog.connection_bindings \
             UNION ALL SELECT predecessor_manifest_digest \
                         FROM catalog.package_upgrade_qualifications \
             UNION ALL (SELECT manifest_digest FROM wamn_run.runs ORDER BY run_id)",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    let digest = Some(snapshot_digest);
    assert_eq!(
        digests,
        vec![
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            None,
            digest,
            None
        ],
        "each pinning row takes the digest of its release's snapshot; a candidate and a run \
         of a release with no snapshot keep no pin"
    );
}

const SYSTEM_TABLES: [&str; 5] = [
    "catalog.effective_releases",
    "catalog.effective_release_packages",
    "catalog.effective_release_heads",
    "catalog.deployment_attestations",
    "catalog.release_selections",
];

/// The control release tables as `system/0018` left them.
const SYSTEM_BEFORE_SQL: &str = r"
ALTER TABLE catalog.release_selections
    DROP CONSTRAINT release_selections_release_fkey,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0);
ALTER TABLE catalog.deployment_attestations
    DROP CONSTRAINT deployment_attestations_release_fkey,
    DROP CONSTRAINT deployment_attestations_coordinate,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0),
    ADD CONSTRAINT deployment_attestations_coordinate UNIQUE (
        tenant_id, environment_instance, effective_release_id, org_id, project_id, environment);
ALTER TABLE catalog.effective_release_heads
    DROP CONSTRAINT effective_release_heads_release_fkey,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0);
ALTER TABLE catalog.effective_release_packages
    DROP CONSTRAINT effective_release_packages_release_fkey,
    DROP CONSTRAINT effective_release_packages_pkey,
    DROP CONSTRAINT effective_release_packages_exact_pair_key,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0),
    ADD CONSTRAINT effective_release_packages_pkey
        PRIMARY KEY (tenant_id, effective_release_id, package_id),
    ADD CONSTRAINT effective_release_packages_exact_pair_key
        UNIQUE (tenant_id, effective_release_id, package_id, package_version);
ALTER TABLE catalog.effective_releases
    DROP CONSTRAINT effective_releases_pkey,
    DROP CONSTRAINT effective_releases_environment_key,
    DROP COLUMN manifest_digest,
    ADD COLUMN effective_release_id int NOT NULL CHECK (effective_release_id > 0),
    ADD CONSTRAINT effective_releases_pkey PRIMARY KEY (tenant_id, effective_release_id),
    ADD CONSTRAINT effective_releases_environment_key
        UNIQUE (tenant_id, effective_release_id, environment);
ALTER TABLE catalog.effective_release_packages
    ADD CONSTRAINT effective_release_packages_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id);
ALTER TABLE catalog.effective_release_heads
    ADD CONSTRAINT effective_release_heads_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment);
ALTER TABLE catalog.deployment_attestations
    ADD CONSTRAINT deployment_attestations_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment);
ALTER TABLE catalog.release_selections
    ADD CONSTRAINT release_selections_release_fkey
        FOREIGN KEY (tenant_id, effective_release_id, environment)
        REFERENCES catalog.effective_releases (tenant_id, effective_release_id, environment);
";

/// One release, attested once, with its membership, head and selection.
fn system_rows(digest: &str) -> String {
    format!(
        "SET app.tenant = 't1';
INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256)
VALUES ('t1', 'widgets', '1.0.0', 'sha256:' || repeat('a', 64));
INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment)
VALUES ('t1', 1, 'dev');
INSERT INTO catalog.effective_release_packages
  (tenant_id, effective_release_id, package_id, package_version)
VALUES ('t1', 1, 'widgets', '1.0.0');
INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id)
VALUES ('t1', 'dev', 1);
INSERT INTO catalog.deployment_attestations
  (tenant_id, environment_instance, effective_release_id, org_id, project_id, environment,
   deployed_manifest_hash, attested_at)
VALUES ('t1', '', 1, 'acme', 'widgets', 'dev', '{digest}', now());
INSERT INTO catalog.release_selections (tenant_id, environment, effective_release_id, reason)
VALUES ('t1', 'dev', 1, 'environment-creation');
RESET app.tenant;"
    )
}

#[tokio::test]
async fn the_system_migration_keys_every_release_row_by_its_attested_digest() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    let fresh = definition(&client, &SYSTEM_TABLES, &[]).await;
    assert!(
        fresh.iter().any(|entry| entry
            == "catalog.effective_releases constraint effective_releases_pkey \
                PRIMARY KEY (tenant_id, manifest_digest)"),
        "a fresh install keys a release by its digest: {fresh:?}"
    );

    client
        .batch_execute(SYSTEM_BEFORE_SQL)
        .await
        .expect("make the release tables as system/0018 left them");
    let digest = format!("sha256:{}", "d".repeat(64));
    client
        .batch_execute(&system_rows(&digest))
        .await
        .expect("seed release rows keyed by the integer id");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {SYSTEM_MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0019 as wamn_system");

    assert_eq!(
        definition(&client, &SYSTEM_TABLES, &[]).await,
        fresh,
        "the migration keys the release rows as a fresh install has them"
    );
    client.batch_execute("SET app.tenant = 't1'").await.unwrap();
    let digests: Vec<String> = client
        .query(
            "SELECT manifest_digest FROM catalog.effective_releases \
             UNION ALL SELECT manifest_digest FROM catalog.effective_release_packages \
             UNION ALL SELECT manifest_digest FROM catalog.effective_release_heads \
             UNION ALL SELECT manifest_digest FROM catalog.deployment_attestations \
             UNION ALL SELECT manifest_digest FROM catalog.release_selections",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        digests,
        vec![digest; 5],
        "every row takes its attested digest"
    );
}

#[tokio::test]
async fn the_system_migration_refuses_a_release_it_cannot_name() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    client
        .batch_execute(SYSTEM_BEFORE_SQL)
        .await
        .expect("make the release tables as system/0018 left them");
    client
        .batch_execute(
            "SET app.tenant = 't1'; \
             INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
             VALUES ('t1', 1, 'dev'); RESET app.tenant;",
        )
        .await
        .expect("seed an unattested release");
    let error = client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {SYSTEM_MIGRATION} COMMIT;"
        ))
        .await
        .expect_err("an unattested release has no digest to take");
    assert_eq!(
        error
            .as_db_error()
            .map(tokio_postgres::error::DbError::message),
        Some("release-digest-migration-unconvertible")
    );
}
