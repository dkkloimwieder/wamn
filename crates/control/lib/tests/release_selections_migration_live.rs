//! Live test of `system/0011_release_selections.sql` (wamn-zua8.3): on a
//! control database installed before it, the migration creates
//! `catalog.qualifications` and `catalog.release_selections` as a fresh
//! install has them. The test holds the process lock of its server, because
//! the installer creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0011_release_selections.sql");
/// `system/0019` keys the selection by the release digest (wamn-snz0.5).
const DIGEST_MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0019_release_digest.sql");

/// The release tables that 0011 refers to, keyed by the integer id as 0010
/// left them.
const RELEASES_BY_ID_SQL: &str = "
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
";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns, constraints, policies, triggers, RLS flags, owner and grants
/// of the two tables.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "WITH t(oid) AS (VALUES ('catalog.qualifications'::regclass), \
                                    ('catalog.release_selections'::regclass)) \
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
               SELECT p.polrelid::regclass::text || ' policy ' || p.polname || ' ' \
                      || coalesce(pg_get_expr(p.polqual, p.polrelid), '') || ' ' \
                      || coalesce(pg_get_expr(p.polwithcheck, p.polrelid), '') \
                 FROM pg_policy p JOIN t ON t.oid = p.polrelid \
               UNION ALL \
               SELECT 'trigger ' || pg_get_triggerdef(g.oid) \
                 FROM pg_trigger g JOIN t ON t.oid = g.tgrelid WHERE NOT g.tgisinternal \
               UNION ALL \
               SELECT c.oid::regclass::text || ' table ' || c.relrowsecurity::text || ' ' \
                      || c.relforcerowsecurity::text || ' ' || pg_get_userbyid(c.relowner) \
                      || ' ' || coalesce(c.relacl::text, '') \
                 FROM pg_class c JOIN t ON t.oid = c.oid \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of the two tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_creates_the_selection_tables_as_a_fresh_install_has_them() {
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
    let fresh = definition(&client).await;
    assert!(
        fresh.iter().any(|entry| entry
            == "catalog.release_selections constraint release_selections_qualification_fkey \
                FOREIGN KEY (qualification_sha256) \
                REFERENCES catalog.qualifications(qualification_sha256)"),
        "a fresh install has the tables: {fresh:?}"
    );
    // Only a selection by the create-environment saga names no qualification.
    assert!(
        fresh.iter().any(|entry| entry
            == "catalog.release_selections constraint release_selections_qualification_check \
                CHECK (((qualification_sha256 IS NULL) = (reason = 'environment-creation'::text)))"),
        "a fresh install ties a null qualification to environment creation: {fresh:?}"
    );

    client
        .batch_execute(&format!(
            "DROP TABLE catalog.release_selections, catalog.qualifications; {RELEASES_BY_ID_SQL}"
        ))
        .await
        .expect("make the catalog as 0010 left it");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} {DIGEST_MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0011 and 0019 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration creates the tables as a fresh install has them"
    );
}
