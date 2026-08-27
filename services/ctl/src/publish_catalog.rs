//! Publishes a project's canonical applied catalog snapshot into the `wamn_catalog` table.
//!
//! This reusable, idempotent host subcommand writes or replaces the canonical
//! snapshot while publishing an applied release. It reads a catalog JSON,
//! `Catalog::to_json`s the canonical document, and UPSERTs it under the project's
//! tenant — connecting as a **superuser** so it bypasses the snapshot table's RLS
//! `WITH CHECK` and the runtime role's SELECT-only grant.
//!
//! `--provision` additionally stands up the schema and the 3.2 tenant floor (the
//! entity tables) when they are absent — used by the in-cluster catalog-publication
//! gate to exercise real data (the demo-row seeding rides the gates-side
//! `wamn-gates publish-catalog --seed` wrapper; the prod tool carries no fixture
//! content). Everything is **additive**: the schema is created `IF NOT EXISTS`,
//! the floor is applied only when missing, and no existing object is ever dropped
//! or altered (the shared-cluster guardrail).
//!
//! POC-F1 extended this into the one project-provisioning tool: `--runstate`
//! applies the run-state storage (`deploy/sql/run-state.sql`: runs)
//! into the project schema — the canonical deploy file, embedded at compile time
//! and rewritten from `wamn_run` to the target schema — when its tables are
//! absent (the authoring-test orchestration tables are the run-plane
//! reconciler's, wamn-0h0g.15.170);
//! and `--seed-dataset` compiles a wamn-schema-compiler (3.6) dataset against the
//! catalog and applies it (deterministic ids, `ON CONFLICT DO NOTHING` —
//! idempotent).

use anyhow::Context as _;

// The canonical `wamn_run` → project-schema deploy-DDL rewrite and its
// lowercase bare-schema type share one owner in the reconcile-run-plane
// planner's crate.
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The one `catalog`-schema relation only the CONTROL plane carries.
///
/// `deploy/sql/control-portable-store.sql` installs it in the same schema, in
/// the same apply, as the `catalog.catalogs` the baseline probe below reads, and
/// no project installer ever creates it — a project install DROPs `catalog` and
/// rebuilds it from `deploy/sql/catalog-schema.sql`, which has no such table.
/// `protected_relations_live` already names it control-only for that reason.
const CONTROL_STORE_WITNESS: &str = "catalog.authoring_command_audit";

/// Stable marker for the two-plane residency refusal (wamn-0h0g.12.180).
pub const CATALOG_PLANE_RESIDENCY_REFUSAL: &str = "catalog-plane-residency-control-store";

/// Refuse a CONTROL-plane database before anything is created, granted or revoked.
///
/// The baseline probe in [`ensure_catalog_storage`] is keyed on the NAME
/// `catalog.catalogs`, and that name lives in BOTH planes — it is one of the
/// tree's own shared portable relations. Pointed at a control store the name-only
/// probe therefore answers "baseline present" and the converge arm runs, so the
/// project-only connection, wiring and release-component storage would be
/// installed into the control plane. A probe keyed on a shared name cannot tell
/// the planes apart; ask the SERVER for a fact only one plane carries instead.
///
/// This sits AHEAD of `ensure_wamn_app_role` on purpose: that call is the first
/// side effect of the wrong-plane path and it mutates CLUSTER-GLOBAL role state
/// (it may `CREATE ROLE wamn_app` with a known password and always issues
/// `REVOKE wamn_scenario_author FROM wamn_app`), which a refusal raised after it
/// could not take back. Note that `promote` and `migrate-catalog` each call
/// `ensure_wamn_app_role` themselves one line ahead of this verb, so on those two
/// paths the role bootstrap still precedes the refusal; only the storage is
/// guarded there.
async fn refuse_control_plane_residency(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    let control_store: bool = client
        .query_one(
            "SELECT to_regclass($1) IS NOT NULL",
            &[&CONTROL_STORE_WITNESS],
        )
        .await
        .context("probe the catalog schema's plane")?
        .get(0);
    anyhow::ensure!(
        !control_store,
        "{CATALOG_PLANE_RESIDENCY_REFUSAL}: {CONTROL_STORE_WITNESS} is present, so this \
         `catalog` schema is the control portable store rather than a project catalog; \
         project catalog storage must not be installed into the control plane"
    );
    Ok(())
}

/// Install or additively upgrade the catalog persistence schema.
///
/// Refuses a control-plane database first — see [`refuse_control_plane_residency`].
pub async fn ensure_catalog_storage(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    refuse_control_plane_residency(client).await?;
    ensure_wamn_app_role(client).await?;
    let baseline_present: bool = client
        .query_one("SELECT to_regclass('catalog.catalogs') IS NOT NULL", &[])
        .await?
        .get(0);
    if !baseline_present {
        let catalog_schema_present: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'catalog')",
                &[],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            !catalog_schema_present,
            "catalog schema exists without catalog.catalogs; reconcile it before publication"
        );
        client
            .batch_execute(CATALOG_SCHEMA_SQL)
            .await
            .context("apply catalog storage")?;
        return Ok(());
    }

    let release_row = client
        .query_one(
            "SELECT to_regclass('catalog.releases') IS NOT NULL, \
                    to_regclass('catalog.catalog_heads') IS NOT NULL, \
                    EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'releases' \
                               AND column_name = 'verified_publisher_principal'), \
                    EXISTS (SELECT 1 FROM pg_constraint con \
                             JOIN pg_class rel ON rel.oid = con.conrelid \
                             JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                             WHERE ns.nspname = 'catalog' \
                               AND rel.relname = 'releases' \
                               AND con.conname = 'releases_verified_publisher_principal_check' \
                               AND pg_get_constraintdef(con.oid, true) = \
                                   'CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''''::text)'), \
                    to_regclass('catalog.connection_requirements') IS NOT NULL, \
                    to_regclass('catalog.connection_instances') IS NOT NULL, \
                    to_regclass('catalog.connection_generations') IS NOT NULL, \
                    to_regclass('catalog.connection_bindings') IS NOT NULL, \
                    to_regclass('catalog.connection_generation_retention') IS NOT NULL, \
                    to_regclass('catalog.wirings') IS NOT NULL, \
                    to_regclass('catalog.wiring_tombstones') IS NOT NULL, \
                    to_regclass('catalog.wiring_activation') IS NOT NULL, \
                    to_regclass('catalog.wiring_activation_events') IS NOT NULL, \
                    to_regclass('catalog.component_library') IS NOT NULL, \
                    to_regclass('catalog.release_components') IS NOT NULL, \
                    to_regclass('catalog.release_manifest_v2_snapshots') IS NOT NULL",
            &[],
        )
        .await?;
    let release_objects = [
        release_row.get::<_, bool>(0),
        release_row.get::<_, bool>(1),
    ];
    if release_objects.iter().all(|present| *present) {
        let provenance_storage = [
            release_row.get::<_, bool>(2),
            release_row.get::<_, bool>(3),
        ];
        if !provenance_storage.iter().all(|present| *present) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN DISPOSITION PROVENANCE STORAGE MIGRATION")
                .expect("disposition provenance migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END DISPOSITION PROVENANCE STORAGE MIGRATION")
                .expect("disposition provenance migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install verified publication provenance storage")?;
        }
        let connection_objects = [
            release_row.get::<_, bool>(4),
            release_row.get::<_, bool>(5),
            release_row.get::<_, bool>(6),
            release_row.get::<_, bool>(7),
            release_row.get::<_, bool>(8),
        ];
        if !connection_objects.iter().all(|present| *present) {
            anyhow::ensure!(
                connection_objects.iter().all(|present| !*present),
                "catalog connection storage is partially installed; reconcile it before publication"
            );
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN CONNECTION STORAGE MIGRATION")
                .expect("connection storage migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END CONNECTION STORAGE MIGRATION")
                .expect("connection storage migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install connection storage")?;
        }
        ensure_connection_component_grain(client).await?;

        // wamn-0h0g.18.2: the wiring relations, their activation guard and the
        // activation doorbell reach an EXISTING project database only here —
        // `catalog-schema.sql` applies whole on a fresh install and never again.
        // All four or none: the slice is one CREATE TABLE run, so a partial
        // install is a reconcile, not something to re-execute over.
        let wiring_objects = [
            release_row.get::<_, bool>(9),
            release_row.get::<_, bool>(10),
            release_row.get::<_, bool>(11),
            release_row.get::<_, bool>(12),
        ];
        if !wiring_objects.iter().all(|present| *present) {
            anyhow::ensure!(
                wiring_objects.iter().all(|present| !*present),
                "catalog wiring storage is partially installed; reconcile it before publication"
            );
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN WIRING STORAGE MIGRATION")
                .expect("wiring storage migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END WIRING STORAGE MIGRATION")
                .expect("wiring storage migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install wiring storage")?;
        }

        if !release_row.get::<_, bool>(13) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN COMPONENT LIBRARY STORAGE MIGRATION")
                .expect("component library migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END COMPONENT LIBRARY STORAGE MIGRATION")
                .expect("component library migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install component library storage")?;
        }
        ensure_component_library_effects(client).await?;

        let release_component_objects = [
            release_row.get::<_, bool>(14),
            release_row.get::<_, bool>(15),
        ];
        if !release_component_objects.iter().all(|present| *present) {
            anyhow::ensure!(
                release_component_objects.iter().all(|present| !*present),
                "catalog release component storage is partially installed; reconcile it before publication"
            );
            install_catalog_migration_if_missing(
                client,
                "catalog.release_components",
                "-- BEGIN RELEASE COMPONENT MEMBERSHIP MIGRATION",
                "-- END RELEASE COMPONENT MEMBERSHIP MIGRATION",
                "install release component membership",
            )
            .await?;
        }

        ensure_authoring_catalog_privileges(client).await?;
        return Ok(());
    }
    anyhow::ensure!(
        release_objects.iter().all(|present| !*present),
        "catalog release storage is partially installed; reconcile it before publication"
    );
    let start = CATALOG_SCHEMA_SQL
        .find("CREATE FUNCTION catalog.reject_immutable_row_change()")
        .expect("catalog release section start");
    let end = CATALOG_SCHEMA_SQL
        .find("-- Migration history (2.5")
        .expect("catalog release section end");
    client
        .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
        .await
        .context("install catalog release storage into baseline catalog")?;
    ensure_connection_component_grain(client).await?;
    for (relation, begin, end, context) in [
        (
            "catalog.component_library",
            "-- BEGIN COMPONENT LIBRARY STORAGE MIGRATION",
            "-- END COMPONENT LIBRARY STORAGE MIGRATION",
            "install component library storage",
        ),
        (
            "catalog.wirings",
            "-- BEGIN WIRING STORAGE MIGRATION",
            "-- END WIRING STORAGE MIGRATION",
            "install wiring storage",
        ),
        (
            "catalog.release_components",
            "-- BEGIN RELEASE COMPONENT MEMBERSHIP MIGRATION",
            "-- END RELEASE COMPONENT MEMBERSHIP MIGRATION",
            "install release component membership",
        ),
    ] {
        install_catalog_migration_if_missing(client, relation, begin, end, context).await?;
    }
    ensure_component_library_effects(client).await?;
    ensure_authoring_catalog_privileges(client).await?;
    Ok(())
}

/// Apply one delimited additive migration when its anchor relation is absent.
async fn install_catalog_migration_if_missing(
    client: &tokio_postgres::Client,
    relation: &str,
    begin: &str,
    end: &str,
    context: &'static str,
) -> anyhow::Result<()> {
    let installed: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&relation])
        .await?
        .get(0);
    if installed {
        return Ok(());
    }
    let start = CATALOG_SCHEMA_SQL
        .find(begin)
        .expect("catalog migration start marker");
    let end = CATALOG_SCHEMA_SQL
        .find(end)
        .expect("catalog migration end marker");
    client
        .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
        .await
        .context(context)
}

/// Converge the derived effect projection onto an existing component library.
async fn ensure_component_library_effects(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    let present: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'component_library' \
                               AND column_name = 'effects')",
            &[],
        )
        .await?
        .get(0);
    if present {
        return Ok(());
    }

    let start = CATALOG_SCHEMA_SQL
        .find("-- BEGIN COMPONENT LIBRARY EFFECTS MIGRATION")
        .expect("component library effects migration start");
    let end = CATALOG_SCHEMA_SQL
        .find("-- END COMPONENT LIBRARY EFFECTS MIGRATION")
        .expect("component library effects migration end");
    client
        .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
        .await
        .context("install component library effect projection")
}

/// Preserve legacy connection facts while converging the component-owned grain.
async fn ensure_connection_component_grain(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    let complete: bool = client
        .query_one(
            "SELECT \
               EXISTS (SELECT 1 FROM information_schema.columns \
                       WHERE table_schema = 'catalog' \
                         AND table_name = 'connection_requirements' \
                         AND column_name = 'component_digest') \
               AND EXISTS (SELECT 1 FROM information_schema.columns \
                           WHERE table_schema = 'catalog' \
                             AND table_name = 'connection_requirements' \
                             AND column_name = 'store_alias') \
               AND EXISTS (SELECT 1 FROM information_schema.columns \
                           WHERE table_schema = 'catalog' \
                             AND table_name = 'connection_bindings' \
                             AND column_name = 'component_digest') \
               AND EXISTS (SELECT 1 FROM information_schema.columns \
                           WHERE table_schema = 'catalog' \
                             AND table_name = 'connection_bindings' \
                             AND column_name = 'store_alias') \
               AND EXISTS (SELECT 1 FROM pg_constraint con \
                           JOIN pg_class rel ON rel.oid = con.conrelid \
                           JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                           WHERE ns.nspname = 'catalog' \
                             AND rel.relname = 'connection_requirements' \
                             AND con.conname = 'connection_requirements_complete_grain') \
               AND EXISTS (SELECT 1 FROM pg_constraint con \
                           JOIN pg_class rel ON rel.oid = con.conrelid \
                           JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                           WHERE ns.nspname = 'catalog' \
                             AND rel.relname = 'connection_bindings' \
                             AND con.conname = 'connection_bindings_complete_grain') \
               AND to_regclass('catalog.connection_requirements_component_key') IS NOT NULL \
               AND to_regclass('catalog.connection_bindings_component_key') IS NOT NULL \
               AND EXISTS (SELECT 1 FROM pg_trigger trigger \
                           WHERE trigger.tgrelid = 'catalog.connection_bindings'::regclass \
                             AND trigger.tgname = 'connection_bindings_require_requirement' \
                             AND NOT trigger.tgisinternal) \
               AND NOT EXISTS (SELECT 1 FROM pg_trigger trigger \
                               WHERE trigger.tgrelid = 'catalog.connection_requirements'::regclass \
                                 AND trigger.tgname = 'connection_requirements_require_artifact' \
                                 AND NOT trigger.tgisinternal)",
            &[],
        )
        .await?
        .get(0);
    if complete {
        return Ok(());
    }

    let start = CATALOG_SCHEMA_SQL
        .find("-- BEGIN CONNECTION COMPONENT GRAIN MIGRATION")
        .expect("connection component grain migration start");
    let end = CATALOG_SCHEMA_SQL
        .find("-- END CONNECTION COMPONENT GRAIN MIGRATION")
        .expect("connection component grain migration end");
    client
        .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
        .await
        .context("migrate connection storage to the component grain")
}

/// Converge the catalog schema's role grants on an ALREADY-PROVISIONED database.
///
/// `deploy/sql/catalog-schema.sql` applies whole only on a fresh install, so this
/// is the only path that reaches a database provisioned by an earlier revision —
/// and it is re-run on every publish, which means an arm missing here silently
/// restores whatever the old file granted. Two boundaries live in one batch: the
/// authoring split between `wamn_app` and `wamn_scenario_author`, and the
/// wamn-0h0g.12.20-.12.29 confinement that leaves the guest-reachable `wamn_app`
/// LOGIN read-only on the schema-of-record relations no production writer
/// reaches through it. Each half asserts its own effective ACL before returning.
async fn ensure_authoring_catalog_privileges(
    client: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    client
        .batch_execute(
            "REVOKE wamn_scenario_author FROM wamn_app; \
             GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.releases FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.releases TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.catalog_heads FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.catalog_heads TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.component_library FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.component_library TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.release_components FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.release_components TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.release_manifest_v2_snapshots FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.release_manifest_v2_snapshots TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_requirements FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_requirements TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_instances FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_instances TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_generations FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_generations TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_bindings FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_bindings TO wamn_app, wamn_scenario_author; \
             DO $effective_acl$ BEGIN \
               IF has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'DELETE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.releases', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalog_heads', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.component_library', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.release_components', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.release_manifest_v2_snapshots', 'INSERT') THEN \
                 RAISE EXCEPTION USING ERRCODE = '42501', \
                   MESSAGE = 'authoring-effective-privilege-out-of-bounds:catalog'; \
               END IF; \
             END $effective_acl$; \
             DO $confine_catalog$ DECLARE rel text; BEGIN \
               FOREACH rel IN ARRAY ARRAY[ \
                 'catalog.catalogs', \
                 'catalog.schema_migrations', \
                 'catalog.entities', \
                 'catalog.fields', \
                 'catalog.relations', \
                 'catalog.indexes', \
                 'catalog.constraints', \
                 'catalog.rls_policies', \
                 'catalog.seed_datasets', \
                 'catalog.event_registrations'] LOOP \
                 IF to_regclass(rel) IS NULL THEN CONTINUE; END IF; \
                 EXECUTE format( \
                   'REVOKE ALL PRIVILEGES ON %s FROM PUBLIC, wamn_app, wamn_scenario_author', rel); \
                 EXECUTE format('GRANT SELECT ON %s TO wamn_app', rel); \
                 IF has_any_column_privilege('wamn_app', rel, 'INSERT,REFERENCES') \
                    OR has_table_privilege('wamn_app', rel, 'UPDATE,DELETE,TRUNCATE,TRIGGER') \
                    OR has_any_column_privilege( \
                         'wamn_scenario_author', rel, 'SELECT,INSERT,UPDATE,REFERENCES') \
                    OR has_table_privilege('wamn_scenario_author', rel, 'DELETE,TRUNCATE,TRIGGER') THEN \
                   RAISE EXCEPTION USING ERRCODE = '42501', \
                     MESSAGE = 'catalog-schema-model-privilege-out-of-bounds:' || rel; \
                 END IF; \
               END LOOP; \
               IF to_regclass('catalog.event_registrations') IS NOT NULL THEN \
                 EXECUTE 'GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app'; \
                 IF NOT has_column_privilege( \
                          'wamn_app', 'catalog.event_registrations', 'tenant_id', 'UPDATE') \
                    OR has_column_privilege( \
                         'wamn_app', 'catalog.event_registrations', 'catalog_id', 'UPDATE') \
                    OR has_column_privilege( \
                         'wamn_app', 'catalog.event_registrations', 'registration_id', 'UPDATE') \
                    OR has_column_privilege( \
                         'wamn_app', 'catalog.event_registrations', 'flow_id', 'UPDATE') \
                    OR has_column_privilege( \
                         'wamn_app', 'catalog.event_registrations', 'entity_id', 'UPDATE') \
                    OR has_column_privilege( \
                         'wamn_app', 'catalog.event_registrations', 'registration', 'UPDATE') THEN \
                   RAISE EXCEPTION USING ERRCODE = '42501', \
                     MESSAGE = 'catalog-registration-lock-grant-out-of-bounds'; \
                 END IF; \
               END IF; \
             END $confine_catalog$;",
        )
        .await
        .context("converge host-only catalog authoring privileges")
}

/// Lock the stable head for a release, initializing it from an applied legacy
/// catalog in the same transaction. Locking the legacy catalog row serializes
/// concurrent first-head initialization and makes its pinned runs visible to
/// the replacement guard.
pub(crate) async fn lock_or_initialize_catalog_head(
    client: &impl tokio_postgres::GenericClient,
    tenant: &str,
    catalog_id: &str,
    environment: &str,
) -> anyhow::Result<Option<i32>> {
    if let Some(head) = client
        .query_opt(
            wamn_schema_control::sql::lock_catalog_head_sql(),
            &[&tenant, &catalog_id, &environment],
        )
        .await
        .context("lock catalog head")?
    {
        return Ok(Some(head.get(0)));
    }
    let legacy_applied = client
        .query_opt(
            wamn_schema_control::sql::lock_current_applied_version_sql(),
            &[&tenant, &catalog_id, &environment],
        )
        .await
        .context("lock applied legacy catalog")?
        .map(|row| row.get::<_, i32>(0));
    if let Some(version) = legacy_applied {
        client
            .execute(
                wamn_schema_control::sql::advance_catalog_head_sql(),
                &[&tenant, &catalog_id, &environment, &version],
            )
            .await
            .context("initialize catalog head from applied legacy catalog")?;
    }
    Ok(legacy_applied)
}

/// Ensure + upsert the `wamn_entities` map for every entity of `cat` (rows are
/// upsert-only; a dropped entity's row keeps old-WAL decode resolvable).
/// Generic over [`tokio_postgres::GenericClient`] so `migrate-catalog` can run
/// it INSIDE its apply transaction (atomic with the rename DDL).
pub async fn upsert_entity_map(
    client: &impl tokio_postgres::GenericClient,
    cat: &wamn_schema_model::Catalog,
    schema: &BareSchemaName,
) -> anyhow::Result<()> {
    client
        .batch_execute(&wamn_control_provision::sql::ensure_entity_map_sql(
            schema.as_str(),
        ))
        .await
        .context("ensure entity map")?;
    let upsert = wamn_control_provision::sql::upsert_entity_map_sql(schema.as_str());
    for e in &cat.entities {
        client
            .execute(upsert.as_str(), &[&e.id.as_str(), &e.name])
            .await
            .with_context(|| format!("upsert entity map row for {:?}", e.id))?;
    }
    Ok(())
}

/// The D24 registration-orphan guard (EVT-REG, wamn-rmxa), shared by
/// publish-catalog and migrate-catalog. Reads every event registration for
/// `cat`'s catalog id across ALL tenants and refuses when any references an
/// entity `cat` does not keep, naming every orphan (the pure decision
/// `wamn_schema_control::check_registration_orphans`). Read-only: a refusal
/// mutates nothing.
///
/// **The empty read is the dangerous one (wamn-0h0g.12.119).** This guard gates a
/// DESTRUCTIVE apply — `migrate-catalog` has no other refusal in front of it — so
/// "I saw no registrations" and "I could not read the registrations" must not be
/// the same value. Both used to arrive as zero rows and CLEAR the apply, letting
/// the migration REMOVE an entity that registrations still referenced while the
/// run reported clean. Both are now named refusals, mirroring wamn-0h0g.12.103:
///
/// - **absent** — no `catalog.event_registrations` table at all. NOT a
///   provisioned-but-empty registration set, which legitimately passes.
/// - **unreadable** — the read errored. The dangerous member is silent: the table
///   is `FORCE ROW LEVEL SECURITY`, so a session that does not BYPASS RLS reads
///   zero rows with NO error. That includes the table's own non-superuser owner
///   (FORCE strips the owner exemption) and a role that merely INHERITS a
///   BYPASSRLS role, because Postgres checks BYPASSRLS on the current role only.
///   The read therefore runs under `row_security = off`, the `pg_dump` idiom,
///   which makes Postgres raise SQLSTATE 42501 instead of filtering — so the
///   silence becomes an error and the error becomes this refusal.
pub(crate) async fn guard_registration_orphans(
    client: &impl tokio_postgres::GenericClient,
    cat: &wamn_schema_model::Catalog,
) -> anyhow::Result<()> {
    let table_present: bool = client
        .query_one(
            "SELECT to_regclass('catalog.event_registrations') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.event_registrations")?
        .get(0);
    if !table_present {
        return Err(wamn_schema_control::UnreadableRegistrations {
            kind: wamn_schema_control::UnreadableRegistrationsKind::OrphanGuardAbsent,
            catalog_id: cat.catalog_id.clone(),
        }
        .into());
    }
    client
        .batch_execute("SET row_security = off")
        .await
        .context("SET row_security = off for the cross-tenant registration read")?;
    let read = client
        .query(
            &wamn_schema_control::sql::select_registrations_for_catalog_sql(),
            &[&cat.catalog_id],
        )
        .await;
    // Best-effort restore: a failed read inside a caller's open transaction
    // aborts it, so RESET cannot run and the caller is already unwinding to a
    // ROLLBACK that restores the GUC anyway.
    let _ = client.batch_execute("RESET row_security").await;
    let rows = read.map_err(|e| {
        anyhow::Error::new(e).context(wamn_schema_control::UnreadableRegistrations {
            kind: wamn_schema_control::UnreadableRegistrationsKind::OrphanGuardUnreadable,
            catalog_id: cat.catalog_id.clone(),
        })
    })?;
    let referenced: Vec<wamn_schema_control::RegistrationRef> = rows
        .iter()
        .map(|row| wamn_schema_control::RegistrationRef {
            registration_id: row.get(0),
            tenant: row.get(1),
            entity_id: row.get(2),
        })
        .collect();
    let present: std::collections::BTreeSet<&str> =
        cat.entities.iter().map(|e| e.id.as_str()).collect();
    wamn_schema_control::check_registration_orphans(&present, &referenced)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// Run-state provisioning. Shared with the integration proof so it provisions
// through the same code path production provisioning uses.
// ---------------------------------------------------------------------------

/// Ensure the non-superuser runtime role exists (pre-created in production;
/// bare gate/throwaway databases lack it). Shared with `reconcile-run-plane`,
/// whose sections GRANT to it.
pub(crate) async fn ensure_wamn_app_role(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "DO $$ BEGIN \
               PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("ensure wamn_app role")?;
    client
        .batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .context("ensure host-only wamn_scenario_author role")?;
    client
        .batch_execute(
            "SELECT pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
             REVOKE wamn_scenario_author FROM wamn_app",
        )
        .await
        .context("separate guest and scenario-author roles")
}

/// Apply `deploy/sql/run-state.sql` (the `runs` plane) into `schema` when its
/// `runs` table is absent. Returns whether it applied (false = already there).
pub async fn ensure_runstate(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<bool> {
    ensure_wamn_app_role(client).await?;
    if table_exists(client, schema, "runs").await? {
        return Ok(false);
    }
    let ddl = rewrite_schema(include_str!("../../../deploy/sql/run-state.sql"), schema);
    client
        .batch_execute(&ddl)
        .await
        .context("apply run-state")?;
    Ok(true)
}

async fn table_exists(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
    table: &str,
) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS ( SELECT FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2 )",
            &[&schema.as_str(), &table],
        )
        .await
        .with_context(|| format!("probe {schema}.{table}"))?
        .get(0))
}

/// Compile a wamn-schema-compiler dataset against the catalog into idempotent INSERTs.
pub fn seed_dataset_sql(
    dataset_json: &str,
    cat: &wamn_schema_model::Catalog,
    tenant: &str,
) -> anyhow::Result<String> {
    let dataset = wamn_schema_compiler::seed::Dataset::from_json(dataset_json)
        .context("parse seed dataset")?;
    let plan = wamn_schema_compiler::seed::compile(&dataset, cat, tenant)
        .map_err(|e| anyhow::anyhow!("seed compile: {e}"))?;
    plan.sql().map_err(|e| anyhow::anyhow!("seed sql: {e}"))
}
