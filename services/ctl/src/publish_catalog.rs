//! The `publish-catalog` subcommand: write a project's catalog snapshot into the
//! `wamn_catalog` table the api-gateway component (4.1) reads at startup.
//!
//! In production the schema-designer→gateway seam writes this row whenever a
//! catalog version is applied/promoted (3.4); 4.1b provides the mechanism as a
//! reusable, idempotent host subcommand so a per-project gateway has a snapshot
//! to serve. It reads a catalog JSON (the applied catalog for a project),
//! `Catalog::to_json`s the canonical document, and UPSERTs it under the project's
//! tenant — connecting as a **superuser** so it bypasses the snapshot table's RLS
//! `WITH CHECK` and the runtime role's SELECT-only grant.
//!
//! `--provision` additionally stands up the schema and the 3.2 tenant floor (the
//! entity tables) when they are absent — used by the in-cluster `apiproof` gate
//! to give the deployed gateway real data to serve (the demo-row seeding rides
//! the gates-side `wamn-gates publish-catalog --seed` wrapper; the prod tool
//! carries no fixture content). Everything is **additive**: the schema is created
//! `IF NOT EXISTS`, the floor is applied only when missing, and no existing object
//! is ever dropped or altered (the shared-cluster guardrail).
//!
//! POC-F1 extended this into the one project-provisioning tool: `--runstate`
//! applies the run-state storage (`deploy/sql/run-state.sql`: runs/node_runs),
//! the flow registry (`deploy/sql/flows.sql`), and the 11.2 flow test-suite
//! tables (`deploy/sql/flow-tests.sql`: test_suites/test_cases) into the project
//! schema — the canonical deploy files, embedded at compile time and rewritten
//! from `wamn_run` to the target schema — when their tables are absent;
//! `--seed-dataset` compiles a wamn-schema-compiler (3.6) dataset against the catalog and
//! applies it (deterministic ids, `ON CONFLICT DO NOTHING` — idempotent); and
//! `--flow` resolves standard-node interfaces, constructs canonical CF-DEF-ID
//! artifacts, and publishes artifacts + release membership + head atomically.
//! It does not write the retired mutable `flows.active` publication path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
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

    /// Tenant the snapshot is published under (the gateway's `app.tenant` claim).
    #[arg(long)]
    pub tenant: String,

    /// Optional callable-flow POC project configuration. The accepted shape is
    /// exactly `{"raw_sql_enabled": <boolean>}`; other environments omit this
    /// fixture and retain the fail-closed default.
    #[arg(long)]
    pub project_config: Option<PathBuf>,

    /// Schema the `wamn_catalog` table (and, with `--provision`, the entity
    /// tables) live in; the gateway reaches them via the host-injected
    /// `search_path`.
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

    /// Custom-node publication descriptor JSON. Repeat for each supplied node.
    /// Each descriptor pins a node type, component path, manifest path, and the
    /// expected sha256 digest of the exact component bytes.
    #[arg(long = "custom-node")]
    pub custom_node: Vec<PathBuf>,

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

pub async fn run(args: PublishCatalogArgs) -> anyhow::Result<()> {
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid schema name {:?}", args.schema))?;

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
    let supplied = load_supplied_components(&args.custom_node)?;
    let prepared_flows = prepare_publication_flows(&args, &supplied)?;

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
                .sql(wamn_schema_compiler::Confirmation::None)
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
        // 11.2 test-suite tables (FK to flows, so AFTER ensure_flow_registry).
        if ensure_flow_tests(client, schema).await? {
            println!("applied flow test-suite tables (test_suites/test_cases) in schema {schema}");
        } else {
            println!("flow test-suite tables already present in schema {schema}; skipping");
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

pub(crate) async fn ensure_catalog_storage(client: &tokio_postgres::Client) -> anyhow::Result<()> {
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
                               AND column_name = 'interface_bundle_json')",
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
        release_row.get::<_, bool>(10),
    ];
    if release_objects.iter().all(|present| *present) {
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
    Ok(())
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
    let members = wamn_schema_control::canonical_release_flows(
        artifacts
            .iter()
            .map(|prepared| {
                let id = prepared.artifact.identity().id();
                Ok(wamn_schema_control::ReleaseFlow {
                    flow_id: id.flow_id().to_string(),
                    flow_version: i32::try_from(id.flow_version()).context("flow version")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    )
    .map_err(anyhow::Error::new)?;
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
        if let Some(existing) = client
            .query_opt(
                "SELECT members_json::text FROM catalog.release_manifests \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                &[&tenant, &cat.catalog_id, &catalog_version],
            )
            .await
            .context("preflight existing release membership")?
        {
            let existing: String = existing.get(0);
            let existing: serde_json::Value =
                serde_json::from_str(&existing).context("parse stored release manifest")?;
            let requested: serde_json::Value =
                serde_json::from_str(&release_manifest).expect("prepared manifest is JSON");
            anyhow::ensure!(existing == requested, "catalog-release-content-conflict");
        }

        for prepared in &artifacts {
            let artifact = &prepared.artifact;
            let id = artifact.identity().id();
            let flow_version = i32::try_from(id.flow_version()).context("flow version")?;
            let interfaces = std::str::from_utf8(artifact.interface_bundle().canonical_bytes())
                .context("canonical interface bundle is UTF-8")?;
            let interface_bundle_hash = artifact.interface_bundle().hash();
            let component_digests = serde_json::to_string(artifact.supplied_components())
                .context("serialize digests")?;
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
                        &interfaces,
                        &interface_bundle_hash,
                        &component_digests.as_str(),
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
                       AND confirmation = 'none' AND statement_count = 0 \
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
                       AND confirmation = 'none' AND statement_count = 0 \
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
                &[
                    &tenant,
                    &cat.catalog_id,
                    &catalog_version,
                    &release_manifest.as_str(),
                ],
            )
            .await
            .context("seal release membership")?;
        for member in &members {
            client
                .execute(
                    wamn_schema_control::sql::insert_release_flow_sql(),
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &catalog_version,
                        &member.flow_id,
                        &member.flow_version,
                    ],
                )
                .await
                .with_context(|| format!("publish flow {:?}", member.flow_id))?;
            let exact: bool = client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM catalog.release_flows \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
                       AND flow_id = $4 AND flow_version = $5)",
                    &[
                        &tenant,
                        &cat.catalog_id,
                        &catalog_version,
                        &member.flow_id,
                        &member.flow_version,
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
        let stored_members: i64 = client
            .query_one(
                "SELECT count(*) FROM catalog.release_flows \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                &[&tenant, &cat.catalog_id, &catalog_version],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            stored_members == i64::try_from(members.len()).unwrap_or(i64::MAX),
            "catalog-release-content-conflict"
        );
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
    if let Err(error) = writes {
        return Err(error);
    }
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
/// `cat`'s catalog id across ALL tenants (the caller connects as a superuser, so
/// RLS is bypassed) and refuses when any references an entity `cat` does not
/// keep, naming every orphan (the pure decision `wamn_schema_control::check_registration_orphans`).
/// A DB with no `catalog.event_registrations` table (a project not yet
/// registration-provisioned) has nothing to orphan, so the probe returns a clean
/// pass. Read-only: a refusal mutates nothing.
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
        return Ok(());
    }
    let rows = client
        .query(
            &wamn_schema_control::sql::select_registrations_for_catalog_sql(),
            &[&cat.catalog_id],
        )
        .await
        .context("read event registrations for the D24 orphan guard")?;
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
// f1bench gate so the bench provisions through the same code path production
// provisioning uses.
// ---------------------------------------------------------------------------

/// Ensure the non-superuser runtime role exists (pre-created in production;
/// bare gate/throwaway databases lack it). Shared with `reconcile-run-plane`,
/// whose sections GRANT to it.
pub(crate) async fn ensure_wamn_app_role(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("ensure wamn_app role")
}

/// Apply `deploy/sql/run-state.sql` (runs + node_runs) into `schema` when its
/// `runs` table is absent. Returns whether it applied (false = already there).
pub async fn ensure_runstate(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<bool> {
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

/// Apply `deploy/sql/flow-tests.sql` (the 11.2 test-suite tables) into `schema`
/// when its `test_suites` table is absent. Returns whether it applied. The FK to
/// `flows` means [`ensure_flow_registry`] MUST have run first — publish calls
/// them in that order below.
pub async fn ensure_flow_tests(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
) -> anyhow::Result<bool> {
    if table_exists(client, schema, "test_suites").await? {
        return Ok(false);
    }
    let ddl = rewrite_schema(include_str!("../../../deploy/sql/flow-tests.sql"), schema);
    client
        .batch_execute(&ddl)
        .await
        .context("apply flow test-suite tables")?;
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
    plan.sql(wamn_schema_compiler::Confirmation::None)
        .map_err(|e| anyhow::anyhow!("seed sql: {e}"))
}

#[derive(Debug)]
struct PreparedFlowArtifact {
    graph_json: String,
    entry_kind: wamn_flow::EntryKind,
    artifact: wamn_catalog::Artifact,
    supplied_node_types: BTreeSet<String>,
}

#[derive(Debug)]
struct PreparedPublicationFlows {
    artifacts: Vec<PreparedFlowArtifact>,
    exposure: Option<(
        wamn_schema_control::ExposureRelease,
        Vec<wamn_schema_control::ResolvedAttachment>,
    )>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SuppliedComponentDescriptor {
    node_type: String,
    component: PathBuf,
    manifest: PathBuf,
    component_digest: String,
}

fn descriptor_path(descriptor: &Path, supplied: &Path) -> PathBuf {
    if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        descriptor
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(supplied)
    }
}

fn component_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity("sha256:".len() + digest.len() * 2);
    result.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut result, "{byte:02x}").expect("writing to a string is infallible");
    }
    result
}

/// Read and verify all explicit custom-node publication inputs before any
/// database connection or publication mutation.
fn load_supplied_components(
    descriptors: &[PathBuf],
) -> anyhow::Result<BTreeMap<String, wamn_catalog::NodeImplementation>> {
    let mut supplied = BTreeMap::new();
    for descriptor_pathname in descriptors {
        let source = std::fs::read_to_string(descriptor_pathname).with_context(|| {
            format!(
                "read custom-node descriptor {}",
                descriptor_pathname.display()
            )
        })?;
        let descriptor: SuppliedComponentDescriptor =
            serde_json::from_str(&source).with_context(|| {
                format!(
                    "parse custom-node descriptor {}",
                    descriptor_pathname.display()
                )
            })?;
        let component_path = descriptor_path(descriptor_pathname, &descriptor.component);
        let manifest_path = descriptor_path(descriptor_pathname, &descriptor.manifest);
        let bytes = std::fs::read(&component_path)
            .with_context(|| format!("read custom-node component {}", component_path.display()))?;
        let actual_digest = component_digest(&bytes);
        anyhow::ensure!(
            descriptor.component_digest == actual_digest,
            "custom-node component digest mismatch for {:?}: expected {}, got {}",
            descriptor.node_type,
            descriptor.component_digest,
            actual_digest
        );
        let manifest_source = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read custom-node manifest {}", manifest_path.display()))?;
        let manifest = wamn_node_manifest::NodeManifest::from_json(&manifest_source)
            .with_context(|| format!("parse custom-node manifest {}", manifest_path.display()))?;
        anyhow::ensure!(
            manifest.node_type == descriptor.node_type,
            "custom-node manifest node-type mismatch: descriptor {:?}, manifest {:?}",
            descriptor.node_type,
            manifest.node_type
        );
        let resolved = manifest
            .resolved_component(actual_digest)
            .map_err(|issues| anyhow::anyhow!("resolve custom-node manifest: {issues:?}"))?;
        let implementation = wamn_catalog::NodeImplementation::from_resolved_component(resolved)
            .map_err(|error| anyhow::anyhow!("resolve custom-node implementation: {error}"))?;
        anyhow::ensure!(
            supplied
                .insert(descriptor.node_type.clone(), implementation)
                .is_none(),
            "duplicate custom-node implementation for {:?}",
            descriptor.node_type
        );
    }
    Ok(supplied)
}

fn prepare_publication_flows(
    args: &PublishCatalogArgs,
    supplied: &BTreeMap<String, wamn_catalog::NodeImplementation>,
) -> anyhow::Result<PreparedPublicationFlows> {
    let mut artifacts = Vec::new();
    let mut used_supplied = BTreeSet::new();
    for path in &args.flow {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read flow {}", path.display()))?;
        let prepared = prepare_flow_artifact(&args.tenant, &source, supplied)
            .with_context(|| format!("prepare flow {}", path.display()))?;
        used_supplied.extend(prepared.supplied_node_types.iter().cloned());
        let id = prepared.artifact.identity().id();
        println!(
            "prepared immutable flow artifact {} v{}",
            id.flow_id(),
            id.flow_version()
        );
        artifacts.push(prepared);
    }
    ensure_all_supplied_used(supplied, &used_supplied)?;
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

fn ensure_all_supplied_used(
    supplied: &BTreeMap<String, wamn_catalog::NodeImplementation>,
    used_supplied: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if let Some(node_type) = supplied
        .keys()
        .find(|node_type| !used_supplied.contains(*node_type))
    {
        anyhow::bail!("unknown supplied custom node {node_type:?}: no published graph declares it");
    }
    Ok(())
}

/// Resolve standard and verified supplied-node interfaces, then build the
/// canonical CF-DEF-ID artifact without mutating storage.
fn prepare_flow_artifact(
    tenant: &str,
    graph_json: &str,
    supplied: &BTreeMap<String, wamn_catalog::NodeImplementation>,
) -> anyhow::Result<PreparedFlowArtifact> {
    use wamn_node_manifest::{
        CapabilityClass, ConnectionRequirement, RecoveryClass, ResolvedNodeInterface,
        ResolvedPurity,
    };

    let flow =
        wamn_flow::Flow::from_json(graph_json).map_err(|e| anyhow::anyhow!("flow parse: {e}"))?;
    let mut implementations = Vec::new();
    let node_types: BTreeSet<_> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    for node_type in node_types {
        if matches!(node_type, "cron" | "event" | "fail" | "request" | "respond") {
            continue;
        }
        let Some(ports) = wamn_standard_nodes::completion_ports(node_type) else {
            if let Some(implementation) = supplied.get(node_type) {
                implementations.push(implementation.clone());
            }
            continue;
        };
        let replay_safe =
            wamn_standard_nodes::is_replay_safe(node_type).expect("standard node has semantics");
        let required = wamn_standard_nodes::required_capabilities(node_type)
            .expect("standard node has a capability descriptor");
        let mut capability_classes = required
            .iter()
            .map(|capability| match capability {
                wamn_standard_nodes::Capability::HttpEgress => CapabilityClass::Http,
                wamn_standard_nodes::Capability::Postgres
                | wamn_standard_nodes::Capability::RawSql => CapabilityClass::Postgres,
            })
            .collect::<Vec<_>>();
        capability_classes.sort();
        capability_classes.dedup();
        if capability_classes.is_empty() {
            capability_classes.push(CapabilityClass::Pure);
        }
        let connection_requirements = capability_classes
            .iter()
            .filter_map(|capability| match capability {
                CapabilityClass::Pure => None,
                CapabilityClass::Http => Some(ConnectionRequirement {
                    requirement_type: "http".to_string(),
                    contract: "wamn:connection/http@0.1.0".to_string(),
                }),
                CapabilityClass::Postgres => Some(ConnectionRequirement {
                    requirement_type: "postgres".to_string(),
                    contract: "wamn:connection/postgres@0.1.0".to_string(),
                }),
            })
            .collect();
        implementations.push(wamn_catalog::NodeImplementation::platform(
            ResolvedNodeInterface::new(
                node_type,
                "wamn:node@0.1.0",
                ports.iter().map(|port| (*port).to_string()).collect(),
                capability_classes,
                connection_requirements,
                if replay_safe {
                    ResolvedPurity::Pure
                } else {
                    ResolvedPurity::Effectful
                },
                if replay_safe {
                    RecoveryClass::Replay
                } else {
                    RecoveryClass::NeverReplay
                },
            ),
        ));
    }
    let supplied_node_types = implementations
        .iter()
        .filter(|implementation| implementation.component_digest().is_some())
        .map(|implementation| implementation.interface().node_type.clone())
        .collect();
    let artifact = wamn_catalog::Artifact::new(tenant, &flow, implementations)
        .map_err(|error| anyhow::anyhow!("immutable artifact: {error}"))?;
    Ok(PreparedFlowArtifact {
        graph_json: graph_json.to_string(),
        entry_kind: flow
            .nodes
            .iter()
            .find_map(wamn_flow::Node::entry_kind)
            .expect("validated flow has one entry"),
        artifact,
        supplied_node_types,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_graph(node_type: &str) -> String {
        format!(
            r#"{{
              "schema-version":"0.1","flow-id":"custom-flow","version":1,
              "nodes":[
                {{"id":"request","type":"request","config":{{"input-schema":{{
                  "$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"
                }}}}}},
                {{"id":"custom","type":"{node_type}"}},
                {{"id":"respond","type":"respond","config":{{"status":200}}}}
              ],
              "edges":[
                {{"from":"request","to":"custom"}},
                {{"from":"custom","to":"respond"}}
              ]
            }}"#
        )
    }

    fn manifest_json(node_type: &str, purity: Option<&str>) -> String {
        let purity = purity.map_or_else(String::new, |value| format!(r#","purity":"{value}""#));
        format!(
            r#"{{"schema-version":"0.1","node-type":"{node_type}","name":"Fixture","version":"0.1.0","contract":"0.1.0"{purity}}}"#
        )
    }

    fn custom_input_fixture(
        suffix: &str,
        descriptor_node_type: &str,
        manifest_node_type: &str,
        purity: Option<&str>,
        bytes: &[u8],
    ) -> (PathBuf, PathBuf, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "wamn-custom-publish-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let component = directory.join("node.wasm");
        let manifest = directory.join("manifest.json");
        let descriptor = directory.join("descriptor.json");
        std::fs::write(&component, bytes).unwrap();
        std::fs::write(&manifest, manifest_json(manifest_node_type, purity)).unwrap();
        std::fs::write(
            &descriptor,
            serde_json::json!({
                "node-type": descriptor_node_type,
                "component": "node.wasm",
                "manifest": "manifest.json",
                "component-digest": component_digest(bytes),
            })
            .to_string(),
        )
        .unwrap();
        (descriptor, component, manifest)
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
                   (SELECT members_json::text FROM catalog.release_manifests \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = 1), \
                   COALESCE((SELECT jsonb_agg(jsonb_build_array(flow_id, flow_version) \
                     ORDER BY flow_id)::text FROM catalog.release_flows \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = 1), '[]'), \
                   (SELECT jsonb_build_array(from_version, to_version, confirmation, \
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
                row.get::<_, String>(4),
            )
        )
        .into_bytes()
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
                vec![prepare_flow_artifact(&tenant, &graph, &BTreeMap::new()).unwrap()],
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
                vec![prepare_flow_artifact(&tenant, &graph, &BTreeMap::new()).unwrap()],
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
                vec![prepare_flow_artifact(&tenant, &graph, &BTreeMap::new()).unwrap()],
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
    async fn resolved_custom_component_rolls_back_at_publication_fault_and_retries() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        ensure_wamn_app_role(&client).await.unwrap();
        ensure_catalog_storage(&client).await.unwrap();
        let suffix = std::process::id();
        let tenant = format!("custom-fault-{suffix}");
        let catalog_json = format!(
            r#"{{"schema-version":"0.1","catalog-id":"custom-fault-{suffix}","version":1,"entities":[]}}"#
        );
        let catalog = wamn_schema_model::Catalog::from_json(&catalog_json).unwrap();
        let (descriptor, _, _) = custom_input_fixture(
            "fault-live",
            "normalize-receipt",
            "normalize-receipt",
            Some("pure"),
            b"resolved-before-transaction",
        );
        let supplied = load_supplied_components(&[descriptor]).unwrap();
        let graph = custom_graph("normalize-receipt");

        client.batch_execute("BEGIN").await.unwrap();
        client
            .batch_execute("SET LOCAL wamn.test.publication_fault = 'after-artifacts'")
            .await
            .unwrap();
        let failure = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &BareSchemaName::new("pg_temp").unwrap(),
            vec![prepare_flow_artifact(&tenant, &graph, &supplied).unwrap()],
            None,
            &catalog_json,
            None,
        )
        .await;
        let error = finish_publication_transaction(&client, failure)
            .await
            .expect_err("fault after custom artifact insert must roll back");
        assert!(format!("{error:#}").contains("injected-publication-fault-after-artifacts"));
        let count: i64 = client
            .query_one(
                "SELECT count(*) FROM catalog.flow_artifacts WHERE tenant_id = $1",
                &[&tenant],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 0);

        client.batch_execute("BEGIN").await.unwrap();
        let retry = publish_release(
            &client,
            &catalog,
            &tenant,
            "dev",
            &BareSchemaName::new("pg_temp").unwrap(),
            vec![prepare_flow_artifact(&tenant, &graph, &supplied).unwrap()],
            None,
            &catalog_json,
            None,
        )
        .await;
        finish_publication_transaction(&client, retry)
            .await
            .unwrap();
        let count: i64 = client
            .query_one(
                "SELECT count(*) FROM catalog.flow_artifacts WHERE tenant_id = $1",
                &[&tenant],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 1);
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
            custom_node: vec![],
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
    fn standard_transform_resolves_into_a_canonical_artifact() {
        let graph = r#"{
          "schema-version":"0.1","flow-id":"standard-flow","version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":{
              "$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"
            }}},
            {"id":"shape","type":"transform","config":{"expression":"@"}},
            {"id":"respond","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"shape"},
            {"from":"shape","to":"respond"}
          ]
        }"#;
        let prepared = prepare_flow_artifact("tenant", graph, &BTreeMap::new())
            .expect("standard node resolves");
        assert_eq!(prepared.artifact.interfaces().len(), 1);
        assert_eq!(prepared.artifact.interfaces()[0].node_type, "transform");
        assert_eq!(prepared.artifact.interfaces()[0].output_ports, ["main"]);
        let contract = &prepared.artifact.interface_bundle().contracts()[0];
        assert_eq!(contract.interface.interface_contract, "wamn:node@0.1.0");
        assert!(matches!(
            contract.executable,
            wamn_node_manifest::ExecutableIdentity::Platform { .. }
        ));
    }

    #[test]
    fn unresolved_custom_node_is_refused_before_storage() {
        let graph = custom_graph("tenant-custom");
        let error = prepare_flow_artifact("tenant", &graph, &BTreeMap::new())
            .expect_err("unresolved custom node must fail closed");
        assert!(format!("{error:#}").contains("has no resolved interface"));
    }

    #[test]
    fn verified_custom_component_pins_exact_bytes_and_pure_manifest() {
        let (descriptor, _, _) = custom_input_fixture(
            "pure",
            "normalize-receipt",
            "normalize-receipt",
            Some("pure"),
            b"component",
        );
        let supplied = load_supplied_components(&[descriptor]).unwrap();
        let graph = custom_graph("normalize-receipt");
        let prepared = prepare_flow_artifact("tenant", &graph, &supplied).unwrap();

        assert_eq!(
            prepared.supplied_node_types,
            BTreeSet::from(["normalize-receipt".to_string()])
        );
        assert_eq!(
            match &prepared.artifact.supplied_components()[0]
                .contract
                .executable
            {
                wamn_node_manifest::ExecutableIdentity::Component { digest } => digest.clone(),
                wamn_node_manifest::ExecutableIdentity::Platform { .. } => {
                    panic!("custom node resolved to a platform executable")
                }
            },
            component_digest(b"component")
        );
        let interface = &prepared.artifact.supplied_components()[0]
            .contract
            .interface;
        assert_eq!(
            &prepared.artifact.supplied_components()[0].contract,
            &prepared.artifact.interface_bundle().contracts()[0],
            "custom resolution must use the persisted canonical contract"
        );
        assert_eq!(interface.node_type, "normalize-receipt");
        assert_eq!(interface.output_ports, ["main"]);
        assert_eq!(interface.purity, wamn_node_manifest::ResolvedPurity::Pure);
        assert_eq!(
            interface.recovery_class,
            wamn_node_manifest::RecoveryClass::Replay
        );
    }

    #[test]
    fn absent_custom_purity_resolves_effectful_never_replay() {
        let (descriptor, _, _) = custom_input_fixture(
            "absent-purity",
            "legacy-node",
            "legacy-node",
            None,
            b"component",
        );
        let supplied = load_supplied_components(&[descriptor]).unwrap();
        let graph = custom_graph("legacy-node");
        let prepared = prepare_flow_artifact("tenant", &graph, &supplied).unwrap();
        let interface = &prepared.artifact.supplied_components()[0]
            .contract
            .interface;
        assert_eq!(
            interface.purity,
            wamn_node_manifest::ResolvedPurity::Effectful
        );
        assert_eq!(
            interface.recovery_class,
            wamn_node_manifest::RecoveryClass::NeverReplay
        );
    }

    #[test]
    fn custom_inputs_refuse_missing_mismatched_mutated_and_duplicate_material() {
        let (descriptor, component, manifest) =
            custom_input_fixture("negative", "node-a", "node-a", Some("pure"), b"component");

        let original_descriptor = std::fs::read_to_string(&descriptor).unwrap();
        let mut missing_digest: serde_json::Value =
            serde_json::from_str(&original_descriptor).unwrap();
        missing_digest
            .as_object_mut()
            .unwrap()
            .remove("component-digest");
        std::fs::write(&descriptor, missing_digest.to_string()).unwrap();
        let error = load_supplied_components(std::slice::from_ref(&descriptor)).unwrap_err();
        assert!(format!("{error:#}").contains("component-digest"));
        std::fs::write(&descriptor, &original_descriptor).unwrap();

        std::fs::write(&component, b"altered").unwrap();
        let error = load_supplied_components(std::slice::from_ref(&descriptor)).unwrap_err();
        assert!(format!("{error:#}").contains("component digest mismatch"));
        std::fs::write(&component, b"component").unwrap();

        let mut value: serde_json::Value = serde_json::from_str(&original_descriptor).unwrap();
        value["component-digest"] = serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
        std::fs::write(&descriptor, value.to_string()).unwrap();
        let error = load_supplied_components(std::slice::from_ref(&descriptor)).unwrap_err();
        assert!(format!("{error:#}").contains("component digest mismatch"));
        std::fs::write(&descriptor, &original_descriptor).unwrap();

        std::fs::write(&manifest, manifest_json("node-b", Some("pure"))).unwrap();
        let error = load_supplied_components(std::slice::from_ref(&descriptor)).unwrap_err();
        assert!(format!("{error:#}").contains("manifest node-type mismatch"));
        std::fs::write(&manifest, manifest_json("node-a", Some("pure"))).unwrap();

        let error =
            load_supplied_components(&[descriptor.clone(), descriptor.clone()]).unwrap_err();
        assert!(format!("{error:#}").contains("duplicate custom-node implementation"));

        std::fs::remove_file(&manifest).unwrap();
        let error = load_supplied_components(&[descriptor]).unwrap_err();
        assert!(format!("{error:#}").contains("read custom-node manifest"));
    }

    #[test]
    fn supplied_node_must_be_declared_directly_by_a_graph() {
        let (descriptor, _, _) = custom_input_fixture(
            "graph-shape",
            "normalize-receipt",
            "normalize-receipt",
            Some("pure"),
            b"component",
        );
        let supplied = load_supplied_components(&[descriptor]).unwrap();
        let legacy_indirection = r#"{
          "schema-version":"0.1","flow-id":"custom-flow","version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":{
              "$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"
            }}},
            {"id":"custom","type":"custom","config":{"manifest":"normalize-receipt"}},
            {"id":"respond","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"custom"},
            {"from":"custom","to":"respond"}
          ]
        }"#;
        let error = prepare_flow_artifact("tenant", legacy_indirection, &supplied)
            .expect_err("indirect custom declaration must not bypass graph/interface coherence");
        assert!(format!("{error:#}").contains("has no resolved interface"));

        let error = ensure_all_supplied_used(&supplied, &BTreeSet::new()).unwrap_err();
        assert!(format!("{error:#}").contains("unknown supplied custom node"));
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
}
