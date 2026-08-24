//! Live gate for the wamn-0h0g.12.20-.12.29 confinement of `wamn_app` on the
//! catalog schema of record.
//!
//! Set `WAMN_CTL_PG_URL` to a **superuser** url (path `/postgres`) of a throwaway
//! PostgreSQL 18; skipped cleanly when unset. Everything runs against the REAL
//! `deploy/sql/catalog-schema.sql` through the REAL installer
//! (`publish_catalog::ensure_catalog_storage`), never a paraphrase.
//!
//! Two installation paths reach a production database and BOTH are covered,
//! because covering only the first is inert:
//!
//! * the FRESH install, where `catalog-schema.sql` applies whole; and
//! * the CONVERGE, the only path that reaches a database provisioned by an
//!   earlier revision. `ensure_catalog_storage` re-runs it on every
//!   `migrate-catalog` (`migrate_catalog.rs:141`), so an arm missing there
//!   silently restores whatever the old file granted, and a revoke written only
//!   in the SQL file would never reach an existing database at all.
//!
//! Every statement asserted DENIED is deliberately RLS-LEGAL for the probe
//! tenant. An RLS `WITH CHECK` rejection raises SQLSTATE 42501 exactly as a
//! privilege rejection does, so a probe that fed the policy an illegal row would
//! pass for the wrong reason. The effective privilege set is asserted directly
//! alongside the statement outcomes for the same reason.
//!
//! Hermetic: the preamble drops the `catalog` schema and re-hardens both roles,
//! so no leftover healthy object can satisfy an `IF NOT EXISTS` guard.

mod support;

use tokio_postgres::{Client, NoTls};

use wamn_ctl::publish_catalog::ensure_catalog_storage;

const TENANT: &str = "t1";

/// The ten relations wamn-0h0g.12.20-.12.29 confine, in bead order.
const CONFINED: [&str; 10] = [
    "catalog.catalogs",
    "catalog.schema_migrations",
    "catalog.entities",
    "catalog.fields",
    "catalog.relations",
    "catalog.indexes",
    "catalog.constraints",
    "catalog.rls_policies",
    "catalog.seed_datasets",
    "catalog.event_registrations",
];

/// The blanket grants an already-provisioned database still carries — the exact
/// text `deploy/sql/catalog-schema.sql` shipped before this confinement.
const LEGACY_BLANKET_GRANTS: &str = "\
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.catalogs, catalog.entities, \
  catalog.fields, catalog.relations, catalog.indexes, catalog.constraints, \
  catalog.rls_policies, catalog.seed_datasets, catalog.event_registrations \
  TO wamn_app; \
GRANT SELECT, INSERT ON catalog.schema_migrations TO wamn_app;";

/// One RLS-legal mutation per confined relation: `(relation, insert, update)`.
/// Each INSERT satisfies the tenant `WITH CHECK` and every foreign key, so its
/// only possible refusal is the privilege one.
const LEGAL_MUTATIONS: [(&str, &str, &str); 10] = [
    (
        "catalog.catalogs",
        "INSERT INTO catalog.catalogs \
           (tenant_id, catalog_id, version, environment, schema_version, state) \
         VALUES ('t1', 'c1', 9, 'dev', '0.1', 'draft')",
        "UPDATE catalog.catalogs SET name = 'forged'",
    ),
    (
        "catalog.schema_migrations",
        "INSERT INTO catalog.schema_migrations \
           (tenant_id, catalog_id, environment, to_version, statement_count, checksum) \
         VALUES ('t1', 'c1', 'dev', 9, 0, 'forged')",
        "UPDATE catalog.schema_migrations SET checksum = 'forged'",
    ),
    (
        "catalog.entities",
        "INSERT INTO catalog.entities \
           (tenant_id, catalog_id, catalog_version, entity_id, name) \
         VALUES ('t1', 'c1', 1, 'forged', 'forged')",
        "UPDATE catalog.entities SET label = 'forged'",
    ),
    (
        "catalog.fields",
        "INSERT INTO catalog.fields \
           (tenant_id, catalog_id, catalog_version, entity_id, field_id, ordinal, name, type) \
         VALUES ('t1', 'c1', 1, 'hold', 'forged', 2, 'forged', '{\"kind\":\"text\"}'::jsonb)",
        "UPDATE catalog.fields SET label = 'forged'",
    ),
    (
        "catalog.relations",
        "INSERT INTO catalog.relations \
           (tenant_id, catalog_id, catalog_version, relation_id, name, cardinality, \
            from_entity, to_entity) \
         VALUES ('t1', 'c1', 1, 'forged', 'forged', 'one-to-many', 'hold', 'hold')",
        "UPDATE catalog.relations SET description = 'forged'",
    ),
    (
        "catalog.indexes",
        "INSERT INTO catalog.indexes \
           (tenant_id, catalog_id, catalog_version, entity_id, index_name, fields) \
         VALUES ('t1', 'c1', 1, 'hold', 'forged', ARRAY['note'])",
        "UPDATE catalog.indexes SET is_unique = true",
    ),
    (
        "catalog.constraints",
        "INSERT INTO catalog.constraints \
           (tenant_id, catalog_id, catalog_version, entity_id, constraint_name, kind, expression) \
         VALUES ('t1', 'c1', 1, 'hold', 'forged', 'check', 'true')",
        "UPDATE catalog.constraints SET expression = 'false'",
    ),
    (
        "catalog.rls_policies",
        // The self-referential one: this row is an INPUT to the RLS compiler.
        "INSERT INTO catalog.rls_policies (tenant_id, catalog_id, policy_id, entity_id, rule) \
         VALUES ('t1', 'c1', 'forged', 'hold', '{\"kind\":\"row-ownership\"}'::jsonb)",
        "UPDATE catalog.rls_policies SET rule = '{\"kind\":\"forged\"}'::jsonb",
    ),
    (
        "catalog.seed_datasets",
        "INSERT INTO catalog.seed_datasets (tenant_id, catalog_id, dataset_id, dataset) \
         VALUES ('t1', 'c1', 'forged', '{}'::jsonb)",
        "UPDATE catalog.seed_datasets SET dataset = '{\"forged\":true}'::jsonb",
    ),
    (
        "catalog.event_registrations",
        "INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ('t1', 'c1', 'forged', 'flow-a', 'hold', '{}'::jsonb)",
        "UPDATE catalog.event_registrations SET flow_id = 'forged'",
    ),
];

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the throwaway PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Drop the catalog schema and re-harden both roles, then install through the
/// REAL fresh-install path.
async fn fresh_install(su: &Client) {
    su.batch_execute(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           END IF; \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           ELSE \
             ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
           END IF; \
         END $$; \
         REVOKE wamn_scenario_author FROM wamn_app;",
    )
    .await
    .expect("reset the catalog schema and both roles");
    ensure_catalog_storage(su)
        .await
        .expect("fresh install of the real catalog schema");
}

/// Seed one FK-complete tenant island so every probe statement is RLS-legal and
/// every UPDATE and DELETE has a row to reach.
async fn seed(su: &Client) {
    su.batch_execute(
        "INSERT INTO catalog.catalogs \
           (tenant_id, catalog_id, version, environment, schema_version, state) \
         VALUES ('t1', 'c1', 1, 'dev', '0.1', 'applied'); \
         INSERT INTO catalog.schema_migrations \
           (tenant_id, catalog_id, environment, to_version, statement_count, checksum) \
         VALUES ('t1', 'c1', 'dev', 1, 0, 'seed'); \
         INSERT INTO catalog.entities (tenant_id, catalog_id, catalog_version, entity_id, name) \
         VALUES ('t1', 'c1', 1, 'hold', 'hold'); \
         INSERT INTO catalog.fields \
           (tenant_id, catalog_id, catalog_version, entity_id, field_id, ordinal, name, type) \
         VALUES ('t1', 'c1', 1, 'hold', 'note', 1, 'note', '{\"kind\":\"text\"}'::jsonb); \
         INSERT INTO catalog.relations \
           (tenant_id, catalog_id, catalog_version, relation_id, name, cardinality, \
            from_entity, to_entity) \
         VALUES ('t1', 'c1', 1, 'rel', 'rel', 'one-to-many', 'hold', 'hold'); \
         INSERT INTO catalog.indexes \
           (tenant_id, catalog_id, catalog_version, entity_id, index_name, fields) \
         VALUES ('t1', 'c1', 1, 'hold', 'by_note', ARRAY['note']); \
         INSERT INTO catalog.constraints \
           (tenant_id, catalog_id, catalog_version, entity_id, constraint_name, kind, expression) \
         VALUES ('t1', 'c1', 1, 'hold', 'note_set', 'check', 'true'); \
         INSERT INTO catalog.rls_policies (tenant_id, catalog_id, policy_id, entity_id, rule) \
         VALUES ('t1', 'c1', 'own', 'hold', '{\"kind\":\"row-ownership\"}'::jsonb); \
         INSERT INTO catalog.seed_datasets (tenant_id, catalog_id, dataset_id, dataset) \
         VALUES ('t1', 'c1', 'fixtures', '{}'::jsonb); \
         INSERT INTO catalog.event_registrations \
           (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
         VALUES ('t1', 'c1', 'reg-a', 'flow-a', 'hold', '{}'::jsonb);",
    )
    .await
    .expect("seed one FK-complete tenant island");
}

/// The exact effective privilege set the confinement ratifies, asserted through
/// `has_*_privilege` rather than inferred from the statements.
async fn assert_effective_privileges(su: &Client, stage: &str) {
    for relation in CONFINED {
        let row = su
            .query_one(
                "SELECT has_table_privilege('wamn_app', $1, 'SELECT'), \
                        has_any_column_privilege('wamn_app', $1, 'INSERT'), \
                        has_table_privilege('wamn_app', $1, 'UPDATE,DELETE,TRUNCATE'), \
                        has_any_column_privilege('wamn_scenario_author', $1, \
                          'SELECT,INSERT,UPDATE,REFERENCES')",
                &[&relation],
            )
            .await
            .expect("read the effective privilege set");
        assert!(row.get::<_, bool>(0), "{stage}: {relation} lost SELECT");
        assert!(!row.get::<_, bool>(1), "{stage}: {relation} still INSERTs");
        assert!(
            !row.get::<_, bool>(2),
            "{stage}: {relation} still holds table UPDATE/DELETE/TRUNCATE"
        );
        assert!(
            !row.get::<_, bool>(3),
            "{stage}: {relation} is reachable by wamn_scenario_author"
        );
    }

    // wamn-0h0g.12.29: exactly one updatable column, and it is `tenant_id`.
    let columns = su
        .query(
            "SELECT a.attname, \
                    has_column_privilege('wamn_app', 'catalog.event_registrations', \
                                         a.attname, 'UPDATE') \
             FROM pg_attribute a \
             WHERE a.attrelid = 'catalog.event_registrations'::regclass AND a.attnum > 0 \
             ORDER BY a.attnum",
            &[],
        )
        .await
        .expect("read the registration column grants");
    let updatable: Vec<String> = columns
        .iter()
        .filter(|row| row.get::<_, bool>(1))
        .map(|row| row.get::<_, String>(0))
        .collect();
    assert_eq!(updatable, vec!["tenant_id".to_string()], "{stage}");
}

/// Run one statement as `wamn_app` under the probe tenant in its own aborted
/// transaction, returning the database error.
async fn refused(su: &Client, statement: &str) -> tokio_postgres::error::DbError {
    su.batch_execute("BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant = 't1';")
        .await
        .expect("enter the app-role probe transaction");
    let error = su
        .batch_execute(statement)
        .await
        .expect_err(&format!("must be refused: {statement}"));
    su.batch_execute("ROLLBACK")
        .await
        .expect("leave the probe transaction");
    error
        .as_db_error()
        .expect("a refusal carries a database error")
        .clone()
}

/// Run one statement as `wamn_app` under the probe tenant and commit it.
async fn permitted(su: &Client, statement: &str) {
    su.batch_execute(&format!(
        "BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant = 't1'; {statement}; COMMIT;"
    ))
    .await
    .unwrap_or_else(|error| panic!("must be permitted: {statement}: {error}"));
}

/// Every arm shares the fixed `catalog` schema, so they run SEQUENTIALLY under
/// one test entry — parallel entries would clobber each other's reset.
#[tokio::test]
async fn the_app_login_reads_the_catalog_schema_of_record_and_never_writes_it() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the catalog confinement gate");
        return;
    };
    let su = connect(&url).await;

    // ARM 1 — the FRESH install grants exactly the ratified set.
    fresh_install(&su).await;
    assert_effective_privileges(&su, "fresh install").await;
    seed(&su).await;

    // ARM 2 — every RLS-LEGAL mutation is refused, and refused for the PRIVILEGE
    // reason. `permission denied` and `violates row-level security policy` are
    // both 42501; only the message separates a real confinement from a probe
    // that handed the policy an illegal row.
    for (relation, insert, update) in LEGAL_MUTATIONS {
        for statement in [
            insert,
            update,
            &format!("DELETE FROM {relation} WHERE tenant_id = '{TENANT}'"),
        ] {
            let error = refused(&su, statement).await;
            assert_eq!(error.code().code(), "42501", "{statement}: {error}");
            assert!(
                error.message().starts_with("permission denied"),
                "{statement} was refused by row security, not by privilege: {error}"
            );
        }
        // SELECT is retained: confinement, not blindness.
        permitted(&su, &format!("SELECT count(*) FROM {relation}")).await;
    }

    // ARM 3 — TRAP 2. Callable-flow admission locks the live registration with
    // `FOR KEY SHARE` as `wamn_app` (`run-state/src/admission.rs:384`), and
    // PostgreSQL requires UPDATE on at least one column for ANY row-locking
    // clause. A bare `REVOKE UPDATE` would have made this an outage.
    permitted(
        &su,
        "SELECT registration_id FROM catalog.event_registrations \
         WHERE tenant_id = 't1' AND catalog_id = 'c1' AND registration_id = 'reg-a' \
         FOR KEY SHARE",
    )
    .await;

    // ARM 4 — the column the lock is bought with carries NO rewrite authority.
    // The FORCE-RLS `WITH CHECK` admits only the value already in the row, so
    // the one write it permits is the identity write.
    permitted(
        &su,
        "UPDATE catalog.event_registrations SET tenant_id = tenant_id \
         WHERE registration_id = 'reg-a'",
    )
    .await;
    let rewrite = refused(
        &su,
        "UPDATE catalog.event_registrations SET tenant_id = 't2' \
         WHERE registration_id = 'reg-a'",
    )
    .await;
    assert_eq!(rewrite.code().code(), "42501");
    assert!(
        rewrite.message().contains("row-level security policy"),
        "the tenant rewrite must be refused by the policy, not by privilege: {rewrite}"
    );
    let unchanged: i64 = su
        .query_one(
            "SELECT count(*) FROM catalog.event_registrations \
             WHERE tenant_id = 't1' AND registration_id = 'reg-a' AND flow_id = 'flow-a'",
            &[],
        )
        .await
        .expect("re-read the locked registration")
        .get(0);
    assert_eq!(unchanged, 1);

    // ARM 5 — wamn-0h0g.12.27. `catalog.rls_policies` is the stored form of the
    // RLS rules modelled by `wamn_schema_compiler::rls`
    // (`crates/schema/compiler/src/rls/model.rs`). The copy driver's definition
    // pass, which read these rows and applied the emitted CREATE POLICY DDL, was
    // deleted (`5bb69f0d`); no production shell compiles them today, so this arm
    // asserts the write grant itself rather than a downstream compile. The
    // forged rule is RLS-legal under the probe tenant — that is exactly what
    // makes the grant, not the policy, the boundary.
    let forgery = refused(
        &su,
        "INSERT INTO catalog.rls_policies (tenant_id, catalog_id, policy_id, entity_id, rule) \
         VALUES ('t1', 'c1', 'forged', 'hold', \
                 '{\"kind\":\"row-ownership\",\"entity\":\"hold\",\"field\":\"owner\"}'::jsonb)",
    )
    .await;
    assert_eq!(forgery.code().code(), "42501");
    assert!(
        forgery.message().starts_with("permission denied"),
        "{forgery}"
    );

    // ARM 6 — TRAP 1. A database provisioned by an earlier revision still
    // carries the blanket grants; the converge is the ONLY thing that reaches
    // it. Re-widen, re-run the real installer, and re-assert.
    su.batch_execute(LEGACY_BLANKET_GRANTS)
        .await
        .expect("restore the legacy blanket grants");
    let widened: bool = su
        .query_one(
            "SELECT has_table_privilege('wamn_app', 'catalog.catalogs', 'UPDATE') \
                AND has_table_privilege('wamn_app', 'catalog.event_registrations', 'DELETE')",
            &[],
        )
        .await
        .expect("confirm the legacy state")
        .get(0);
    assert!(widened, "the legacy preamble did not actually re-widen");
    ensure_catalog_storage(&su)
        .await
        .expect("converge an already-provisioned database");
    assert_effective_privileges(&su, "converge").await;

    // ARM 7 — the converge is idempotent and its own effective-ACL assertion is
    // live: an over-grant the REVOKE cannot reach (inherited through a role
    // membership) must make the converge REFUSE rather than pass quietly.
    ensure_catalog_storage(&su)
        .await
        .expect("converge is idempotent");
    su.batch_execute(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_catalog_backdoor') THEN \
             CREATE ROLE wamn_catalog_backdoor NOLOGIN; \
           END IF; \
         END $$; \
         GRANT INSERT ON catalog.rls_policies TO wamn_catalog_backdoor; \
         GRANT wamn_catalog_backdoor TO wamn_app;",
    )
    .await
    .expect("open an inherited over-grant");
    let refusal = ensure_catalog_storage(&su)
        .await
        .expect_err("an inherited over-grant must refuse the converge");
    assert!(
        format!("{refusal:#}").contains("catalog-schema-model-privilege-out-of-bounds"),
        "{refusal:#}"
    );
    su.batch_execute(
        "REVOKE wamn_catalog_backdoor FROM wamn_app; \
         REVOKE INSERT ON catalog.rls_policies FROM wamn_catalog_backdoor; \
         DROP ROLE wamn_catalog_backdoor;",
    )
    .await
    .expect("close the inherited over-grant");
    ensure_catalog_storage(&su)
        .await
        .expect("the converge passes again once the backdoor is closed");
    assert_effective_privileges(&su, "after the backdoor").await;
}
