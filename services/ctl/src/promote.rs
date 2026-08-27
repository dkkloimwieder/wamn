//! Promote one immutable serving release into a target environment.
//!
//! The source v2 manifest is the closed deployment input. Promotion verifies
//! that snapshot, advances the target catalog only when it is behind, pulls and
//! verifies missing component artifacts, copies portable component facts and
//! requirements, and requires target-owned connection instances and generations.
//! It then re-runs wiring compatibility against the target catalog, mints the
//! target v2 snapshot through the ordinary release publisher, and atomically
//! flips every target pointer with one append-only provenance row.
//!
//! No connection instance, generation, credential handle, legacy flow, plan, or
//! execution bundle is read from the source environment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, IsolationLevel, NoTls, Row, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentParameter, AdmittedComponentPort,
    ComponentCatalogScope, ServingManifest, WiringDocument, flip_activation,
    record_activation_event, validate_wiring_compatibility,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_schema_control::BareSchemaName;
use wamn_schema_control::connections::ComponentConnectionRequirement;
use wamn_schema_model::Catalog;

use crate::migrate_catalog::{ApplyOutcome, apply_catalog_target};
use crate::publish_release::{MintReleaseManifest, ReleaseWiringTarget, mint_release_manifest};

const COMPONENT_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

const CLAIM_TENANT_LOCAL_SQL: &str = "SELECT set_config('app.tenant', $1, true)";
const CLAIM_TENANT_SESSION_SQL: &str = "SELECT set_config('app.tenant', $1, false)";

const SELECT_SOURCE_RELEASE_SQL: &str = "\
SELECT snapshot.manifest_digest, snapshot.canonical_bytes, catalog.document::text \
  FROM catalog.release_manifest_v2_snapshots AS snapshot \
  JOIN catalog.releases AS release \
    ON release.tenant_id = snapshot.tenant_id \
   AND release.catalog_id = snapshot.catalog_id \
   AND release.catalog_version = snapshot.catalog_version \
  JOIN catalog.catalogs AS catalog \
    ON catalog.tenant_id = release.tenant_id \
   AND catalog.catalog_id = release.catalog_id \
   AND catalog.version = release.catalog_version \
 WHERE snapshot.tenant_id = $1 AND snapshot.catalog_id = $2 \
   AND snapshot.catalog_version = $3 AND catalog.environment = $4";

const SELECT_COMPONENTS_SQL: &str = "\
SELECT component, interface_version, operation, component_digest, \
       imports::text, imports_fingerprint, input_ports::text, \
       output_ports::text, parameters::text, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
 ORDER BY component COLLATE \"C\", interface_version COLLATE \"C\"";

const SELECT_COMPONENT_SQL: &str = "\
SELECT component, interface_version, operation, component_digest, \
       imports::text, imports_fingerprint, input_ports::text, \
       output_ports::text, parameters::text, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
   AND component = $4 AND interface_version = $5";

const SELECT_SOURCE_WIRING_SQL: &str = "\
SELECT gated_catalog_version, wiring_hash, graph_json::text \
  FROM catalog.wirings \
 WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = $3 AND version = $4";

const INSERT_TARGET_WIRING_SQL: &str = "\
INSERT INTO catalog.wirings (\
       tenant_id, catalog_id, wiring_id, version, gated_catalog_version, \
       graph_json, wiring_hash\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
ON CONFLICT DO NOTHING";

const EXACT_TARGET_WIRING_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.wirings \
     WHERE tenant_id = $1 AND catalog_id = $2 AND wiring_id = $3 AND version = $4 \
       AND gated_catalog_version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7\
    )";

const SELECT_COMPONENT_REQUIREMENTS_SQL: &str = "\
SELECT component_digest, store_alias, requirement_json::text, requirement_hash \
  FROM catalog.connection_requirements \
 WHERE tenant_id = $1 AND component_digest IS NOT NULL \
 ORDER BY component_digest COLLATE \"C\", store_alias COLLATE \"C\"";

const TARGET_BINDING_READY_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 \
      FROM catalog.connection_bindings AS binding \
      JOIN catalog.connection_instances AS instance \
        ON instance.tenant_id = binding.tenant_id \
       AND instance.environment = binding.environment \
       AND instance.instance_id = binding.instance_id \
     WHERE binding.tenant_id = $1 AND binding.catalog_id = $2 \
       AND binding.catalog_version = $3 AND binding.component_digest = $4 \
       AND binding.store_alias = $5 AND binding.environment = $6 \
       AND binding.artifact_hash IS NULL AND binding.requirement_name IS NULL \
       AND binding.binding_status = 'active' \
       AND binding.validation_status = 'valid' \
       AND instance.lifecycle_status = 'enabled' \
       AND instance.active_generation IS NOT NULL\
    )";

const SELECT_TARGET_HEAD_SQL: &str = "\
SELECT applied_catalog_version \
  FROM catalog.catalog_heads \
 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3";

const SELECT_TARGET_CATALOG_SQL: &str = "\
SELECT document::text \
  FROM catalog.catalogs \
 WHERE tenant_id = $1 AND catalog_id = $2 AND version = $3 AND environment = $4";

const LOCK_TARGET_HEAD_SQL: &str = "\
SELECT applied_catalog_version \
  FROM catalog.catalog_heads \
 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
 FOR UPDATE";

const SELECT_TARGET_POINTER_SQL: &str = "\
SELECT confirmed_definition_hash, enabled \
  FROM catalog.wiring_activation \
 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 AND wiring_id = $4";

const SELECT_PROMOTION_EVENTS_SQL: &str = "\
SELECT changed_by, reason \
  FROM catalog.wiring_activation_events \
 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 AND wiring_id = $4 \
   AND enabled AND confirmed_definition_hash = $5 \
   AND source_environment = $6 \
 ORDER BY event_seq";

/// Promote one source release into one target project-environment database.
#[derive(Debug, Args)]
pub struct PromoteArgs {
    /// Owner URL to the source project-environment database.
    #[arg(long)]
    pub source_database_url: String,

    /// Owner URL to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub target_database_url: String,

    /// Tenant claim carried by the source release and target catalog.
    #[arg(long)]
    pub tenant: String,

    /// Catalog identity of the release.
    #[arg(long)]
    pub catalog_id: String,

    /// Exact source release catalog version.
    #[arg(long)]
    pub catalog_version: u32,

    /// Environment recorded by the source serving manifest.
    #[arg(long)]
    pub source_environment: String,

    /// Target environment whose catalog head and wiring pointers move.
    #[arg(long)]
    pub target_environment: String,

    /// Target data schema used by additive catalog migration.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// The run-plane schema in the TARGET project database holding the
    /// provisioned environment fact — the same `--schema` `reconcile-run-plane`
    /// converged there (`wamn_run` in the schema of record).
    ///
    /// Bare identifier, and REQUIRED, exactly as on `publish-release`: a default
    /// would let an invocation that omits the flag check the promoted release
    /// against a relation the operator never named. Distinct from `--schema`
    /// above, which is the target DATA schema.
    ///
    /// PRECONDITION: run `reconcile-run-plane` for this tenant and this schema in
    /// the target database FIRST. Promoting before it has converged the tenant's
    /// `environment_policies` row refuses on `environment-policy-not-converged`;
    /// a row naming another environment refuses on
    /// `environment-policy-environment-mismatch`.
    #[arg(long)]
    pub run_schema: String,

    /// Explicit registry/repository base used by component publication.
    #[arg(long)]
    pub artifact_base: String,

    /// Projected `.dockerconfigjson` file carrying the target pull credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in artifact-base.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// Authenticated principal recorded on every target activation event.
    #[arg(long)]
    pub principal: String,

    /// Stable operator reason recorded on every target activation event.
    #[arg(long, default_value = "promote-release")]
    pub reason: String,
}

#[derive(Clone, Debug)]
struct SourceWiring {
    target: ReleaseWiringTarget,
    document: WiringDocument,
    graph_json: String,
    wiring_hash: String,
}

#[derive(Clone, Debug)]
struct PortableRequirement {
    requirement: ComponentConnectionRequirement,
    canonical_json: String,
    requirement_hash: String,
}

#[derive(Debug)]
struct SourceRelease {
    manifest: ServingManifest,
    manifest_digest: String,
    catalog: Catalog,
    components: Vec<AdmittedComponent>,
    wirings: Vec<SourceWiring>,
    requirements: Vec<PortableRequirement>,
}

#[derive(Debug)]
struct PromotionSummary {
    target_manifest_digest: String,
    pulled_components: usize,
    activated_wirings: usize,
}

/// Run the convergent promotion pipeline.
pub async fn run(args: PromoteArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid target schema {:?}", args.schema))?;
    let run_schema = BareSchemaName::new(args.run_schema.clone())
        .with_context(|| format!("invalid --run-schema {:?}", args.run_schema))?;
    let artifact_config = ComponentArtifactSourceConfig::new(
        &args.artifact_base,
        args.insecure_registry,
        COMPONENT_FETCH_TIMEOUT,
    )
    .context("configure component artifact source")?
    .with_registry_auth_file(&args.registry_auth_file)
    .context("load component registry pull credential")?;
    let artifact_source = ComponentArtifactSource::new(artifact_config);
    let catalog_version = i32::try_from(args.catalog_version)
        .context("source catalog version exceeds the PostgreSQL integer carrier")?;

    let (mut source_client, source_connection) =
        tokio_postgres::connect(&args.source_database_url, NoTls)
            .await
            .context("connect to source project environment")?;
    let source_task = tokio::spawn(source_connection);
    let source = match load_source_release(
        &mut source_client,
        &args.tenant,
        &args.catalog_id,
        catalog_version,
        &args.source_environment,
    )
    .await
    {
        Ok(source) => source,
        Err(error) => {
            source_task.abort();
            return Err(error);
        }
    };
    drop(source_client);
    source_task
        .await
        .context("join source database connection")?
        .context("drive source database connection")?;
    // The released wiring ids must still be unique: promotion writes one target
    // row per id, so two members sharing one id would silently promote whichever
    // ran last. This used to ride the gate-report key-set check that supplied
    // the per-wiring report ids; that argument is gone (wamn-0h0g.8.5.6) and the
    // uniqueness rule it carried is not.
    require_unique_wiring_ids(
        source
            .wirings
            .iter()
            .map(|wiring| wiring.target.wiring_id.as_str()),
    )?;

    let (mut target_client, target_connection) =
        tokio_postgres::connect(&args.target_database_url, NoTls)
            .await
            .context("connect to target project environment")?;
    let target_task = tokio::spawn(target_connection);
    let promoted = promote_target(
        &mut target_client,
        &artifact_source,
        &source,
        &args,
        &schema,
        &run_schema,
    )
    .await;
    let summary = match promoted {
        Ok(summary) => summary,
        Err(error) => {
            target_task.abort();
            return Err(error);
        }
    };
    drop(target_client);
    target_task
        .await
        .context("join target database connection")?
        .context("drive target database connection")?;

    println!(
        "promoted {} from {} to {} as {} ({} component pull(s), {} pointer flip(s))",
        source.manifest_digest,
        args.source_environment,
        args.target_environment,
        summary.target_manifest_digest,
        summary.pulled_components,
        summary.activated_wirings,
    );
    Ok(())
}

fn validate_args(args: &PromoteArgs) -> anyhow::Result<()> {
    for (field, value) in [
        ("tenant", args.tenant.as_str()),
        ("catalog-id", args.catalog_id.as_str()),
        ("source-environment", args.source_environment.as_str()),
        ("target-environment", args.target_environment.as_str()),
        ("principal", args.principal.as_str()),
        ("reason", args.reason.as_str()),
    ] {
        ensure!(!value.is_empty(), "promotion {field} must not be empty");
    }
    ensure!(
        args.catalog_version > 0,
        "catalog-version must be greater than zero"
    );
    ensure!(
        args.source_environment != args.target_environment,
        "source and target environments must differ"
    );
    Ok(())
}

fn require_unique_wiring_ids<'a>(
    wiring_ids: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut observed = BTreeSet::new();
    for wiring_id in wiring_ids {
        ensure!(
            observed.insert(wiring_id),
            "source-release-duplicate-wiring-id: {wiring_id:?} names more than one version"
        );
    }
    Ok(())
}

async fn load_source_release(
    client: &mut Client,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
    source_environment: &str,
) -> anyhow::Result<SourceRelease> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .context("begin source release snapshot")?;
    transaction
        .query_one(CLAIM_TENANT_LOCAL_SQL, &[&tenant])
        .await
        .context("claim source tenant")?;
    let row = transaction
        .query_opt(
            SELECT_SOURCE_RELEASE_SQL,
            &[&tenant, &catalog_id, &catalog_version, &source_environment],
        )
        .await
        .context("read source v2 release snapshot")?
        .with_context(|| {
            format!(
                "source-v2-release-missing: catalog {catalog_id:?} version {catalog_version} in {source_environment:?}"
            )
        })?;
    let stored_digest: String = row.get(0);
    let canonical_bytes: Vec<u8> = row.get(1);
    let catalog_json: Option<String> = row.get(2);
    let catalog_json = catalog_json.context("source release catalog has no stored document")?;
    let (manifest, derived_digest) = ServingManifest::from_canonical_bytes(&canonical_bytes)
        .context("verify source v2 serving manifest")?;
    ensure!(
        stored_digest == derived_digest.as_str(),
        "source serving-manifest digest does not name its canonical bytes"
    );
    let expected_version = u32::try_from(catalog_version).expect("validated source width");
    ensure!(
        manifest.release.tenant_id == tenant
            && manifest.release.catalog_id == catalog_id
            && manifest.release.catalog_version == expected_version
            && manifest.release.environment == source_environment,
        "source serving-manifest coordinate mismatch"
    );
    let catalog = Catalog::from_json(&catalog_json).context("parse source release catalog")?;
    ensure!(
        catalog.catalog_id == catalog_id && catalog.version == expected_version,
        "source catalog document coordinate mismatch"
    );

    let all_components = load_components(
        &transaction,
        ComponentCatalogScope {
            tenant_id: tenant.to_owned(),
            catalog_id: catalog_id.to_owned(),
            catalog_version: expected_version,
        },
    )
    .await?;
    let components = select_manifest_components(&manifest, all_components)?;
    let wirings = load_source_wirings(
        &transaction,
        tenant,
        catalog_id,
        catalog_version,
        &manifest,
        &components,
    )
    .await?;
    let requirements = load_source_requirements(&transaction, tenant, &manifest).await?;
    transaction
        .commit()
        .await
        .context("finish source release snapshot")?;
    Ok(SourceRelease {
        manifest,
        manifest_digest: stored_digest,
        catalog,
        components,
        wirings,
        requirements,
    })
}

async fn load_components(
    transaction: &Transaction<'_>,
    scope: ComponentCatalogScope,
) -> anyhow::Result<Vec<AdmittedComponent>> {
    transaction
        .query(
            SELECT_COMPONENTS_SQL,
            &[
                &scope.tenant_id,
                &scope.catalog_id,
                &i32::try_from(scope.catalog_version).context("component catalog version")?,
            ],
        )
        .await
        .context("read component-library facts")?
        .into_iter()
        .map(|row| decode_component(row, &scope))
        .collect()
}

fn decode_component(row: Row, scope: &ComponentCatalogScope) -> anyhow::Result<AdmittedComponent> {
    let component: String = row.get(0);
    let decoded = AdmittedComponent {
        scope: scope.clone(),
        component: component.clone(),
        interface_version: row.get(1),
        operation: row.get(2),
        component_digest: row.get(3),
        imports: decode_json(row.get(4), &component, "imports")?,
        imports_fingerprint: row.get(5),
        input_ports: decode_json::<Vec<AdmittedComponentPort>>(
            row.get(6),
            &component,
            "input-ports",
        )?,
        output_ports: decode_json::<Vec<AdmittedComponentPort>>(
            row.get(7),
            &component,
            "output-ports",
        )?,
        parameters: decode_json::<Vec<AdmittedComponentParameter>>(
            row.get(8),
            &component,
            "parameters",
        )?,
        effects: decode_json::<Vec<AdmittedComponentEffect>>(row.get(9), &component, "effects")?,
    };
    // wamn-0h0g.21.10: promotion copies this stored fact verbatim into the
    // target catalog version, so an effect projection no validator derived
    // would be laundered forward as a fresh admission. Refuse it here instead.
    wamn_catalog::verify_stored_effect_projection(&decoded).with_context(|| {
        format!(
            "component {component:?} stores an effect projection its audited imports do not \
             derive; re-admit it through the validator"
        )
    })?;
    Ok(decoded)
}

fn decode_json<T: DeserializeOwned>(
    stored: String,
    component: &str,
    field: &'static str,
) -> anyhow::Result<T> {
    serde_json::from_str(&stored)
        .with_context(|| format!("component {component:?} stores unreadable {field}"))
}

fn select_manifest_components(
    manifest: &ServingManifest,
    facts: Vec<AdmittedComponent>,
) -> anyhow::Result<Vec<AdmittedComponent>> {
    let by_pair = facts
        .into_iter()
        .map(|fact| {
            (
                (fact.component.clone(), fact.interface_version.clone()),
                fact,
            )
        })
        .collect::<BTreeMap<_, _>>();
    manifest
        .components
        .iter()
        .map(|member| {
            let fact = by_pair
                .get(&(member.component.clone(), member.interface_version.clone()))
                .with_context(|| {
                    format!(
                        "source component fact missing for {:?} interface {:?}",
                        member.component, member.interface_version
                    )
                })?;
            ensure!(
                fact.component_digest == member.digest.as_str(),
                "source component {:?} interface {:?} has digest {:?}, not released digest {:?}",
                member.component,
                member.interface_version,
                fact.component_digest,
                member.digest
            );
            Ok(fact.clone())
        })
        .collect()
}

async fn load_source_wirings(
    transaction: &Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
    manifest: &ServingManifest,
    components: &[AdmittedComponent],
) -> anyhow::Result<Vec<SourceWiring>> {
    let scope = ComponentCatalogScope {
        tenant_id: tenant.to_owned(),
        catalog_id: catalog_id.to_owned(),
        catalog_version: u32::try_from(catalog_version).expect("validated source width"),
    };
    let mut wirings = Vec::with_capacity(manifest.wirings.len());
    for member in &manifest.wirings {
        let version = i32::try_from(member.wiring_version)
            .context("source wiring version exceeds the PostgreSQL integer carrier")?;
        let row = transaction
            .query_opt(
                SELECT_SOURCE_WIRING_SQL,
                &[&tenant, &catalog_id, &member.wiring_id, &version],
            )
            .await
            .context("read source wiring")?
            .with_context(|| {
                format!(
                    "source wiring {:?} version {} is missing",
                    member.wiring_id, member.wiring_version
                )
            })?;
        let gated_catalog_version: i32 = row.get(0);
        let wiring_hash: String = row.get(1);
        let graph_json: String = row.get(2);
        ensure!(
            gated_catalog_version == catalog_version && wiring_hash == member.graph_hash.as_str(),
            "source wiring {:?} version {} does not match its released identity",
            member.wiring_id,
            member.wiring_version
        );
        let graph_value = serde_json::from_str(&graph_json).with_context(|| {
            format!(
                "source wiring {:?} stores unreadable JSON",
                member.wiring_id
            )
        })?;
        let document = WiringDocument::parse(&graph_value).with_context(|| {
            format!(
                "source wiring {:?} stores an invalid document",
                member.wiring_id
            )
        })?;
        ensure!(
            document.wiring_id == member.wiring_id
                && document.version == member.wiring_version
                && document.wiring_hash().as_str() == wiring_hash,
            "source wiring {:?} row and document identities differ",
            member.wiring_id
        );
        validate_wiring_compatibility(&document, &scope, components)
            .with_context(|| format!("source wiring {:?} no longer validates", member.wiring_id))?;
        wirings.push(SourceWiring {
            target: ReleaseWiringTarget {
                wiring_id: member.wiring_id.clone(),
                wiring_version: member.wiring_version,
            },
            document,
            graph_json,
            wiring_hash,
        });
    }
    Ok(wirings)
}

async fn load_source_requirements(
    transaction: &Transaction<'_>,
    tenant: &str,
    manifest: &ServingManifest,
) -> anyhow::Result<Vec<PortableRequirement>> {
    let released_digests = manifest
        .components
        .iter()
        .map(|component| component.digest.as_str())
        .collect::<BTreeSet<_>>();
    let mut requirements = Vec::new();
    for row in transaction
        .query(SELECT_COMPONENT_REQUIREMENTS_SQL, &[&tenant])
        .await
        .context("read source component connection requirements")?
    {
        let component_digest: String = row.get(0);
        if !released_digests.contains(component_digest.as_str()) {
            continue;
        }
        let store_alias: String = row.get(1);
        let canonical_json: String = row.get(2);
        let requirement_hash: String = row.get(3);
        let requirement: ComponentConnectionRequirement = serde_json::from_str(&canonical_json)
            .with_context(|| {
                format!(
                    "source requirement for component {component_digest:?} alias {store_alias:?} is unreadable"
                )
            })?;
        ensure!(
            requirement.component_digest() == component_digest
                && requirement.store_alias() == store_alias
                && requirement.requirement_hash() == requirement_hash,
            "source component requirement identity or hash mismatch"
        );
        requirements.push(PortableRequirement {
            requirement,
            canonical_json,
            requirement_hash,
        });
    }
    Ok(requirements)
}

async fn promote_target(
    client: &mut Client,
    artifact_source: &ComponentArtifactSource,
    source: &SourceRelease,
    args: &PromoteArgs,
    schema: &BareSchemaName,
    run_schema: &BareSchemaName,
) -> anyhow::Result<PromotionSummary> {
    // wamn-0h0g.12.183: `ensure_catalog_storage` refuses a control-plane target
    // BEFORE bootstrapping the role, and calls `ensure_wamn_app_role` itself.
    // A second call here only ran that CLUSTER-GLOBAL mutation ahead of the
    // refusal, which no later error can take back.
    crate::publish_catalog::ensure_catalog_storage(client).await?;
    client
        .query_one(CLAIM_TENANT_SESSION_SQL, &[&args.tenant])
        .await
        .context("claim target tenant")?;
    let target_version = converge_target_catalog(client, source, args, schema).await?;
    let target_catalog = load_target_catalog(client, args, target_version).await?;
    require_target_registration_entities(source, &target_catalog)?;
    let target_scope = ComponentCatalogScope {
        tenant_id: args.tenant.clone(),
        catalog_id: args.catalog_id.clone(),
        catalog_version: u32::try_from(target_version).expect("positive target catalog version"),
    };
    let target_components = source
        .components
        .iter()
        .cloned()
        .map(|mut component| {
            component.scope = target_scope.clone();
            component
        })
        .collect::<Vec<_>>();

    let mut pulled_components = 0;
    for (source_component, target_component) in
        source.components.iter().zip(target_components.iter())
    {
        match load_one_component(client, &target_scope, target_component).await? {
            Some(existing) => ensure!(
                existing == *target_component,
                "target component-library fact conflicts for {:?} interface {:?}",
                target_component.component,
                target_component.interface_version
            ),
            None => {
                artifact_source
                    .pull_verified(source_component)
                    .await
                    .with_context(|| {
                        format!(
                            "pull target component {:?} interface {:?}",
                            source_component.component, source_component.interface_version
                        )
                    })?;
                pulled_components += 1;
            }
        }
    }

    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::Serializable)
        .start()
        .await
        .context("begin target promotion")?;
    transaction
        .query_one(CLAIM_TENANT_LOCAL_SQL, &[&args.tenant])
        .await
        .context("claim target promotion tenant")?;
    let locked_version: i32 = transaction
        .query_opt(
            LOCK_TARGET_HEAD_SQL,
            &[&args.tenant, &args.catalog_id, &args.target_environment],
        )
        .await
        .context("lock target catalog head")?
        .context("target catalog head disappeared during promotion")?
        .get(0);
    ensure!(
        locked_version == target_version,
        "target-catalog-head-moved: planned {target_version}, observed {locked_version}"
    );

    for component in &target_components {
        crate::push_component::append_or_verify_admitted_component(
            &transaction,
            component,
            target_version,
        )
        .await?;
    }
    for requirement in &source.requirements {
        persist_requirement(&transaction, &args.tenant, requirement).await?;
        require_target_binding(
            &transaction,
            &args.tenant,
            &args.catalog_id,
            target_version,
            &args.target_environment,
            requirement,
        )
        .await?;
    }

    let target_wirings = source
        .wirings
        .iter()
        .map(|wiring| wiring.target.clone())
        .collect::<BTreeSet<_>>();
    for wiring in &source.wirings {
        validate_wiring_compatibility(&wiring.document, &target_scope, &target_components)
            .with_context(|| {
                format!(
                    "target wiring {:?} version {} is incompatible",
                    wiring.target.wiring_id, wiring.target.wiring_version
                )
            })?;
        persist_target_wiring(
            &transaction,
            &args.tenant,
            &args.catalog_id,
            target_version,
            wiring,
        )
        .await?;
    }

    let minted = mint_release_manifest(
        &transaction,
        &MintReleaseManifest {
            tenant_id: &args.tenant,
            catalog_id: &args.catalog_id,
            catalog_version: target_version,
            wirings: &target_wirings,
            attachments: &source.manifest.attachments,
            registrations: &source.manifest.registrations,
        },
    )
    .await
    .context("mint target v2 release snapshot")?;

    // wamn-0h0g.8.27, owner ruling 2026-08-27: COVER PROMOTE. `--target-environment`
    // is an operator's word for where this release is going; the release the mint
    // froze carries `catalog.catalogs.environment`, which no constraint ties to any
    // provisioned identity. The same verify arm `publish-release` takes checks the
    // carried label against the fact `reconcile-run-plane` projected into THIS
    // target database, so a promotion into an environment nothing verified refuses
    // instead of freezing a release there. Inside the promotion's own transaction:
    // a refusal commits neither the snapshot nor the pointer flip below.
    let expected_environment =
        crate::publish_release::read_expected_environment(&transaction, run_schema, &args.tenant)
            .await?;
    crate::publish_release::verify_provisioned_environment(
        expected_environment.as_deref(),
        &minted.manifest.release,
        run_schema,
    )?;

    let mut activated_wirings = 0;
    for wiring in &source.wirings {
        if activate_once(&transaction, args, wiring).await? {
            activated_wirings += 1;
        }
    }
    transaction
        .commit()
        .await
        .context("commit target promotion")?;
    Ok(PromotionSummary {
        target_manifest_digest: minted.digest.as_str().to_owned(),
        pulled_components,
        activated_wirings,
    })
}

async fn converge_target_catalog(
    client: &mut Client,
    source: &SourceRelease,
    args: &PromoteArgs,
    schema: &BareSchemaName,
) -> anyhow::Result<i32> {
    let source_version = i32::try_from(source.manifest.release.catalog_version)
        .context("source release version exceeds target storage width")?;
    let mut target_version = read_target_head(client, args).await?;
    if target_version.is_none_or(|version| version < source_version) {
        crate::publish_catalog::guard_registration_orphans(client, &source.catalog).await?;
        let expected_base = target_version
            .map(u32::try_from)
            .transpose()
            .context("target catalog head is negative")?;
        let outcome = apply_catalog_target(
            client,
            &args.tenant,
            &args.target_environment,
            schema,
            &source.catalog,
            expected_base,
            true,
        )
        .await
        .context("apply source catalog to target")?;
        if matches!(outcome, ApplyOutcome::Applied(_)) {
            crate::reconcile_replica_identity::reconcile_after_apply(
                client,
                &source.catalog,
                schema.as_str(),
            )
            .await?;
        }
        target_version = read_target_head(client, args).await?;
    }
    let target_version = target_version.context("target catalog has no applied head")?;
    ensure!(
        target_version >= source_version,
        "target-catalog-behind: target {target_version}, source release {source_version}"
    );
    Ok(target_version)
}

async fn read_target_head(client: &Client, args: &PromoteArgs) -> anyhow::Result<Option<i32>> {
    Ok(client
        .query_opt(
            SELECT_TARGET_HEAD_SQL,
            &[&args.tenant, &args.catalog_id, &args.target_environment],
        )
        .await
        .context("read target catalog head")?
        .map(|row| row.get(0)))
}

async fn load_target_catalog(
    client: &Client,
    args: &PromoteArgs,
    target_version: i32,
) -> anyhow::Result<Catalog> {
    let row = client
        .query_opt(
            SELECT_TARGET_CATALOG_SQL,
            &[
                &args.tenant,
                &args.catalog_id,
                &target_version,
                &args.target_environment,
            ],
        )
        .await
        .context("read target-current catalog document")?
        .context("target-current catalog document is missing")?;
    let document: Option<String> = row.get(0);
    let document = document.context("target-current catalog has no stored document")?;
    let catalog = Catalog::from_json(&document).context("parse target-current catalog document")?;
    let expected_version = u32::try_from(target_version).context("target catalog version")?;
    ensure!(
        catalog.catalog_id == args.catalog_id && catalog.version == expected_version,
        "target-current catalog document coordinate mismatch"
    );
    Ok(catalog)
}

fn require_target_registration_entities(
    source: &SourceRelease,
    target_catalog: &Catalog,
) -> anyhow::Result<()> {
    let target_entities = target_catalog
        .entities
        .iter()
        .map(|entity| entity.id.as_str())
        .collect::<BTreeSet<_>>();
    for (registration_id, registration) in &source.manifest.registrations {
        ensure!(
            target_entities.contains(registration.entity.as_str()),
            "target-registration-entity-missing: registration {registration_id:?} names entity {:?}",
            registration.entity
        );
    }
    Ok(())
}

async fn load_one_component(
    client: &Client,
    scope: &ComponentCatalogScope,
    target: &AdmittedComponent,
) -> anyhow::Result<Option<AdmittedComponent>> {
    let version = i32::try_from(scope.catalog_version).context("target component version")?;
    client
        .query_opt(
            SELECT_COMPONENT_SQL,
            &[
                &scope.tenant_id,
                &scope.catalog_id,
                &version,
                &target.component,
                &target.interface_version,
            ],
        )
        .await
        .context("read target component-library fact")?
        .map(|row| decode_component(row, scope))
        .transpose()
}

async fn persist_requirement(
    transaction: &Transaction<'_>,
    tenant: &str,
    requirement: &PortableRequirement,
) -> anyhow::Result<()> {
    let component_digest = requirement.requirement.component_digest();
    let store_alias = requirement.requirement.store_alias();
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] = [
        &tenant,
        &component_digest,
        &store_alias,
        &requirement.canonical_json,
        &requirement.requirement_hash,
    ];
    transaction
        .execute(
            wamn_schema_control::connections::insert_component_connection_requirement_sql(),
            &params,
        )
        .await
        .context("append portable component requirement")?;
    let exact: bool = transaction
        .query_one(
            wamn_schema_control::connections::exact_component_connection_requirement_sql(),
            &params,
        )
        .await
        .context("verify portable component requirement")?
        .get(0);
    ensure!(exact, "target-component-requirement-conflict");
    Ok(())
}

async fn require_target_binding(
    transaction: &Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
    environment: &str,
    requirement: &PortableRequirement,
) -> anyhow::Result<()> {
    let component_digest = requirement.requirement.component_digest();
    let store_alias = requirement.requirement.store_alias();
    let ready: bool = transaction
        .query_one(
            TARGET_BINDING_READY_SQL,
            &[
                &tenant,
                &catalog_id,
                &catalog_version,
                &component_digest,
                &store_alias,
                &environment,
            ],
        )
        .await
        .context("verify target connection binding")?
        .get(0);
    ensure!(
        ready,
        "target-connection-binding-not-ready: component {component_digest:?} alias {store_alias:?}"
    );
    Ok(())
}

async fn persist_target_wiring(
    transaction: &Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
    wiring: &SourceWiring,
) -> anyhow::Result<()> {
    let wiring_version = i32::try_from(wiring.target.wiring_version)
        .context("target wiring version exceeds PostgreSQL width")?;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
        &tenant,
        &catalog_id,
        &wiring.target.wiring_id,
        &wiring_version,
        &catalog_version,
        &wiring.graph_json,
        &wiring.wiring_hash,
    ];
    transaction
        .execute(INSERT_TARGET_WIRING_SQL, &params)
        .await
        .context("append target gated wiring")?;
    let exact: bool = transaction
        .query_one(EXACT_TARGET_WIRING_SQL, &params)
        .await
        .context("verify target gated wiring")?
        .get(0);
    ensure!(
        exact,
        "target-wiring-content-conflict: {:?} version {}",
        wiring.target.wiring_id,
        wiring.target.wiring_version
    );
    Ok(())
}

async fn activate_once(
    transaction: &Transaction<'_>,
    args: &PromoteArgs,
    wiring: &SourceWiring,
) -> anyhow::Result<bool> {
    let pointer = transaction
        .query_opt(
            SELECT_TARGET_POINTER_SQL,
            &[
                &args.tenant,
                &args.catalog_id,
                &args.target_environment,
                &wiring.target.wiring_id,
            ],
        )
        .await
        .context("read target wiring pointer")?;
    let pointer_is_exact = pointer
        .is_some_and(|row| row.get::<_, String>(0) == wiring.wiring_hash && row.get::<_, bool>(1));
    let events = transaction
        .query(
            SELECT_PROMOTION_EVENTS_SQL,
            &[
                &args.tenant,
                &args.catalog_id,
                &args.target_environment,
                &wiring.target.wiring_id,
                &wiring.wiring_hash,
                &args.source_environment,
            ],
        )
        .await
        .context("read target promotion provenance")?;
    ensure!(
        events.len() <= 1,
        "promotion-provenance-conflict: duplicate events for wiring {:?}",
        wiring.target.wiring_id
    );
    if let Some(event) = events.first() {
        let changed_by: String = event.get(0);
        let reason: String = event.get(1);
        ensure!(
            changed_by == args.principal && reason == args.reason,
            "promotion-provenance-conflict: wiring {:?} was promoted with different caller facts",
            wiring.target.wiring_id
        );
        ensure!(
            pointer_is_exact,
            "promotion-pointer-moved-after-completion: wiring {:?}",
            wiring.target.wiring_id
        );
        return Ok(false);
    }
    ensure!(
        !pointer_is_exact,
        "promotion-provenance-missing: wiring {:?} already points at the promoted hash",
        wiring.target.wiring_id
    );

    transaction
        .execute(
            flip_activation(),
            &[
                &args.catalog_id,
                &args.target_environment,
                &wiring.target.wiring_id,
                &wiring.wiring_hash,
                &true,
            ],
        )
        .await
        .context("flip target wiring pointer")?;
    transaction
        .query_one(
            record_activation_event(),
            &[
                &args.catalog_id,
                &args.target_environment,
                &wiring.target.wiring_id,
                &true,
                &wiring.wiring_hash,
                &args.source_environment,
                &args.principal,
                &args.reason,
            ],
        )
        .await
        .context("append target wiring activation provenance")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wamn-0h0g.8.27, owner ruling 2026-08-27: COVER PROMOTE. The run-plane
    /// schema this verb verifies its target environment in is REQUIRED and is
    /// NOT `--schema`.
    ///
    /// `--schema` is the target DATA schema and carries a default, so a surface
    /// that reused it would silently check the promoted release against whatever
    /// relation that default named — the trusted-carry the verify arm exists to
    /// stop. The two must therefore be separate flags, and the run-plane one must
    /// have no default at all.
    #[test]
    fn the_run_plane_schema_is_required_and_separate_from_the_data_schema() {
        use clap::Parser as _;

        #[derive(clap::Parser)]
        struct PromoteProbe {
            #[command(flatten)]
            args: PromoteArgs,
        }

        const PLACEMENT: [&str; 18] = [
            "promote",
            "--source-database-url",
            "postgres://source.invalid/env",
            "--target-database-url",
            "postgres://target.invalid/env",
            "--tenant",
            "tenant-a",
            "--catalog-id",
            "orders",
            "--catalog-version",
            "3",
            "--source-environment",
            "prod",
            "--target-environment",
            "canary",
            "--artifact-base",
            "registry.example/wamn/components",
            "--principal",
        ];

        let mut without = PLACEMENT.to_vec();
        without.extend_from_slice(&["operator", "--registry-auth-file", "auth.json"]);
        assert!(
            PromoteProbe::try_parse_from(without.clone()).is_err(),
            "promoted a release with no run-plane schema to verify its environment in"
        );

        let mut with = without;
        with.extend_from_slice(&["--run-schema", "wamn_run"]);
        let parsed = PromoteProbe::try_parse_from(with)
            .expect("the complete promotion surface parses")
            .args;
        assert_eq!(parsed.run_schema, "wamn_run");
        assert_eq!(
            parsed.schema, "public",
            "the DATA schema default must not stand in for the run-plane schema"
        );
        assert_ne!(parsed.run_schema, parsed.schema);
    }

    /// One target row per released wiring id, and no second identity supplied
    /// alongside the document (wamn-0h0g.8.5.6).
    #[test]
    fn released_wiring_ids_are_unique_and_no_report_is_supplied() {
        assert!(require_unique_wiring_ids(["orders", "orders"]).is_err());
        assert!(require_unique_wiring_ids(["orders", "shipments"]).is_ok());
        for statement in [INSERT_TARGET_WIRING_SQL, EXACT_TARGET_WIRING_SQL] {
            assert!(
                !statement.contains("gate_report_id"),
                "promotion regrew the collapsed second identifier"
            );
        }
    }

    #[test]
    fn promotion_writes_are_append_or_verify_and_activation_uses_the_one_builder() {
        for insert in [
            wamn_schema_control::connections::insert_component_connection_requirement_sql(),
            INSERT_TARGET_WIRING_SQL,
        ] {
            assert!(insert.contains("ON CONFLICT DO NOTHING"));
            assert!(!insert.contains("DO UPDATE"));
        }
        assert!(
            wamn_schema_control::connections::exact_component_connection_requirement_sql()
                .contains("requirement_hash = $5")
        );
        assert!(EXACT_TARGET_WIRING_SQL.contains("wiring_hash = $7"));
        assert!(flip_activation().contains("INTO catalog.wiring_activation"));
        assert!(record_activation_event().contains("INTO catalog.wiring_activation_events"));
    }

    #[test]
    fn source_queries_cannot_copy_environment_owned_connection_state() {
        let source_queries = [
            SELECT_SOURCE_RELEASE_SQL,
            SELECT_COMPONENTS_SQL,
            SELECT_SOURCE_WIRING_SQL,
            SELECT_COMPONENT_REQUIREMENTS_SQL,
        ]
        .join("\n");
        for forbidden in [
            "connection_instances",
            "connection_generations",
            "credential_set_handle",
            "release_flows",
            "execution_bundles",
            "flow_artifacts",
        ] {
            assert!(
                !source_queries.contains(forbidden),
                "source promotion query copies retired or environment-owned fact {forbidden}"
            );
        }
        assert!(TARGET_BINDING_READY_SQL.contains("validation_status = 'valid'"));
        assert!(TARGET_BINDING_READY_SQL.contains("active_generation IS NOT NULL"));
    }

    #[test]
    fn target_head_is_locked_before_snapshot_and_pointer_writes() {
        assert!(LOCK_TARGET_HEAD_SQL.contains("FOR UPDATE"));
        assert!(SELECT_SOURCE_RELEASE_SQL.contains("release_manifest_v2_snapshots"));
        assert!(SELECT_SOURCE_RELEASE_SQL.contains("catalog.environment = $4"));
        assert!(SELECT_PROMOTION_EVENTS_SQL.contains("source_environment = $6"));
    }
}
