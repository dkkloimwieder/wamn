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
//! applies the run-state storage (`deploy/sql/run-state.sql`: runs/node_runs),
//! the flow registry (`deploy/sql/flows.sql`), and the authoring test
//! orchestration tables (`deploy/sql/authoring-tests.sql`) into the project schema —
//! the canonical deploy files, embedded at compile time and rewritten
//! from `wamn_run` to the target schema — when their tables are absent;
//! `--seed-dataset` compiles a wamn-schema-compiler (3.6) dataset against the catalog and
//! applies it (deterministic ids, `ON CONFLICT DO NOTHING` — idempotent); and
//! `--flow` resolves standard-node interfaces, constructs canonical CF-DEF-ID
//! artifacts, and publishes artifacts + release membership + head atomically.
//! It does not write the retired mutable `flows.active` publication path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

// The canonical `wamn_run` → project-schema deploy-DDL rewrite and its
// lowercase bare-schema type share one owner in the reconcile-run-plane
// planner's crate.
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

#[derive(Debug, Args)]
pub struct PublishCatalogArgs {
    /// Path to the catalog JSON to snapshot (the applied catalog for the project).
    #[arg(long)]
    pub catalog: PathBuf,

    /// Superuser Postgres URL — bypasses RLS to write the snapshot and (with
    /// `--provision`) create the schema/tables (env `WAMN_PG_ADMIN_URL`).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Tenant stored with the RLS-scoped snapshot.
    #[arg(long)]
    pub tenant: String,

    /// Optional callable-flow POC project configuration. The accepted shape is
    /// exactly `{"raw_sql_enabled": <boolean>}`; other environments omit this
    /// fixture and retain the fail-closed default.
    #[arg(long)]
    pub project_config: Option<PathBuf>,

    /// Schema the `wamn_catalog` table (and, with `--provision`, the entity
    /// tables) live in.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Also create the schema + apply the 3.2 tenant floor for the catalog (the
    /// entity tables) when they are absent. Additive: never drops or alters.
    #[arg(long)]
    pub provision: bool,

    /// Also apply the run-state storage (runs/node_runs, `deploy/sql/run-state.sql`)
    /// and the flow registry (`deploy/sql/flows.sql`) into the schema when their
    /// tables are absent. Additive: never drops or alters.
    #[arg(long)]
    pub runstate: bool,

    /// Seed dataset JSON (wamn-schema-compiler, 3.6) compiled against the catalog and
    /// applied under `--tenant` (deterministic ids; idempotent re-apply).
    #[arg(long)]
    pub seed_dataset: Option<PathBuf>,

    /// Flow graph JSON (wamn-flow, 5.1) to resolve and publish as an immutable
    /// member of this catalog release.
    #[arg(long)]
    pub flow: Vec<PathBuf>,

    /// Sources and attachments JSON for this immutable catalog release.
    #[arg(long)]
    pub exposure: Option<PathBuf>,

    /// Skip the post-publish REPLICA IDENTITY reconcile (EVT-RI-ORCH, l5i9.61).
    /// By default publish reconciles RI for the catalog's data schema so an
    /// entity that needs the old image is never left on DEFAULT; pass this to run
    /// `reconcile-replica-identity` separately instead.
    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,
}

/// Publish one catalog snapshot — CLOSED by the wamn-0h0g.8.18 cutover.
///
/// The authoring, draft, and report store this verb published against moved to
/// the control database, so a project-database release would name a definition
/// nothing reads. Every invocation refuses with
/// [`crate::CONTROL_DEFINITION_PUBLISH_REFUSAL`] **before any filesystem read or
/// admin connection**: the refusal sits after the pure schema-name validator
/// (which already refused before any effect and keeps doing so) and before the
/// first `std::fs::read_to_string`.
///
/// The publication body below stays in the tree deliberately. wamn-0h0g.15.14
/// owns the replacement path and rewrites it against the control store; deleting
/// it here would throw away the landed release/journal/head ordering that
/// replacement has to preserve.
#[expect(
    unreachable_code,
    // `schema` is validated before the refusal on purpose — the pre-I/O guard
    // proves a malformed schema name is refused without opening a file — and it
    // is consumed only by the unreachable body below. When wamn-0h0g.15.14
    // removes the refusal, both expectations go unfulfilled and this attribute
    // must come off with it; that unfulfilled-expectation warning is the point.
    unused_variables,
    reason = "wamn-0h0g.8.18 closes the verb without deleting the publication \
              body wamn-0h0g.15.14 rewrites"
)]
pub async fn run(args: PublishCatalogArgs) -> anyhow::Result<()> {
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid schema name {:?}", args.schema))?;

    anyhow::bail!(crate::CONTROL_DEFINITION_PUBLISH_REFUSAL);

    // Parse (and thereby validate) the catalog; this is the snapshot document.
    let catalog_src = std::fs::read_to_string(&args.catalog)
        .with_context(|| format!("read catalog {}", args.catalog.display()))?;
    let cat = wamn_schema_model::Catalog::from_json(&catalog_src)
        .map_err(|e| anyhow::anyhow!("catalog parse/validate: {e}"))?;
    let document = cat.to_json();
    let raw_sql_enabled = args
        .project_config
        .as_ref()
        .map(|path| {
            let source = std::fs::read_to_string(path)
                .with_context(|| format!("read project config {}", path.display()))?;
            parse_raw_sql_config(&source)
                .with_context(|| format!("parse project config {}", path.display()))
        })
        .transpose()?;
    let prepared_flows = prepare_publication_flows(&args)?;

    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    let (client, conn) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = publish(
        &client,
        &cat,
        &args,
        &document,
        &schema,
        raw_sql_enabled,
        prepared_flows,
    )
    .await;
    drop(client);
    let _ = conn_task.await;
    result?;

    println!(
        "published catalog snapshot: schema={} tenant={} (provision={})",
        args.schema, args.tenant, args.provision
    );
    Ok(())
}

async fn publish(
    client: &tokio_postgres::Client,
    cat: &wamn_schema_model::Catalog,
    args: &PublishCatalogArgs,
    document: &str,
    schema: &BareSchemaName,
    raw_sql_enabled: Option<bool>,
    prepared_flows: PreparedPublicationFlows,
) -> anyhow::Result<()> {
    // D24 (EVT-REG, wamn-rmxa): refuse a publish that would drop an entity still
    // referenced by an event registration — BEFORE any mutation, naming every
    // orphan. The owner deletes the registrations via the API first; publish
    // never seeds or prunes them.
    guard_registration_orphans(client, cat).await?;

    // Ensure the non-superuser runtime role exists (pre-created in production).
    ensure_wamn_app_role(client).await?;
    ensure_catalog_storage(client).await?;

    let seed_sql = if let Some(path) = &args.seed_dataset {
        let src = std::fs::read_to_string(path)
            .with_context(|| format!("read seed dataset {}", path.display()))?;
        Some((
            path.display().to_string(),
            seed_dataset_sql(&src, cat, &args.tenant)?,
        ))
    } else {
        None
    };
    let PreparedPublicationFlows {
        artifacts,
        exposure,
    } = prepared_flows;
    let expected_base: Option<i32> = client
        .query_one(
            wamn_schema_control::sql::select_publication_base_sql(),
            &[&args.tenant, &cat.catalog_id, &"dev"],
        )
        .await
        .context("read publication base")?
        .get(0);
    client
        .batch_execute("BEGIN")
        .await
        .context("begin publication")?;
    let publication_result: anyhow::Result<()> = async {

        // Create the schema if absent and pin this session's search_path to it, so
        // every statement below — and the parameterized UPSERT — resolves unqualified
        // names there, exactly as the gateway does via the host-injected search_path.
        client
            .batch_execute(&format!(
                "CREATE SCHEMA IF NOT EXISTS {schema}; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app; \
                 SET search_path TO {schema};",
                schema = schema.quoted(),
            ))
            .await
            .context("ensure schema")?;

    // The catalog snapshot table (idempotent): tenant-scoped, read-only to wamn_app.
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS wamn_catalog ( \
               id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
               tenant_id text NOT NULL, \
               document jsonb NOT NULL); \
             ALTER TABLE wamn_catalog ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE wamn_catalog FORCE ROW LEVEL SECURITY; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_policies WHERE schemaname = current_schema() \
                              AND tablename = 'wamn_catalog' AND policyname = 'wamn_catalog_tenant') THEN \
                 CREATE POLICY wamn_catalog_tenant ON wamn_catalog \
                   USING (tenant_id = current_setting('app.tenant', true)) \
                   WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
               END IF; \
             END $$; \
             GRANT SELECT ON wamn_catalog TO wamn_app;",
        )
        .await
        .context("ensure wamn_catalog table")?;

    // Optionally stand up the entity tables (the 3.2 floor). The floor DDL is not
    // idempotent (plain CREATE TABLE / CREATE POLICY), so apply it only when the
    // first entity table is absent; a re-run against a provisioned schema is a
    // clean no-op that still refreshes the snapshot below.
    if args.provision {
        let first = cat
            .entities
            .first()
            .map(|e| e.name.clone())
            .context("catalog has no entities to provision")?;
        let exists: bool = client
            .query_one(
                "SELECT EXISTS ( SELECT FROM information_schema.tables \
                 WHERE table_schema = current_schema() AND table_name = $1 )",
                &[&first],
            )
            .await
            .context("probe floor")?
            .get(0);
        if exists {
            println!("floor already present in schema {schema}; skipping provision");
        } else {
            let floor = wamn_schema_compiler::Migration::create(cat)
                .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
                .sql()
                .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
            client.batch_execute(&floor).await.context("apply floor")?;
            println!("provisioned tenant floor in schema {schema}");
        }
    }

    // Optionally apply the run-state storage + flow registry (POC-F1).
    if args.runstate {
        if ensure_runstate(client, schema).await? {
            println!("applied run-state storage (runs/node_runs) in schema {schema}");
        } else {
            println!("run-state storage already present in schema {schema}; skipping");
        }
        if ensure_flow_registry(client, schema).await? {
            println!("applied flow registry (flows) in schema {schema}");
        } else {
            println!("flow registry already present in schema {schema}; skipping");
        }
        // Authoring test orchestration (FKs to runs, so after ensure_runstate).
        if ensure_flow_tests(client, schema).await? {
            println!("applied authoring test orchestration in schema {schema}");
        } else {
            println!("authoring test orchestration already present in schema {schema}; skipping");
        }
    }

    // Optionally compile + apply a wamn-schema-compiler dataset against this catalog.
    if let Some((path, sql)) = &seed_sql {
        client
            .batch_execute(sql)
            .await
            .context("apply seed dataset")?;
        println!("applied seed dataset {path} in schema {schema}");
    }

    if let Some(enabled) = raw_sql_enabled {
        let value = enabled.to_string();
        client
            .execute(
                "INSERT INTO app_system.configurations \
                   (tenant_id, config_key, config_value) \
                 VALUES ($1, 'raw_sql_enabled', $2::text::jsonb) \
                 ON CONFLICT (tenant_id, config_key) DO UPDATE \
                 SET config_value = EXCLUDED.config_value, updated_at = now()",
                &[&args.tenant, &value],
            )
            .await
            .context("apply callable-flow POC project config")?;
    }

    publish_release(
        client,
        cat,
        &args.tenant,
        "dev",
        schema,
        artifacts,
        exposure,
        document,
        expected_base,
    )
    .await?;

    // Snapshot UPSERT: replace this tenant's row. The document (arbitrary jsonb)
    // is a bound parameter — never string-interpolated — so it can carry no SQL;
    // the superuser connection bypasses the RLS WITH CHECK + the SELECT-only grant.
    // `$2::text::jsonb` types the parameter as `text` (so a Rust `&str` binds) and
    // then casts to jsonb — a bare `$2::jsonb` types the parameter as jsonb, which
    // tokio_postgres cannot serialize a `&str` into.
    client
        .execute(
            "DELETE FROM wamn_catalog WHERE tenant_id = $1",
            &[&args.tenant],
        )
        .await
        .context("clear old snapshot")?;
    client
        .execute(
            "INSERT INTO wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)",
            &[&args.tenant, &document],
        )
        .await
        .context("write snapshot")?;

    // Refresh the decode-time entity map (wamn-l5i9.11): entity id → the
    // table's CURRENT pg_class OID, for every catalog entity whose table
    // exists (absent tables upsert nothing). This is also the BACKFILL path
    // for an env CDC-enabled after its catalog was published — re-running
    // publish-catalog populates the map.
    upsert_entity_map(client, cat, schema).await?;

    // EVT-RI-ORCH (wamn-l5i9.61): reconcile REPLICA IDENTITY for this schema as
    // the automatic operational caller — the catalog's table/registration set
    // just changed, so an entity that needs the old image must be flipped to FULL
    // here rather than waiting for a manual verb run (the flip is non-retroactive,
    // so the gap would be permanent for events captured meanwhile). Idempotent and
    // scoped strictly to `schema`; a schema without the floor yet is a clean no-op.
    if !args.skip_reconcile_replica_identity {
        crate::reconcile_replica_identity::reconcile_after_apply(client, cat, schema.as_str())
            .await?;
    }

        anyhow::Ok(())
    }
    .await;
    finish_publication_transaction(client, publication_result).await
}

fn parse_raw_sql_config(source: &str) -> anyhow::Result<bool> {
    let value: serde_json::Value = serde_json::from_str(source)?;
    let object = value
        .as_object()
        .context("project config must be a JSON object")?;
    if object.len() != 1 {
        anyhow::bail!("project config must contain exactly raw_sql_enabled");
    }
    object
        .get("raw_sql_enabled")
        .and_then(serde_json::Value::as_bool)
        .context("raw_sql_enabled must be a JSON boolean")
}

/// End a manually scoped publication transaction without ever returning a
/// reusable client in an open or aborted transaction. The original publication
/// error remains the primary error if rollback itself also fails.
async fn finish_publication_transaction(
    client: &tokio_postgres::Client,
    publication_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match publication_result {
        Ok(()) => {
            if let Err(commit_error) = client
                .batch_execute("COMMIT")
                .await
                .context("commit publication")
            {
                let rollback_result = client.batch_execute("ROLLBACK").await;
                return match rollback_result {
                    Ok(()) => Err(commit_error),
                    Err(rollback_error) => Err(commit_error.context(format!(
                        "publication rollback after commit failure also failed: {rollback_error}"
                    ))),
                };
            }
            Ok(())
        }
        Err(publication_error) => {
            let rollback_result = client.batch_execute("ROLLBACK").await;
            match rollback_result {
                Ok(()) => Err(publication_error),
                Err(rollback_error) => Err(publication_error.context(format!(
                    "publication rollback also failed: {rollback_error}"
                ))),
            }
        }
    }
}

/// Install or additively upgrade the catalog persistence schema.
pub async fn ensure_catalog_storage(client: &tokio_postgres::Client) -> anyhow::Result<()> {
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
            "SELECT to_regclass('catalog.flow_artifacts') IS NOT NULL, \
                    to_regclass('catalog.release_manifests') IS NOT NULL, \
                    to_regclass('catalog.release_flows') IS NOT NULL, \
                    to_regclass('catalog.catalog_heads') IS NOT NULL, \
                    to_regclass('catalog.release_exposure_manifests') IS NOT NULL, \
                    to_regclass('catalog.release_sources') IS NOT NULL, \
                    to_regclass('catalog.release_attachments') IS NOT NULL, \
                    to_regclass('catalog.attachment_tombstones') IS NOT NULL, \
                    to_regclass('catalog.attachment_activation') IS NOT NULL, \
                    to_regclass('catalog.attachment_activation_events') IS NOT NULL, \
                    EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'flow_artifacts' \
                               AND column_name = 'verified_author_principal'), \
                    EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'release_manifests' \
                               AND column_name = 'verified_publisher_principal'), \
                    EXISTS (SELECT 1 FROM pg_constraint con \
                             JOIN pg_class rel ON rel.oid = con.conrelid \
                             JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                             WHERE ns.nspname = 'catalog' \
                               AND rel.relname = 'flow_artifacts' \
                               AND con.conname = 'flow_artifacts_verified_author_principal_check' \
                               AND pg_get_constraintdef(con.oid, true) = \
                                   'CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''''::text)'), \
                    EXISTS (SELECT 1 FROM pg_constraint con \
                             JOIN pg_class rel ON rel.oid = con.conrelid \
                             JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                             WHERE ns.nspname = 'catalog' \
                               AND rel.relname = 'release_manifests' \
                               AND con.conname = 'release_manifests_verified_publisher_principal_check' \
                               AND pg_get_constraintdef(con.oid, true) = \
                                   'CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''''::text)'), \
                    to_regclass('catalog.connection_requirements') IS NOT NULL, \
                    to_regclass('catalog.connection_instances') IS NOT NULL, \
                    to_regclass('catalog.connection_generations') IS NOT NULL, \
                    to_regclass('catalog.connection_bindings') IS NOT NULL, \
                    to_regclass('catalog.connection_generation_retention') IS NOT NULL, \
                    to_regclass('catalog.flow_drafts') IS NOT NULL, \
                    to_regclass('catalog.validated_flow_drafts') IS NOT NULL, \
                    to_regclass('catalog.draft_safe_connection_grants') IS NOT NULL, \
                    to_regclass('catalog.authoring_command_audit') IS NOT NULL, \
                    EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'flow_drafts' \
                               AND column_name = 'definition'), \
                    EXISTS (SELECT 1 FROM information_schema.columns \
                             WHERE table_schema = 'catalog' \
                               AND table_name = 'authoring_command_audit' \
                               AND column_name = 'provenance_commit')",
            &[],
        )
        .await?;
    let release_objects = [
        release_row.get::<_, bool>(0),
        release_row.get::<_, bool>(1),
        release_row.get::<_, bool>(2),
        release_row.get::<_, bool>(3),
        release_row.get::<_, bool>(4),
        release_row.get::<_, bool>(5),
        release_row.get::<_, bool>(6),
        release_row.get::<_, bool>(7),
        release_row.get::<_, bool>(8),
        release_row.get::<_, bool>(9),
    ];
    if release_objects.iter().all(|present| *present) {
        let provenance_storage = [
            release_row.get::<_, bool>(10),
            release_row.get::<_, bool>(11),
            release_row.get::<_, bool>(12),
            release_row.get::<_, bool>(13),
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
            release_row.get::<_, bool>(14),
            release_row.get::<_, bool>(15),
            release_row.get::<_, bool>(16),
            release_row.get::<_, bool>(17),
            release_row.get::<_, bool>(18),
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

        let authoring_draft_objects = [
            release_row.get::<_, bool>(19),
            release_row.get::<_, bool>(20),
        ];
        if !authoring_draft_objects.iter().all(|present| *present) {
            anyhow::ensure!(
                authoring_draft_objects.iter().all(|present| !*present),
                "catalog authoring draft storage is partially installed; reconcile it before publication"
            );
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN AUTHORING DRAFT STORAGE MIGRATION")
                .expect("authoring draft storage migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END AUTHORING DRAFT STORAGE MIGRATION")
                .expect("authoring draft storage migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install authoring draft storage")?;
        }

        if !release_row.get::<_, bool>(21) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN AUTHORING CONNECTION AUTHORITY MIGRATION")
                .expect("authoring connection authority migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END AUTHORING CONNECTION AUTHORITY MIGRATION")
                .expect("authoring connection authority migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install authoring connection authority")?;
        }

        // The adapter's startup authority probe hard-requires the ledger, so an
        // existing catalog that predates wamn-ctc8.8 must gain it here or the
        // management surface refuses to start against that project.
        if !release_row.get::<_, bool>(22) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN AUTHORING COMMAND AUDIT MIGRATION")
                .expect("authoring command audit migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END AUTHORING COMMAND AUDIT MIGRATION")
                .expect("authoring command audit migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install authoring command audit")?;
        }

        // Both slices below run after the two above deliberately: an existing
        // catalog may have just gained `flow_drafts` or the audit ledger in
        // this same pass, and these alter exactly those tables.

        // A draft saved before wamn-ftfc.2 was reparsed into `jsonb`, so its
        // exact submitted text no longer exists anywhere. The backfill recovers
        // the document, not the bytes; that is the most an upgrade can honestly
        // do, and every revision saved afterwards is byte-exact.
        if !release_row.get::<_, bool>(23) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN AUTHORING DRAFT DEFINITION MIGRATION")
                .expect("authoring draft definition migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END AUTHORING DRAFT DEFINITION MIGRATION")
                .expect("authoring draft definition migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install exact-text authoring draft storage")?;
        }

        if !release_row.get::<_, bool>(24) {
            let start = CATALOG_SCHEMA_SQL
                .find("-- BEGIN AUTHORING COMMAND PROVENANCE MIGRATION")
                .expect("authoring command provenance migration start");
            let end = CATALOG_SCHEMA_SQL
                .find("-- END AUTHORING COMMAND PROVENANCE MIGRATION")
                .expect("authoring command provenance migration end");
            client
                .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
                .await
                .context("install authoring command provenance attribution")?;
        }

        ensure_authoring_catalog_privileges(client).await?;
        return Ok(());
    }
    anyhow::ensure!(
        release_objects.iter().all(|present| !*present),
        "catalog release storage is partially installed; reconcile it before publication"
    );
    let start = CATALOG_SCHEMA_SQL
        .find("CREATE TABLE catalog.flow_artifacts")
        .expect("catalog release section start");
    let end = CATALOG_SCHEMA_SQL
        .find("-- Migration history (2.5")
        .expect("catalog release section end");
    client
        .batch_execute(&CATALOG_SCHEMA_SQL[start..end])
        .await
        .context("install catalog release storage into baseline catalog")?;
    ensure_authoring_catalog_privileges(client).await?;
    Ok(())
}

/// Converge the catalog schema's role grants on an ALREADY-PROVISIONED database.
///
/// `deploy/sql/catalog-schema.sql` applies whole only on a fresh install, so this
/// is the only path that reaches a database provisioned by an earlier revision —
/// and it is re-run on every publish, which means an arm missing here silently
/// restores whatever the old file granted. Two boundaries live in one batch: the
/// authoring split between `wamn_app` and `wamn_scenario_author`, and the
/// wamn-0h0g.12.20-.12.29 confinement that leaves the guest-reachable `wamn_app`
/// LOGIN read-only on the ten schema-of-record relations no production writer
/// reaches through it. Each half asserts its own effective ACL before returning.
async fn ensure_authoring_catalog_privileges(
    client: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    client
        .batch_execute(
            "REVOKE wamn_scenario_author FROM wamn_app; \
             GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.flow_artifacts FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.flow_artifacts TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.release_manifests FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.release_manifests TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.release_flows FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.release_flows TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.catalog_heads FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.catalog_heads TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_requirements FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_requirements TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_instances FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_instances TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_generations FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_generations TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.connection_bindings FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.connection_bindings TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.flow_drafts FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT, INSERT, UPDATE ON catalog.flow_drafts TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.validated_flow_drafts FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.validated_flow_drafts TO wamn_app; \
             GRANT SELECT, INSERT ON catalog.validated_flow_drafts TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.draft_safe_connection_grants FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT ON catalog.draft_safe_connection_grants TO wamn_app, wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON catalog.authoring_command_audit FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT, INSERT ON catalog.authoring_command_audit TO wamn_scenario_author; \
             DO $effective_acl$ BEGIN \
               IF has_table_privilege('wamn_app', 'catalog.flow_drafts', 'INSERT') \
                  OR has_table_privilege('wamn_app', 'catalog.flow_drafts', 'UPDATE') \
                  OR has_table_privilege('wamn_app', 'catalog.flow_drafts', 'DELETE') \
                  OR has_table_privilege('wamn_app', 'catalog.validated_flow_drafts', 'INSERT') \
                  OR has_table_privilege('wamn_app', 'catalog.draft_safe_connection_grants', 'INSERT') \
                  OR has_table_privilege('wamn_app', 'catalog.draft_safe_connection_grants', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'DELETE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'TRUNCATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'REFERENCES') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.draft_safe_connection_grants', 'TRIGGER') \
                  OR has_table_privilege('wamn_app', 'catalog.authoring_command_audit', 'SELECT') \
                  OR has_table_privilege('wamn_app', 'catalog.authoring_command_audit', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.authoring_command_audit', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.authoring_command_audit', 'DELETE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'UPDATE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalogs', 'DELETE') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.flow_artifacts', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.release_manifests', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.release_flows', 'INSERT') \
                  OR has_table_privilege('wamn_scenario_author', 'catalog.catalog_heads', 'UPDATE') THEN \
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

// Seven call sites in this file; a params struct would churn them all for no
// behaviour change.
#[allow(clippy::too_many_arguments)]
async fn publish_release(
    client: &tokio_postgres::Client,
    cat: &wamn_schema_model::Catalog,
    tenant: &str,
    environment: &str,
    run_schema: &BareSchemaName,
    artifacts: Vec<PreparedFlowArtifact>,
    exposure: Option<(
        wamn_schema_control::ExposureRelease,
        Vec<wamn_schema_control::ResolvedAttachment>,
    )>,
    document: &str,
    expected_base: Option<i32>,
) -> anyhow::Result<()> {
    let members = release_members(&artifacts)?;
    let release_manifest = serde_json::to_string(
        &members
            .iter()
            .map(|member| {
                let artifact = artifacts
                    .iter()
                    .find(|prepared| prepared.artifact.identity().id().flow_id() == member.flow_id)
                    .expect("canonical member came from prepared artifact");
                serde_json::json!({
                    "flow-id": member.flow_id,
                    "flow-version": member.flow_version,
                    "artifact-hash": artifact.artifact.identity().artifact_hash().as_str(),
                })
            })
            .collect::<Vec<_>>(),
    )
    .context("serialize release manifest")?;
    let catalog_version = i32::try_from(cat.version).context("catalog version")?;
    let writes = async {
        let applied_version =
            lock_or_initialize_catalog_head(client, tenant, &cat.catalog_id, environment).await?;
        let runs_present: bool = client
            .query_one(
                &format!(
                    "SELECT to_regclass('{}.runs') IS NOT NULL",
                    run_schema.as_str()
                ),
                &[],
            )
            .await?
            .get(0);
        let nonterminal_runs = if runs_present {
            if let Some(applied) = applied_version {
                client
                    .query_one(
                        &wamn_schema_control::sql::count_nonterminal_release_runs_sql(
                            run_schema.as_str(),
                        ),
                        &[&tenant, &cat.catalog_id, &applied],
                    )
                    .await
                    .context("check release-pinned runs")?
                    .get(0)
            } else {
                0_i64
            }
        } else {
            0_i64
        };
        wamn_schema_control::guard_publication(&wamn_schema_control::PublicationGuard {
            expected_base,
            applied_version,
            nonterminal_runs,
            unresolved_sources: &[],
        })
        .map_err(anyhow::Error::new)?;
        let same_target_retry = publication_is_same_target_retry(applied_version, catalog_version)?;
        // Existence and membership are two reads now that membership is derived
        // from catalog.release_flows: the derivation answers `[]` for a release
        // that was never sealed exactly as it does for one sealed empty, so only
        // the identity row can say whether there is anything to conflict with.
        let release_sealed: bool = client
            .query_one(
                wamn_schema_control::sql::release_manifest_exists_sql(),
                &[&tenant, &cat.catalog_id, &catalog_version],
            )
            .await
            .context("preflight existing release identity")?
            .get(0);
        if release_sealed {
            let existing: String = client
                .query_one(
                    wamn_schema_control::sql::select_release_members_sql(),
                    &[&tenant, &cat.catalog_id, &catalog_version],
                )
                .await
                .context("preflight existing release membership")?
                .get(0);
            let existing: serde_json::Value =
                serde_json::from_str(&existing).context("parse stored release manifest")?;
            let requested: serde_json::Value =
                serde_json::from_str(&release_manifest).expect("prepared manifest is JSON");
            anyhow::ensure!(
                divergent_release_members(&existing, &requested).is_empty(),
                "catalog-release-content-conflict"
            );
        }

        for prepared in &artifacts {
            let artifact = &prepared.artifact;
            let id = artifact.identity().id();
            let flow_version = i32::try_from(id.flow_version()).context("flow version")?;
            client
                .execute(
                    wamn_schema_control::sql::register_flow_artifact_sql(),
                    &[
                        &tenant,
                        &id.flow_id(),
                        &flow_version,
                        &artifact.schema_version(),
                        &prepared.graph_json.as_str(),
                        &artifact.graph_hash(),
                        &artifact.identity().artifact_hash().as_str(),
                    ],
                )
                .await
                .with_context(|| {
                    format!(
                        "register immutable flow artifact {} v{}",
                        id.flow_id(),
                        id.flow_version()
                    )
                })?;
        }
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-artifacts"],
            )
            .await
            .context("publication boundary after artifacts")?;

        client
            .execute(
                "UPDATE catalog.catalogs SET state = 'superseded' \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
                   AND state = 'applied' AND version <> $4",
                &[&tenant, &cat.catalog_id, &environment, &catalog_version],
            )
            .await
            .context("supersede prior release")?;
        client
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state, document) \
                 VALUES ($1, $2, $3, $4, $5, 'applied', $6::text::jsonb) \
                 ON CONFLICT (tenant_id, catalog_id, version) DO NOTHING",
                &[
                    &tenant,
                    &cat.catalog_id,
                    &catalog_version,
                    &environment,
                    &cat.schema_version,
                    &document,
                ],
            )
            .await
            .context("persist catalog release")?;
        let stored: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM catalog.catalogs \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3 \
                   AND environment = $4 AND document = $5::text::jsonb)",
                &[
                    &tenant,
                    &cat.catalog_id,
                    &catalog_version,
                    &environment,
                    &document,
                ],
            )
            .await?
            .get(0);
        anyhow::ensure!(stored, "catalog-version-content-conflict");
        let journal_checksum = wamn_schema_control::sql::ddl_checksum(document);
        let exact_journal: bool = if same_target_retry {
            client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM catalog.schema_migrations \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
                       AND to_version = $4 \
                       AND statement_count = 0 \
                       AND destructive = false AND checksum = $5)",
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &environment,
                        &catalog_version,
                        &journal_checksum,
                    ],
                )
                .await
                .context("verify retried release publication journal")?
                .get(0)
        } else {
            client
                .execute(
                    wamn_schema_control::sql::record_release_publication_sql(),
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &environment,
                        &applied_version,
                        &catalog_version,
                        &journal_checksum,
                    ],
                )
                .await
                .context("record release publication")?;
            client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM catalog.schema_migrations \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
                       AND from_version IS NOT DISTINCT FROM $4 AND to_version = $5 \
                       AND statement_count = 0 \
                       AND destructive = false AND checksum = $6)",
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &environment,
                        &applied_version,
                        &catalog_version,
                        &journal_checksum,
                    ],
                )
                .await
                .context("verify release publication journal base")?
                .get(0)
        };
        anyhow::ensure!(exact_journal, "catalog-release-journal-conflict");
        client
            .execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-journal"],
            )
            .await
            .context("publication boundary after-journal")?;
        client
            .execute(
                wamn_schema_control::sql::register_release_manifest_sql(),
                &[&tenant, &cat.catalog_id, &catalog_version],
            )
            .await
            .context("seal release membership")?;
        for member in &members {
            let execution_bundle_hash = execution_bundle_hash_for(member, &artifacts);
            client
                .execute(
                    wamn_schema_control::sql::insert_release_flow_sql(),
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &catalog_version,
                        &member.flow_id,
                        &member.flow_version,
                        &execution_bundle_hash,
                    ],
                )
                .await
                .with_context(|| format!("publish flow {:?}", member.flow_id))?;
            let exact: bool = client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM catalog.release_flows \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
                       AND flow_id = $4 AND flow_version = $5 \
                       AND execution_bundle_hash = $6)",
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &catalog_version,
                        &member.flow_id,
                        &member.flow_version,
                        &execution_bundle_hash,
                    ],
                )
                .await?
                .get(0);
            anyhow::ensure!(exact, "catalog-release-content-conflict");
        }
        let exposure_json = serde_json::to_string(&exposure.as_ref().map_or_else(
            || serde_json::json!({"attachments":[],"sources":[]}),
            |(a, _)| serde_json::to_value(a).expect("exposure is serializable"),
        ))
        .context("serialize release exposure")?;
        client
            .execute(
                wamn_schema_control::sql::register_release_exposure_manifest_sql(),
                &[&tenant, &cat.catalog_id, &catalog_version, &exposure_json],
            )
            .await
            .context("seal release exposure")?;
        if let Some((authored, resolved)) = &exposure {
            for source in &authored.sources {
                let definition = serde_json::to_string(&source.definition)
                    .context("serialize exposure source")?;
                let source_hash = wamn_schema_control::sql::ddl_checksum(&definition);
                client
                    .execute(
                        wamn_schema_control::sql::insert_release_source_sql(),
                        &[
                            &tenant,
                            &cat.catalog_id,
                            &catalog_version,
                            &source.id,
                            &source.kind.as_str(),
                            &definition,
                            &source_hash,
                        ],
                    )
                    .await
                    .with_context(|| format!("publish source {:?}", source.id))?;
            }
            for definition in resolved {
                let authored_json = serde_json::to_string(&definition.attachment)
                    .context("serialize resolved attachment")?;
                client
                    .execute(
                        wamn_schema_control::sql::insert_release_attachment_sql(),
                        &[
                            &tenant,
                            &cat.catalog_id,
                            &catalog_version,
                            &definition.attachment.id,
                            &definition.attachment.kind.as_str(),
                            &definition.attachment.flow_id,
                            &definition.attachment.source_id,
                            &definition.definition_hash,
                            &authored_json,
                            &definition.normalized_host,
                            &definition.normalized_path,
                            &definition.normalized_template,
                            &definition.normalized_method,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!("publish attachment {:?}", definition.attachment.id)
                    })?;
            }
        }
        client
            .execute(
                wamn_schema_control::sql::apply_release_exposure_sql(),
                &[
                    &tenant,
                    &cat.catalog_id,
                    &environment,
                    &catalog_version,
                    &"publish-catalog",
                ],
            )
            .await
            .context("carry attachment activation")?;
        // wamn-0h0g.15.163: no member count is asserted here. Every member this
        // publish asked for was read back at exactly its requested content in the
        // loop above — the enforcement of record per ruling R5 — and the only
        // thing a count could add is "and nothing else is stored", which is the
        // invariant grow-only append abolished.
        for stage in ["after-members", "before-head"] {
            client
                .execute(
                    wamn_schema_control::sql::publication_boundary_sql(),
                    &[&stage],
                )
                .await
                .with_context(|| format!("publication boundary {stage}"))?;
        }
        client
            .execute(
                wamn_schema_control::sql::advance_catalog_head_sql(),
                &[&tenant, &cat.catalog_id, &environment, &catalog_version],
            )
            .await
            .context("advance catalog head")?;
        anyhow::Ok(())
    }
    .await;
    writes?;
    Ok(())
}

fn publication_is_same_target_retry(
    applied_version: Option<i32>,
    target_version: i32,
) -> anyhow::Result<bool> {
    let same_target_retry = applied_version == Some(target_version);
    if let Some(applied) = applied_version {
        anyhow::ensure!(
            same_target_retry || target_version > applied,
            "catalog-release-version-regression: target {target_version}, applied {applied}"
        );
    }
    Ok(same_target_retry)
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
// Run-state / flow-registry provisioning + flow registration. Shared with the
// integration proof so it provisions through the same code path production
// provisioning uses.
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

/// Apply `deploy/sql/run-state.sql` (runs + node_runs) into `schema` when its
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

/// Apply `deploy/sql/flows.sql` (the flow registry) into `schema` when its `flows`
/// table is absent. Returns whether it applied.
pub async fn ensure_flow_registry(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<bool> {
    if table_exists(client, schema, "flows").await? {
        return Ok(false);
    }
    let ddl = rewrite_schema(include_str!("../../../deploy/sql/flows.sql"), schema);
    client
        .batch_execute(&ddl)
        .await
        .context("apply flow registry")?;
    Ok(true)
}

/// Apply the authoring test orchestration artifact into `schema` when all three
/// retained tables are absent. Returns whether it applied.
pub async fn ensure_flow_tests(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<bool> {
    ensure_wamn_app_role(client).await?;
    let authoring_storage = client
        .query_one(&authoring_run_storage_probe_sql(schema), &[])
        .await
        .context("probe authoring test orchestration storage")?;
    let authoring_objects = [
        authoring_storage.get::<_, bool>(0),
        authoring_storage.get::<_, bool>(1),
        authoring_storage.get::<_, bool>(2),
    ];
    let installed = if authoring_objects.iter().all(|present| *present) {
        false
    } else {
        anyhow::ensure!(
            authoring_objects.iter().all(|present| !*present),
            "authoring test orchestration is partially installed; reconcile it before publication"
        );
        let ddl = rewrite_schema(
            include_str!("../../../deploy/sql/authoring-tests.sql"),
            schema,
        );
        client
            .batch_execute(&ddl)
            .await
            .context("install authoring test orchestration")?;
        true
    };
    ensure_authoring_run_privileges(client, schema).await?;
    Ok(installed)
}

fn authoring_run_storage_probe_sql(schema: &BareSchemaName) -> String {
    let schema = schema.as_str();
    format!(
        "SELECT to_regclass('{schema}.authoring_test_run_reservations') IS NOT NULL, \
                to_regclass('{schema}.authoring_test_case_runs') IS NOT NULL, \
                to_regclass('{schema}.authoring_test_reports') IS NOT NULL"
    )
}

async fn ensure_authoring_run_privileges(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<()> {
    client
        .batch_execute(&authoring_run_privileges_sql(schema))
        .await
        .context("converge host-only authoring run privileges")
}

fn authoring_run_privileges_sql(schema: &BareSchemaName) -> String {
    let schema_name = schema.quoted();
    let reports = format!("{schema_name}.authoring_test_reports");
    format!(
        "REVOKE wamn_scenario_author FROM wamn_app; \
             GRANT USAGE ON SCHEMA {schema_name} TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON {schema_name}.authoring_test_run_reservations \
                 FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT, INSERT, UPDATE ON {schema_name}.authoring_test_run_reservations \
                 TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON {schema_name}.authoring_test_case_runs \
                 FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT, INSERT, UPDATE ON {schema_name}.authoring_test_case_runs \
                 TO wamn_scenario_author; \
             REVOKE ALL PRIVILEGES ON {schema_name}.authoring_test_reports \
                 FROM PUBLIC, wamn_app, wamn_scenario_author; \
             GRANT SELECT, INSERT ON {schema_name}.authoring_test_reports \
                 TO wamn_scenario_author; \
             DO $effective_acl$ BEGIN \
               IF EXISTS ( \
                   SELECT 1 \
                     FROM pg_catalog.unnest(ARRAY[ \
                         'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', \
                         'REFERENCES', 'TRIGGER']::text[]) AS candidate(privilege) \
                    WHERE pg_catalog.has_table_privilege( \
                              'wamn_app', '{reports}', candidate.privilege) \
                       OR (candidate.privilege IN ( \
                               'SELECT', 'INSERT', 'UPDATE', 'REFERENCES') \
                           AND pg_catalog.has_any_column_privilege( \
                               'wamn_app', '{reports}', candidate.privilege)) \
               ) \
                  OR NOT pg_catalog.has_table_privilege( \
                      'wamn_scenario_author', '{reports}', 'SELECT') \
                  OR NOT pg_catalog.has_table_privilege( \
                      'wamn_scenario_author', '{reports}', 'INSERT') \
                  OR EXISTS ( \
                      SELECT 1 \
                        FROM pg_catalog.unnest(ARRAY[ \
                            'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', \
                            'TRIGGER']::text[]) AS candidate(privilege) \
                       WHERE pg_catalog.has_table_privilege( \
                                 'wamn_scenario_author', '{reports}', \
                                 candidate.privilege) \
                          OR (candidate.privilege IN ('UPDATE', 'REFERENCES') \
                              AND pg_catalog.has_any_column_privilege( \
                                  'wamn_scenario_author', '{reports}', \
                                  candidate.privilege)) \
                  ) THEN \
                 RAISE EXCEPTION USING ERRCODE = '42501', \
                   MESSAGE = 'authoring-effective-privilege-out-of-bounds:test-reports'; \
               END IF; \
             END $effective_acl$;"
    )
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

#[derive(Debug)]
struct PreparedFlowArtifact {
    graph_json: String,
    entry_kind: wamn_flow::EntryKind,
    artifact: wamn_catalog::Artifact,
    execution_bundle_hash: Option<String>,
}

/// The flow ids two release-membership documents both name at DIFFERENT content.
///
/// Release membership is grow-only by owner rulings R1/R5 (wamn-0h0g.15.159): the
/// database now COMMITS an append to a sealed release, so two representations of
/// one release need not carry the same members and a differing member COUNT is no
/// longer evidence of anything. A flow id both sides carry at a different
/// flow-version or artifact hash is the one divergence no append can explain, and
/// it is what `catalog-release-content-conflict` names from here on. A member only
/// one side carries is the permitted append and is not reported.
///
/// Both arguments are the [`wamn_schema_control::sql::select_release_members_sql`]
/// shape — an array of `{flow-id, flow-version, artifact-hash}` — and both are
/// produced by this process or by that statement, never by a caller, so an entry
/// that is not an object with a string `flow-id` is skipped rather than defended
/// against.
pub(crate) fn divergent_release_members(
    stored: &serde_json::Value,
    requested: &serde_json::Value,
) -> Vec<String> {
    let requested = index_release_members(requested);
    index_release_members(stored)
        .into_iter()
        .filter(|(flow_id, member)| requested.get(flow_id).is_some_and(|other| other != member))
        .map(|(flow_id, _)| flow_id)
        .collect()
}

/// Index one membership document by flow id, keeping the whole member as the
/// value: the ids are the keys, so comparing members compares everything else.
fn index_release_members(
    members: &serde_json::Value,
) -> BTreeMap<String, &serde_json::Map<String, serde_json::Value>> {
    members
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|member| {
            let member = member.as_object()?;
            let flow_id = member.get("flow-id")?.as_str()?;
            Some((flow_id.to_string(), member))
        })
        .collect()
}

fn release_members(
    artifacts: &[PreparedFlowArtifact],
) -> anyhow::Result<Vec<wamn_schema_control::ReleaseFlow>> {
    wamn_schema_control::canonical_release_flows(
        artifacts
            .iter()
            .map(|prepared| {
                let id = prepared.artifact.identity().id();
                let flow_version = i32::try_from(id.flow_version()).context("flow version")?;
                prepared.execution_bundle_hash.as_ref().ok_or_else(|| {
                    wamn_schema_control::PublicationError::MissingRootPlan {
                        flow_id: id.flow_id().to_string(),
                        flow_version,
                    }
                })?;
                Ok(wamn_schema_control::ReleaseFlow {
                    flow_id: id.flow_id().to_string(),
                    flow_version,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    )
    .map_err(anyhow::Error::new)
}

fn execution_bundle_hash_for<'a>(
    member: &wamn_schema_control::ReleaseFlow,
    artifacts: &'a [PreparedFlowArtifact],
) -> &'a str {
    artifacts
        .iter()
        .find(|prepared| {
            let id = prepared.artifact.identity().id();
            id.flow_id() == member.flow_id
                && i32::try_from(id.flow_version()).ok() == Some(member.flow_version)
        })
        .and_then(|prepared| prepared.execution_bundle_hash.as_deref())
        .expect("release members were built only after every root plan was present")
}

#[derive(Debug)]
struct PreparedPublicationFlows {
    artifacts: Vec<PreparedFlowArtifact>,
    exposure: Option<(
        wamn_schema_control::ExposureRelease,
        Vec<wamn_schema_control::ResolvedAttachment>,
    )>,
}

fn prepare_publication_flows(
    args: &PublishCatalogArgs,
) -> anyhow::Result<PreparedPublicationFlows> {
    let mut artifacts = Vec::new();
    for path in &args.flow {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read flow {}", path.display()))?;
        let prepared = prepare_flow_artifact(&args.tenant, &source)
            .with_context(|| format!("prepare flow {}", path.display()))?;
        let id = prepared.artifact.identity().id();
        println!(
            "prepared immutable flow artifact {} v{}",
            id.flow_id(),
            id.flow_version()
        );
        artifacts.push(prepared);
    }
    let exposure = if let Some(path) = &args.exposure {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read exposure {}", path.display()))?;
        let authored: wamn_schema_control::ExposureRelease =
            serde_json::from_str(&source).context("parse exposure")?;
        let flows = artifacts
            .iter()
            .map(|prepared| {
                let id = prepared.artifact.identity().id();
                wamn_schema_control::FlowExposure {
                    flow_id: id.flow_id(),
                    entry_kind: prepared.entry_kind,
                    artifact_hash: prepared.artifact.identity().artifact_hash().as_str(),
                }
            })
            .collect::<Vec<_>>();
        let resolved =
            wamn_schema_control::resolve_exposure(&authored, &flows).map_err(anyhow::Error::new)?;
        Some((authored, resolved))
    } else {
        None
    };
    Ok(PreparedPublicationFlows {
        artifacts,
        exposure,
    })
}

/// Resolve standard and verified supplied-node interfaces, then build the
/// canonical CF-DEF-ID artifact without mutating storage.
fn prepare_flow_artifact(tenant: &str, graph_json: &str) -> anyhow::Result<PreparedFlowArtifact> {
    let flow =
        wamn_flow::Flow::from_json(graph_json).map_err(|e| anyhow::anyhow!("flow parse: {e}"))?;
    let mut interfaces = Vec::new();
    let node_types: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    for node_type in node_types {
        let Some(interface) = wamn_standard_nodes::describe_interface(node_type) else {
            anyhow::bail!("unknown node type {node_type:?}");
        };
        interfaces.push(interface.clone());
    }
    let artifact = wamn_catalog::Artifact::new(tenant, &flow, interfaces)
        .map_err(|error| anyhow::anyhow!("immutable artifact: {error}"))?;
    Ok(PreparedFlowArtifact {
        graph_json: graph_json.to_string(),
        entry_kind: flow
            .nodes
            .iter()
            .find_map(wamn_flow::Node::entry_kind)
            .expect("validated flow has one entry"),
        artifact,
        execution_bundle_hash: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_EXECUTION_BUNDLE_BYTES: &[u8] = br#"{}"#;

    fn with_test_execution_bundle(mut prepared: PreparedFlowArtifact) -> PreparedFlowArtifact {
        prepared.execution_bundle_hash = Some(wamn_catalog::execution_bundle_hash(
            TEST_EXECUTION_BUNDLE_BYTES,
        ));
        prepared
    }

    async fn seed_test_execution_bundle(client: &tokio_postgres::Client, tenant: &str) {
        let hash = wamn_catalog::execution_bundle_hash(TEST_EXECUTION_BUNDLE_BYTES);
        client
            .execute(
                wamn_schema_control::sql::insert_execution_bundle_sql(),
                &[&tenant, &hash, &TEST_EXECUTION_BUNDLE_BYTES],
            )
            .await
            .expect("seed test execution bundle");
    }

    #[test]
    fn authoring_test_orchestration_provisioning_is_fresh_and_privilege_closed() {
        let schema = BareSchemaName::new("project_run").expect("valid test schema");
        let probe = authoring_run_storage_probe_sql(&schema);
        assert!(!probe.contains("authoring_test_sets"));
        assert!(probe.contains("project_run.authoring_test_run_reservations"));
        assert!(probe.contains("project_run.authoring_test_case_runs"));
        assert!(probe.contains("project_run.authoring_test_reports"));

        let privileges = authoring_run_privileges_sql(&schema);
        assert!(!privileges.contains("authoring_test_sets"));
        assert!(
            privileges.contains("REVOKE ALL PRIVILEGES ON \"project_run\".authoring_test_reports")
        );
        assert!(
            privileges.contains("GRANT SELECT, INSERT ON \"project_run\".authoring_test_reports")
        );
        assert!(privileges.contains("'wamn_app', '\"project_run\".authoring_test_reports'"));
        assert!(privileges.contains("pg_catalog.has_any_column_privilege"));
        assert!(privileges.contains("authoring-effective-privilege-out-of-bounds:test-reports"));
        assert!(
            !privileges
                .contains("GRANT SELECT, INSERT, UPDATE ON \"project_run\".authoring_test_reports")
        );
    }

    #[test]
    fn legacy_publication_without_validated_plan_returns_missing_root_plan() {
        let (_, graph) = release_fixture("missing-plan");
        let prepared = prepare_flow_artifact("tenant-a", &graph).expect("prepare flow");

        let error = release_members(&[prepared]).expect_err("legacy flow must lack a root plan");
        assert!(matches!(
            error.downcast_ref::<wamn_schema_control::PublicationError>(),
            Some(wamn_schema_control::PublicationError::MissingRootPlan {
                flow_id,
                flow_version: 1,
            }) if flow_id == "flow-missing-plan"
        ));
    }

    fn release_fixture(suffix: &str) -> (wamn_schema_model::Catalog, String) {
        let catalog_json = format!(
            r#"{{"schema-version":"0.1","catalog-id":"release-{suffix}","version":1,"entities":[]}}"#
        );
        let catalog = wamn_schema_model::Catalog::from_json(&catalog_json).unwrap();
        let graph = format!(
            r#"{{
              "schema-version":"0.1","flow-id":"flow-{suffix}","version":1,
              "nodes":[
                {{"id":"request","type":"request","config":{{"input-schema":{{
                  "$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"
                }}}}}},
                {{"id":"shape","type":"transform","config":{{"expression":"@"}}}},
                {{"id":"respond","type":"respond","config":{{"status":200}}}}
              ],
              "edges":[
                {{"from":"request","to":"shape"}},
                {{"from":"shape","to":"respond"}}
              ]
            }}"#
        );
        (catalog, graph)
    }

    async fn persisted_release_bytes(
        client: &tokio_postgres::Client,
        tenant: &str,
        catalog_id: &str,
    ) -> Vec<u8> {
        let row = client
            .query_one(
                "SELECT \
                   COALESCE((SELECT jsonb_agg(to_jsonb(a) - 'created_at' ORDER BY flow_id, flow_version)::text \
                     FROM catalog.flow_artifacts a WHERE tenant_id = $1), '[]'), \
                   COALESCE((SELECT jsonb_agg(jsonb_build_array( \
                       flow_id, flow_version, execution_bundle_hash) \
                     ORDER BY flow_id)::text FROM catalog.release_flows \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = 1), '[]'), \
                   (SELECT jsonb_build_array(from_version, to_version, \
                     statement_count, destructive, checksum)::text \
                     FROM catalog.schema_migrations WHERE tenant_id = $1 \
                       AND catalog_id = $2 AND environment = 'dev' AND to_version = 1), \
                   (SELECT applied_catalog_version::text FROM catalog.catalog_heads \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = 'dev')",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap();
        format!(
            "{:?}",
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
            )
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn missing_root_plan_rolls_back_without_publication_writes() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        ensure_wamn_app_role(&client).await.unwrap();
        ensure_catalog_storage(&client).await.unwrap();
        let tenant = format!("missing-root-plan-{}", std::process::id());
        let (catalog, graph) = release_fixture("missing-root-plan");
        let document = catalog.to_json();

        client.batch_execute("BEGIN").await.unwrap();
        let refusal = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &BareSchemaName::new("pg_temp").unwrap(),
            vec![prepare_flow_artifact(&tenant, &graph).unwrap()],
            None,
            &document,
            None,
        )
        .await;
        let error = finish_publication_transaction(&client, refusal)
            .await
            .expect_err("legacy publication must refuse a missing root plan");
        assert!(format!("{error:#}").contains("missing-root-plan"));

        let counts = client
            .query_one(
                "SELECT \
                   (SELECT count(*) FROM catalog.flow_artifacts WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.release_manifests WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.release_flows WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.schema_migrations WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.catalog_heads WHERE tenant_id = $1)",
                &[&tenant],
            )
            .await
            .unwrap();
        for column in 0..5 {
            assert_eq!(counts.get::<_, i64>(column), 0);
        }
        drop(client);
        let _ = connection_task.await;
    }

    #[tokio::test]
    async fn every_failed_publication_boundary_rolls_back_and_same_connection_retries_identically()
    {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        ensure_wamn_app_role(&client).await.unwrap();
        ensure_catalog_storage(&client).await.unwrap();
        let run_schema = BareSchemaName::new("cf_release_retry_probe").unwrap();
        for (index, stage) in [
            "after-artifacts",
            "after-journal",
            "after-members",
            "before-head",
        ]
        .into_iter()
        .enumerate()
        {
            let suffix = format!("{index}");
            let tenant = format!("release-retry-{index}");
            let (catalog, graph) = release_fixture(&suffix);
            let document = catalog.to_json();
            seed_test_execution_bundle(&client, &tenant).await;

            client.batch_execute("BEGIN").await.unwrap();
            client
                .batch_execute(&format!(
                    "SET LOCAL wamn.test.publication_fault = '{stage}'"
                ))
                .await
                .unwrap();
            let failed_writes = publish_release(
                &client,
                &catalog,
                &tenant,
                "dev",
                &run_schema,
                vec![with_test_execution_bundle(
                    prepare_flow_artifact(&tenant, &graph).unwrap(),
                )],
                None,
                &document,
                None,
            )
            .await;
            let error = finish_publication_transaction(&client, failed_writes)
                .await
                .expect_err("injected publication failure");
            assert!(format!("{error:#}").contains(&format!("injected-publication-fault-{stage}")));
            let counts = client
                .query_one(
                    "SELECT \
                       (SELECT count(*) FROM catalog.flow_artifacts WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.release_manifests WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.release_flows WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.schema_migrations WHERE tenant_id = $1), \
                       (SELECT count(*) FROM catalog.catalog_heads WHERE tenant_id = $1)",
                    &[&tenant],
                )
                .await
                .expect("same connection is usable after rollback");
            for column in 0..5 {
                assert_eq!(
                    counts.get::<_, i64>(column),
                    0,
                    "{stage} left column {column}"
                );
            }

            client.batch_execute("BEGIN").await.unwrap();
            let first_retry = publish_release(
                &client,
                &catalog,
                &tenant,
                "dev",
                &run_schema,
                vec![with_test_execution_bundle(
                    prepare_flow_artifact(&tenant, &graph).unwrap(),
                )],
                None,
                &document,
                None,
            )
            .await;
            finish_publication_transaction(&client, first_retry)
                .await
                .expect("first retry commits");
            let first = persisted_release_bytes(&client, &tenant, &catalog.catalog_id).await;

            client.batch_execute("BEGIN").await.unwrap();
            let same_content_retry = publish_release(
                &client,
                &catalog,
                &tenant,
                "dev",
                &run_schema,
                vec![with_test_execution_bundle(
                    prepare_flow_artifact(&tenant, &graph).unwrap(),
                )],
                None,
                &document,
                Some(1),
            )
            .await;
            finish_publication_transaction(&client, same_content_retry)
                .await
                .expect("same-content retry commits");
            assert_eq!(
                persisted_release_bytes(&client, &tenant, &catalog.catalog_id).await,
                first
            );
        }

        drop(client);
        let _ = connection_task.await;
    }

    #[tokio::test]
    async fn legacy_applied_catalog_initializes_head_and_refuses_its_nonterminal_run() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        ensure_wamn_app_role(&client).await.unwrap();
        ensure_catalog_storage(&client).await.unwrap();
        let suffix = std::process::id();
        let tenant = format!("legacy-guard-{suffix}");
        let catalog_id = format!("legacy-guard-{suffix}");
        let current_document = format!(
            r#"{{"schema-version":"0.1","catalog-id":"{catalog_id}","version":1,"entities":[]}}"#
        );
        client
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state, document) \
                 VALUES ($1, $2, 1, 'dev', '0.1', 'applied', $3::text::jsonb)",
                &[&tenant, &catalog_id, &current_document],
            )
            .await
            .unwrap();
        client
            .batch_execute(
                "CREATE TEMP TABLE runs (\
                   tenant_id text, catalog_id text, catalog_version int, status text)",
            )
            .await
            .unwrap();
        client
            .execute(
                "INSERT INTO runs VALUES ($1, $2, 1, 'running')",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap();
        let target_document = format!(
            r#"{{"schema-version":"0.1","catalog-id":"{catalog_id}","version":2,"entities":[]}}"#
        );
        let target = wamn_schema_model::Catalog::from_json(&target_document).unwrap();
        let expected_base: Option<i32> = client
            .query_one(
                wamn_schema_control::sql::select_publication_base_sql(),
                &[&tenant, &catalog_id, &"dev"],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(expected_base, Some(1));

        client.batch_execute("BEGIN").await.unwrap();
        let refusal = publish_release(
            &client,
            &target,
            &tenant,
            "dev",
            &BareSchemaName::new("pg_temp").unwrap(),
            vec![],
            None,
            &target_document,
            expected_base,
        )
        .await;
        let error = finish_publication_transaction(&client, refusal)
            .await
            .expect_err("legacy pinned run must refuse replacement");
        assert!(format!("{error:#}").contains("catalog-release-has-nonterminal-runs"));
        let head_present: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM catalog.catalog_heads \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = 'dev')",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!head_present, "rolled-back head initialization leaked");
        client
            .execute(
                "DELETE FROM catalog.catalogs WHERE tenant_id = $1 AND catalog_id = $2",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap();
        drop(client);
        let _ = connection_task.await;
    }

    #[tokio::test]
    async fn invalid_schema_is_rejected_before_catalog_io_or_admin_connect() {
        let missing =
            std::env::temp_dir().join("invalid-schema-must-not-read-catalog-5wd1-29.json");
        let error = run(PublishCatalogArgs {
            catalog: missing,
            admin_database_url: Some("postgresql://invalid.invalid/never".to_string()),
            tenant: "t1".to_string(),
            project_config: None,
            schema: format!("s{}", "a".repeat(63)),
            provision: true,
            runstate: true,
            seed_dataset: None,
            flow: vec![],
            exposure: None,
            skip_reconcile_replica_identity: false,
        })
        .await
        .expect_err("overlong schema must fail before any effect");
        let message = format!("{error:#}");
        assert!(
            message.contains("identifier exceeds PostgreSQL's 63-byte limit"),
            "{message}"
        );
        assert!(!message.contains("read catalog"), "{message}");
        assert!(!message.contains("admin connect"), "{message}");
    }

    /// The wamn-0h0g.8.18 cutover: a fully valid invocation refuses, and refuses
    /// before it opens a file or dials a server.
    ///
    /// Every path argument names a file that does not exist and the admin URL is
    /// unroutable, so any I/O this verb performed would surface as its own
    /// context literal — `read catalog`, `read project config`, `read seed
    /// dataset`, or `admin connect` — instead of the refusal. An unroutable host
    /// also means a connection attempt would stall rather than return promptly.
    #[tokio::test]
    async fn a_valid_publication_refuses_before_any_file_or_connection() {
        let missing = |name: &str| std::env::temp_dir().join(name);
        let error = run(PublishCatalogArgs {
            catalog: missing("control-publish-closed-catalog-8-18.json"),
            admin_database_url: Some("postgresql://invalid.invalid/never".to_string()),
            tenant: "t1".to_string(),
            project_config: Some(missing("control-publish-closed-config-8-18.json")),
            schema: "wamn_app".to_string(),
            provision: true,
            runstate: true,
            seed_dataset: Some(missing("control-publish-closed-seed-8-18.json")),
            flow: vec![missing("control-publish-closed-flow-8-18.json")],
            exposure: Some(missing("control-publish-closed-exposure-8-18.json")),
            skip_reconcile_replica_identity: false,
        })
        .await
        .expect_err("the closed definition-publish path must refuse");
        let message = format!("{error:#}");
        assert_eq!(message, crate::CONTROL_DEFINITION_PUBLISH_REFUSAL);
        for io in [
            "read catalog",
            "read project config",
            "read seed dataset",
            "read flow",
            "read exposure",
            "admin connect",
        ] {
            assert!(!message.contains(io), "publish reached {io}: {message}");
        }
        // The refusal names the replacement path, not the superseded pair the
        // pre-amendment ruling wrote down.
        assert!(crate::CONTROL_DEFINITION_PUBLISH_REFUSAL.ends_with("wamn-0h0g.15.14"));
        assert!(!crate::CONTROL_DEFINITION_PUBLISH_REFUSAL.contains("8.19"));
        assert!(!crate::CONTROL_DEFINITION_PUBLISH_REFUSAL.contains("8.22"));
    }

    #[test]
    fn callable_poc_config_is_exact_and_boolean() {
        assert!(parse_raw_sql_config(r#"{"raw_sql_enabled":true}"#).unwrap());
        assert!(!parse_raw_sql_config(r#"{"raw_sql_enabled":false}"#).unwrap());
        for source in [
            "{}",
            r#"{"raw_sql_enabled":"true"}"#,
            r#"{"raw_sql_enabled":true,"other":false}"#,
            "not-json",
        ] {
            assert!(parse_raw_sql_config(source).is_err(), "{source}");
        }
    }

    #[test]
    fn journal_base_is_preserved_only_for_a_true_same_target_retry() {
        assert!(!publication_is_same_target_retry(None, 1).unwrap());
        assert!(!publication_is_same_target_retry(Some(1), 2).unwrap());
        assert!(publication_is_same_target_retry(Some(2), 2).unwrap());
        let regression = publication_is_same_target_retry(Some(3), 2).unwrap_err();
        assert!(format!("{regression:#}").contains("catalog-release-version-regression"));
        let source = include_str!("publish_catalog.rs");
        assert!(source.contains("from_version IS NOT DISTINCT FROM $4"));
        assert!(source.contains("verify retried release publication journal"));
    }

    fn member(flow_id: &str, flow_version: i32, artifact_hash: &str) -> serde_json::Value {
        serde_json::json!({
            "flow-id": flow_id,
            "flow-version": flow_version,
            "artifact-hash": artifact_hash,
        })
    }

    #[test]
    fn only_a_member_both_sides_carry_at_different_content_is_a_release_conflict() {
        let one = serde_json::json!([member("a", 1, "ah-a")]);
        let grown = serde_json::json!([member("a", 1, "ah-a"), member("b", 1, "ah-b")]);

        // wamn-0h0g.15.163: the two directions of a permitted grow-only append.
        assert!(divergent_release_members(&one, &one).is_empty());
        assert!(divergent_release_members(&grown, &one).is_empty());
        assert!(divergent_release_members(&one, &grown).is_empty());
        assert!(divergent_release_members(&serde_json::json!([]), &grown).is_empty());

        // A shared flow id at different content is the one divergence no append
        // explains — in either field, and named individually.
        let rehashed = serde_json::json!([member("a", 1, "ah-other"), member("b", 1, "ah-b")]);
        assert_eq!(divergent_release_members(&grown, &rehashed), vec!["a"]);
        let reversioned = serde_json::json!([member("a", 2, "ah-a")]);
        assert_eq!(divergent_release_members(&grown, &reversioned), vec!["a"]);
        let both = serde_json::json!([member("a", 2, "ah-a"), member("b", 1, "ah-other")]);
        assert_eq!(divergent_release_members(&grown, &both), vec!["a", "b"]);
    }

    /// wamn-0h0g.15.163 against live PostgreSQL: a release the database let grow
    /// republishes, and a changed member still refuses.
    ///
    /// The tenant carries a nanosecond suffix because `catalog.release_flows`
    /// refuses DELETE through its immutability trigger, so a hermetic preamble
    /// cannot reset the coordinate — only a coordinate nothing has touched is
    /// clean.
    #[tokio::test]
    async fn a_release_grown_by_a_later_append_republishes_and_still_refuses_a_changed_member() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        ensure_wamn_app_role(&client).await.unwrap();
        ensure_catalog_storage(&client).await.unwrap();

        let tenant = format!(
            "grown-release-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after the epoch")
                .as_nanos()
        );
        let run_schema = BareSchemaName::new("cf_grown_release_probe").unwrap();
        let (catalog, sealed_graph) = release_fixture("grown");
        let document = catalog.to_json();
        seed_test_execution_bundle(&client, &tenant).await;

        // 1. Seal the release with its single member.
        client.batch_execute("BEGIN").await.unwrap();
        let sealed = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &run_schema,
            vec![with_test_execution_bundle(
                prepare_flow_artifact(&tenant, &sealed_graph).unwrap(),
            )],
            None,
            &document,
            None,
        )
        .await;
        finish_publication_transaction(&client, sealed)
            .await
            .expect("seal the one-member release");

        // 2. A tested publish appends a SECOND member in a LATER transaction —
        //    the append wamn-0h0g.15.159 proved the database now COMMITS.
        client
            .execute(
                "INSERT INTO catalog.flow_artifacts \
                   (tenant_id, flow_id, flow_version, schema_version, graph_json, \
                    graph_hash, artifact_hash) \
                 VALUES ($1, 'flow-appended', 1, '0.1', '{}'::jsonb, 'gh-appended', \
                         'ah-appended')",
                &[&tenant],
            )
            .await
            .expect("register the appended member's artifact");
        client
            .execute(
                wamn_schema_control::sql::insert_release_flow_sql(),
                &[
                    &tenant,
                    &catalog.catalog_id,
                    &1_i32,
                    &"flow-appended",
                    &1_i32,
                    &wamn_catalog::execution_bundle_hash(TEST_EXECUTION_BUNDLE_BYTES),
                ],
            )
            .await
            .expect("append a member to the sealed release");
        let grown: String = client
            .query_one(
                wamn_schema_control::sql::select_release_members_sql(),
                &[&tenant, &catalog.catalog_id, &1_i32],
            )
            .await
            .unwrap()
            .get(0);
        assert!(grown.contains("flow-appended"), "{grown}");

        // 3. Republishing the ORIGINAL member set is ADMITTED, not refused.
        client.batch_execute("BEGIN").await.unwrap();
        let republish = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &run_schema,
            vec![with_test_execution_bundle(
                prepare_flow_artifact(&tenant, &sealed_graph).unwrap(),
            )],
            None,
            &document,
            Some(1),
        )
        .await;
        finish_publication_transaction(&client, republish)
            .await
            .expect("a republish of a grown release is admitted");

        // 4. The same flow id at a different artifact hash is still refused.
        let changed_graph = sealed_graph.replace(r#""status":200"#, r#""status":201"#);
        assert_ne!(changed_graph, sealed_graph);
        client.batch_execute("BEGIN").await.unwrap();
        let conflict = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &run_schema,
            vec![with_test_execution_bundle(
                prepare_flow_artifact(&tenant, &changed_graph).unwrap(),
            )],
            None,
            &document,
            Some(1),
        )
        .await;
        let error = finish_publication_transaction(&client, conflict)
            .await
            .expect_err("a changed member must still refuse");
        assert!(
            format!("{error:#}").contains("catalog-release-content-conflict"),
            "{error:#}"
        );

        drop(client);
        let _ = connection_task.await;
    }
}
