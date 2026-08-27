//! Host-owned active-wiring resolution through the existing platform pool.

use std::sync::Arc;

use anyhow::Context as _;
use tokio_postgres::types::ToSql;
use wamn_catalog::{AdmittedComponent, WiringDocument};
use wamn_router::Wiring;
use wamn_run_state::AuthorityClass;

use crate::wiring_lowering::{
    GatedActiveWiring, ScopedWiringOperationFacts, WiringScope, lower_active_wiring,
    project_component_operation,
};

use super::{CandidateBindingWorld, WamnPostgres};

/// The single SQL snapshot behind a cache miss.
pub const ACTIVE_WIRING_SQL: &str = "\
WITH selected AS MATERIALIZED ( \
    SELECT wiring.version, \
           wiring.gated_catalog_version, \
           wiring.graph_json, \
           wiring.wiring_hash \
      FROM catalog.wiring_activation AS active \
      JOIN catalog.catalog_heads AS head \
        ON head.tenant_id = active.tenant_id \
       AND head.catalog_id = active.catalog_id \
       AND head.environment = active.environment \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = active.tenant_id \
       AND wiring.catalog_id = active.catalog_id \
       AND wiring.wiring_id = active.wiring_id \
       AND wiring.version = $5 \
       AND wiring.gated_catalog_version = head.applied_catalog_version \
       AND wiring.wiring_hash = active.confirmed_definition_hash \
     WHERE active.tenant_id = $1 \
       AND active.catalog_id = $2 \
       AND active.environment = $3 \
       AND active.wiring_id = $4 \
       AND active.enabled \
       AND NOT EXISTS ( \
           SELECT 1 \
             FROM catalog.wiring_tombstones AS dead \
            WHERE dead.tenant_id = active.tenant_id \
              AND dead.catalog_id = active.catalog_id \
              AND dead.environment = active.environment \
              AND dead.wiring_id = active.wiring_id \
       ) \
) \
SELECT selected.version, \
       selected.gated_catalog_version, \
       selected.graph_json::text, \
       selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'catalog-id', $2::text, \
                       'catalog-version', component.catalog_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operation', component.operation, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects, \
                   'input-ports', component.input_ports, \
                   'output-ports', component.output_ports, \
                   'parameters', component.parameters \
               ) ORDER BY component.component, component.interface_version \
           ) FILTER (WHERE component.component IS NOT NULL), \
           '[]'::jsonb \
       )::text AS components \
  FROM selected \
  LEFT JOIN catalog.component_library AS component \
   ON component.tenant_id = $1 \
   AND component.catalog_id = $2 \
   AND component.catalog_version = selected.gated_catalog_version \
   AND EXISTS ( \
       SELECT 1 \
         FROM jsonb_each(selected.graph_json -> 'nodes') AS node(node_id, definition) \
        WHERE definition ->> 'component' = component.component \
          AND definition ->> 'interface-version' = component.interface_version \
          AND definition ->> 'operation' = component.operation \
   ) \
 GROUP BY selected.version, selected.gated_catalog_version, \
          selected.graph_json, selected.wiring_hash";

/// The immutable-version snapshot behind a queued delivery. Unlike
/// [`ACTIVE_WIRING_SQL`], this deliberately does not consult the mutable
/// activation pointer: admission already froze the exact wiring version.
/// Release membership and the verified format-2 snapshot keep that historical
/// version scoped to the carried tenant/catalog/environment release.
pub const RELEASE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT snapshot.catalog_version \
      FROM catalog.release_manifest_v2_snapshots AS snapshot \
     WHERE snapshot.tenant_id = $1 \
       AND snapshot.catalog_id = $2 \
       AND snapshot.catalog_version = $6 \
       AND snapshot.manifest_digest = $7 \
       AND convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb \
             #>> '{release,environment}' = $3 \
), selected AS MATERIALIZED ( \
    SELECT wiring.version, wiring.gated_catalog_version, \
           wiring.graph_json, wiring.wiring_hash, \
           release_scope.catalog_version AS release_catalog_version \
      FROM release_scope \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = $1 \
       AND wiring.catalog_id = $2 \
       AND wiring.wiring_id = $4 \
       AND wiring.version = $5 \
       AND wiring.gated_catalog_version = release_scope.catalog_version \
     WHERE EXISTS ( \
           SELECT 1 \
             FROM catalog.release_components AS member \
            WHERE member.tenant_id = $1 \
              AND member.catalog_id = $2 \
              AND member.catalog_version = release_scope.catalog_version \
              AND member.wiring_id = $4 \
              AND member.wiring_version = $5 \
       ) \
) \
SELECT selected.version, selected.gated_catalog_version, \
       selected.graph_json::text, selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'catalog-id', $2::text, \
                       'catalog-version', component.catalog_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operation', component.operation, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects, \
                   'input-ports', component.input_ports, \
                   'output-ports', component.output_ports, \
                   'parameters', component.parameters \
               ) ORDER BY component.component, component.interface_version \
           ) FILTER (WHERE component.component IS NOT NULL), \
           '[]'::jsonb \
       )::text AS components \
  FROM selected \
  LEFT JOIN catalog.release_components AS member \
    ON member.tenant_id = $1 \
   AND member.catalog_id = $2 \
   AND member.catalog_version = selected.release_catalog_version \
   AND member.wiring_id = $4 \
   AND member.wiring_version = $5 \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = member.tenant_id \
   AND component.catalog_id = member.catalog_id \
   AND component.catalog_version = member.catalog_version \
   AND component.component_digest = member.component_digest \
 GROUP BY selected.version, selected.gated_catalog_version, \
          selected.graph_json, selected.wiring_hash";

/// Exact immutable candidate wiring selected by private management admission.
///
/// A candidate is neither the active environment pointer nor a member of the
/// serving release carried by this executor. The run supplies every immutable
/// coordinate that admission read from the same row.
pub const CANDIDATE_WIRING_SQL: &str = "\
WITH selected AS MATERIALIZED ( \
    SELECT wiring.version, wiring.gated_catalog_version, \
           wiring.graph_json, wiring.wiring_hash \
      FROM catalog.wirings AS wiring \
     WHERE wiring.tenant_id = $1 \
       AND wiring.catalog_id = $2 \
       AND wiring.wiring_id = $4 \
       AND wiring.version = $5 \
       AND wiring.gated_catalog_version = $6 \
       AND wiring.wiring_hash = $7 \
       AND $3::text <> '' \
), candidate_nodes AS MATERIALIZED ( \
    SELECT node.key AS node_id, component.component_digest, \
           component.component IS NOT NULL AS component_admitted \
      FROM selected \
      CROSS JOIN LATERAL jsonb_each( \
        CASE WHEN jsonb_typeof(selected.graph_json -> 'nodes') = 'object' \
             THEN selected.graph_json -> 'nodes' ELSE '{}'::jsonb END \
      ) AS node \
      LEFT JOIN catalog.component_library AS component \
        ON component.tenant_id = $1 \
       AND component.catalog_id = $2 \
       AND component.catalog_version = selected.gated_catalog_version \
       AND component.component = node.value ->> 'component' \
       AND component.interface_version = node.value ->> 'interface-version' \
       AND component.operation = node.value ->> 'operation' \
), node_summary AS MATERIALIZED ( \
    SELECT count(*) AS node_count, \
           count(*) FILTER (WHERE NOT component_admitted) AS invalid_node_count \
      FROM candidate_nodes \
), requirements AS MATERIALIZED ( \
    SELECT requirement.component_digest, requirement.store_alias, \
           requirement.requirement_hash \
      FROM (SELECT DISTINCT component_digest FROM candidate_nodes \
             WHERE component_admitted) AS candidate_component \
      JOIN catalog.connection_requirements AS requirement \
        ON requirement.tenant_id = $1 \
       AND requirement.artifact_hash IS NULL \
       AND requirement.requirement_name IS NULL \
       AND requirement.component_digest = candidate_component.component_digest \
), resolved_requirements AS MATERIALIZED ( \
    SELECT requirement.component_digest, requirement.store_alias, \
           requirement.requirement_hash, binding.instance_id, \
           instance.revision AS instance_revision, instance.requirement_type, \
           instance.contract, binding.validation_hash, generation.generation, \
           generation.definition_hash, generation.credential_set_handle \
      FROM requirements AS requirement \
      JOIN catalog.connection_bindings AS binding \
        ON binding.tenant_id = $1 \
       AND binding.catalog_id = $2 \
       AND binding.catalog_version = $6 \
       AND binding.artifact_hash IS NULL \
       AND binding.requirement_name IS NULL \
       AND binding.component_digest = requirement.component_digest \
       AND binding.store_alias = requirement.store_alias \
       AND binding.environment = $3 \
       AND binding.binding_status = 'active' \
       AND binding.validation_status = 'valid' \
      JOIN catalog.connection_instances AS instance \
        ON instance.tenant_id = binding.tenant_id \
       AND instance.environment = binding.environment \
       AND instance.instance_id = binding.instance_id \
       AND instance.lifecycle_status = 'enabled' \
       AND instance.active_generation IS NOT NULL \
      JOIN catalog.connection_generations AS generation \
        ON generation.tenant_id = instance.tenant_id \
       AND generation.environment = instance.environment \
       AND generation.instance_id = instance.instance_id \
       AND generation.generation = instance.active_generation \
), binding_world AS MATERIALIZED ( \
    SELECT count(requirement.component_digest) AS requirement_count, \
           count(resolved.component_digest) AS resolved_count, \
           COALESCE(jsonb_agg( \
             jsonb_build_object( \
               'component-digest', resolved.component_digest, \
               'store-alias', resolved.store_alias, \
               'requirement-hash', resolved.requirement_hash, \
               'instance-id', resolved.instance_id, \
               'instance-revision', resolved.instance_revision, \
               'requirement-type', resolved.requirement_type, \
               'contract', resolved.contract, \
               'validation-hash', resolved.validation_hash, \
               'generation', resolved.generation, \
               'definition-hash', resolved.definition_hash, \
               'credential-set-handle', resolved.credential_set_handle \
             ) ORDER BY resolved.component_digest, resolved.store_alias \
           ) FILTER (WHERE resolved.component_digest IS NOT NULL), '[]'::jsonb) \
             AS binding_world_json \
      FROM requirements AS requirement \
      LEFT JOIN resolved_requirements AS resolved \
        USING (component_digest, store_alias) \
) \
SELECT selected.version, selected.gated_catalog_version, \
       selected.graph_json::text, selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'catalog-id', $2::text, \
                       'catalog-version', component.catalog_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operation', component.operation, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects, \
                   'input-ports', component.input_ports, \
                   'output-ports', component.output_ports, \
                   'parameters', component.parameters \
               ) ORDER BY component.component, component.interface_version \
           ) FILTER (WHERE component.component IS NOT NULL), \
           '[]'::jsonb \
       )::text AS components, \
       node_summary.node_count, node_summary.invalid_node_count, \
       binding_world.requirement_count, binding_world.resolved_count, \
       binding_world.binding_world_json::text \
  FROM selected CROSS JOIN node_summary CROSS JOIN binding_world \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = $1 \
   AND component.catalog_id = $2 \
   AND component.catalog_version = selected.gated_catalog_version \
   AND EXISTS ( \
       SELECT 1 \
         FROM jsonb_each(selected.graph_json -> 'nodes') AS node(node_id, definition) \
        WHERE definition ->> 'component' = component.component \
          AND definition ->> 'interface-version' = component.interface_version \
          AND definition ->> 'operation' = component.operation \
   ) \
 GROUP BY selected.version, selected.gated_catalog_version, \
          selected.graph_json, selected.wiring_hash, \
          node_summary.node_count, node_summary.invalid_node_count, \
          binding_world.requirement_count, binding_world.resolved_count, \
          binding_world.binding_world_json";

/// Prove that every component-grain requirement in the synchronous release
/// closure has one exact usable environment binding.
pub(crate) const RELEASE_COMPONENT_BINDINGS_READY_SQL: &str = "\
SELECT NOT EXISTS ( \
    SELECT 1 \
      FROM catalog.connection_requirements AS requirement \
     WHERE requirement.tenant_id = $1 \
       AND requirement.artifact_hash IS NULL \
       AND requirement.requirement_name IS NULL \
       AND requirement.component_digest = ANY($5::text[]) \
       AND NOT EXISTS ( \
           SELECT 1 \
             FROM catalog.connection_bindings AS binding \
             JOIN catalog.connection_instances AS instance \
               ON instance.tenant_id = binding.tenant_id \
              AND instance.environment = binding.environment \
              AND instance.instance_id = binding.instance_id \
             JOIN catalog.connection_generations AS generation \
               ON generation.tenant_id = instance.tenant_id \
              AND generation.environment = instance.environment \
              AND generation.instance_id = instance.instance_id \
              AND generation.generation = instance.active_generation \
            WHERE binding.tenant_id = requirement.tenant_id \
              AND binding.catalog_id = $2 \
              AND binding.catalog_version = $3 \
              AND binding.environment = $4 \
              AND binding.artifact_hash IS NULL \
              AND binding.requirement_name IS NULL \
              AND binding.component_digest = requirement.component_digest \
              AND binding.store_alias = requirement.store_alias \
              AND binding.binding_status = 'active' \
              AND binding.validation_status = 'valid' \
              AND instance.lifecycle_status = 'enabled' \
              AND instance.active_generation IS NOT NULL \
       ) \
)";

/// A typed active wiring ready for the router and component source.
#[derive(Debug, Clone)]
pub struct ResolvedActiveWiring {
    pub version: u32,
    pub catalog_version: u32,
    pub graph_hash: Arc<str>,
    pub wiring: Wiring,
    pub components: Arc<[AdmittedComponent]>,
}

/// Exact outcome of re-reading a frozen candidate before component execution.
#[derive(Debug)]
pub enum CandidateWiringResolution {
    Resolved(ResolvedActiveWiring),
    Missing,
    InvalidDefinition,
    BindingWorldUnavailable,
    BindingWorldDrift,
}

impl ResolvedActiveWiring {
    /// The admitted fact whose digest selected one router node.
    pub fn component_by_digest(&self, digest: &str) -> Option<&AdmittedComponent> {
        self.components
            .iter()
            .find(|component| component.component_digest == digest)
    }
}

impl WamnPostgres {
    /// Resolve and lower one exact active wiring version in one SQL snapshot.
    ///
    /// `Ok(None)` means the requested identity/version is not the enabled
    /// pointer. Malformed or contradictory persisted facts are errors, never a
    /// miss that a caller may reinterpret.
    #[expect(
        clippy::too_many_arguments,
        reason = "the complete activation key plus project are independent trusted coordinates"
    )]
    pub async fn resolve_active_wiring(
        &self,
        project: &str,
        tenant_id: &str,
        catalog_id: &str,
        environment: &str,
        wiring_id: &str,
        wiring_version: u32,
    ) -> anyhow::Result<Option<ResolvedActiveWiring>> {
        anyhow::ensure!(wiring_version > 0, "active-wiring-version-zero");
        let wiring_version = i32::try_from(wiring_version)
            .context("active wiring version exceeds PostgreSQL int")?;
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                tenant_id,
                None,
                None,
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let params: [&(dyn ToSql + Sync); 5] = [
            &tenant_id,
            &catalog_id,
            &environment,
            &wiring_id,
            &wiring_version,
        ];
        let selected = connection
            .query_opt(ACTIVE_WIRING_SQL, &params)
            .await
            .context("query exact active wiring");
        let result = match selected {
            Ok(None) => Ok(None),
            Ok(Some(row)) => {
                decode_active_wiring(tenant_id, catalog_id, environment, wiring_id, &row).map(Some)
            }
            Err(error) => Err(error),
        };

        match result {
            Ok(resolved) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(error).context("commit active wiring snapshot");
                }
                Ok(resolved)
            }
            Err(error) => {
                if connection.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Resolve the exact immutable wiring version frozen onto a queued run.
    ///
    /// This path is intentionally independent of `wiring_activation`: a flip
    /// after admission changes new direct deliveries, not history already
    /// accepted by the queue. The exact format-2 release snapshot scopes the
    /// version to the carried environment and release identity.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frozen release and wiring coordinates are independent trusted facts"
    )]
    pub async fn resolve_release_wiring(
        &self,
        project: &str,
        tenant_id: &str,
        catalog_id: &str,
        environment: &str,
        release_version: u32,
        manifest_digest: &str,
        wiring_id: &str,
        wiring_version: u32,
    ) -> anyhow::Result<Option<ResolvedActiveWiring>> {
        anyhow::ensure!(release_version > 0, "release-version-zero");
        anyhow::ensure!(wiring_version > 0, "release-wiring-version-zero");
        let release_version =
            i32::try_from(release_version).context("release version exceeds PostgreSQL int")?;
        let wiring_version = i32::try_from(wiring_version)
            .context("release wiring version exceeds PostgreSQL int")?;
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                tenant_id,
                None,
                None,
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let params: [&(dyn ToSql + Sync); 7] = [
            &tenant_id,
            &catalog_id,
            &environment,
            &wiring_id,
            &wiring_version,
            &release_version,
            &manifest_digest,
        ];
        let selected = connection
            .query_opt(RELEASE_WIRING_SQL, &params)
            .await
            .context("query exact release wiring");
        let result = match selected {
            Ok(None) => Ok(None),
            Ok(Some(row)) => {
                decode_active_wiring(tenant_id, catalog_id, environment, wiring_id, &row).map(Some)
            }
            Err(error) => Err(error),
        };

        match result {
            Ok(resolved) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(error).context("commit release wiring snapshot");
                }
                Ok(resolved)
            }
            Err(error) => {
                if connection.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Resolve one report-owned candidate without consulting activation or a
    /// serving-manifest projection.
    #[expect(
        clippy::too_many_arguments,
        reason = "the complete persisted candidate coordinate is independently trusted"
    )]
    pub async fn resolve_candidate_wiring(
        &self,
        project: &str,
        tenant_id: &str,
        catalog_id: &str,
        environment: &str,
        catalog_version: u32,
        wiring_id: &str,
        wiring_version: u32,
        wiring_hash: &str,
        expected_binding_world: &CandidateBindingWorld,
    ) -> anyhow::Result<CandidateWiringResolution> {
        anyhow::ensure!(catalog_version > 0, "candidate-catalog-version-zero");
        anyhow::ensure!(wiring_version > 0, "candidate-wiring-version-zero");
        anyhow::ensure!(!wiring_hash.is_empty(), "candidate-wiring-hash-empty");
        let catalog_version = i32::try_from(catalog_version)
            .context("candidate catalog version exceeds PostgreSQL int")?;
        let wiring_version = i32::try_from(wiring_version)
            .context("candidate wiring version exceeds PostgreSQL int")?;
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                tenant_id,
                None,
                None,
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let params: [&(dyn ToSql + Sync); 7] = [
            &tenant_id,
            &catalog_id,
            &environment,
            &wiring_id,
            &wiring_version,
            &catalog_version,
            &wiring_hash,
        ];
        let selected = connection
            .query_opt(CANDIDATE_WIRING_SQL, &params)
            .await
            .context("query exact candidate wiring");
        let result = match selected {
            Ok(None) => Ok(CandidateWiringResolution::Missing),
            Ok(Some(row)) => (|| -> anyhow::Result<CandidateWiringResolution> {
                let node_count: i64 = row.try_get(5).context("decode candidate node count")?;
                let invalid_node_count: i64 = row
                    .try_get(6)
                    .context("decode invalid candidate node count")?;
                let requirement_count: i64 = row
                    .try_get(7)
                    .context("decode candidate requirement count")?;
                let resolved_count: i64 = row
                    .try_get(8)
                    .context("decode resolved candidate requirement count")?;
                let live_binding_world: String = row
                    .try_get(9)
                    .context("decode live candidate binding world")?;
                if node_count == 0 || invalid_node_count != 0 {
                    Ok(CandidateWiringResolution::InvalidDefinition)
                } else if requirement_count != resolved_count {
                    Ok(CandidateWiringResolution::BindingWorldUnavailable)
                } else {
                    let live_binding_world = serde_json::from_str(&live_binding_world)
                        .context("parse live candidate binding world")
                        .and_then(CandidateBindingWorld::from_json);
                    match live_binding_world {
                        Ok(live_binding_world) if &live_binding_world == expected_binding_world => {
                            match decode_active_wiring(
                                tenant_id,
                                catalog_id,
                                environment,
                                wiring_id,
                                &row,
                            ) {
                                Ok(resolved) => Ok(CandidateWiringResolution::Resolved(resolved)),
                                Err(_) => Ok(CandidateWiringResolution::InvalidDefinition),
                            }
                        }
                        Ok(_) => Ok(CandidateWiringResolution::BindingWorldDrift),
                        Err(_) => Ok(CandidateWiringResolution::InvalidDefinition),
                    }
                }
            })(),
            Err(error) => Err(error),
        };
        match result {
            Ok(resolved) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(error).context("commit candidate wiring snapshot");
                }
                Ok(resolved)
            }
            Err(error) => {
                if connection.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Check the exact component requirements selected by request readiness.
    ///
    /// An empty digest set is a background-only release and performs no store
    /// call. Otherwise the check shares the driver's existing platform pool and
    /// tenant claim; missing rows, unavailable storage and malformed results are
    /// errors, while an ordinary unbound requirement is `Ok(false)`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the release scope and selected component set are independent trusted facts"
    )]
    pub async fn release_component_bindings_ready(
        &self,
        project: &str,
        tenant_id: &str,
        catalog_id: &str,
        environment: &str,
        catalog_version: u32,
        component_digests: &[String],
    ) -> anyhow::Result<bool> {
        if component_digests.is_empty() {
            return Ok(true);
        }
        anyhow::ensure!(catalog_version > 0, "release-catalog-version-zero");
        let catalog_version = i32::try_from(catalog_version)
            .context("release catalog version exceeds PostgreSQL int")?;
        let component_digests = component_digests.to_vec();
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                tenant_id,
                None,
                None,
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let params: [&(dyn ToSql + Sync); 5] = [
            &tenant_id,
            &catalog_id,
            &catalog_version,
            &environment,
            &component_digests,
        ];
        let result = connection
            .query_one(RELEASE_COMPONENT_BINDINGS_READY_SQL, &params)
            .await
            .context("query synchronous release connection bindings")
            .and_then(|row| row.try_get(0).context("decode release binding readiness"));

        match result {
            Ok(ready) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(error).context("commit release binding readiness snapshot");
                }
                Ok(ready)
            }
            Err(error) => {
                if connection.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }
}

fn decode_active_wiring(
    tenant_id: &str,
    catalog_id: &str,
    environment: &str,
    wiring_id: &str,
    row: &tokio_postgres::Row,
) -> anyhow::Result<ResolvedActiveWiring> {
    let version: i32 = row.try_get(0).context("decode active wiring version")?;
    let version = u32::try_from(version).context("active wiring version is not positive")?;
    let gated_catalog_version: i32 = row.try_get(1).context("decode gated catalog version")?;
    let gated_catalog_version =
        u32::try_from(gated_catalog_version).context("gated catalog version is not positive")?;
    let graph_json: String = row.try_get(2).context("decode wiring document JSON")?;
    let graph_json = serde_json::from_str(&graph_json).context("parse wiring document JSON")?;
    let document = WiringDocument::parse(&graph_json).context("validate wiring document")?;
    anyhow::ensure!(document.wiring_id == wiring_id, "active-wiring-id-mismatch");
    anyhow::ensure!(
        document.version == version,
        "active-wiring-version-mismatch"
    );
    let graph_hash: String = row.try_get(3).context("decode wiring graph hash")?;
    anyhow::ensure!(
        document.wiring_hash().as_str() == graph_hash,
        "active-wiring-hash-mismatch"
    );

    let components: String = row
        .try_get(4)
        .context("decode active wiring component facts")?;
    let components: Vec<AdmittedComponent> =
        serde_json::from_str(&components).context("parse active wiring component facts")?;
    let components = components_used_by_document(&document, components);
    verify_served_effect_projections(&components)?;
    let scope = WiringScope {
        tenant_id,
        catalog_id,
        environment,
    };
    let operations: Vec<_> = components.iter().map(project_component_operation).collect();
    let wiring = lower_active_wiring(
        GatedActiveWiring {
            scope,
            gated_catalog_version,
            document: &document,
        },
        ScopedWiringOperationFacts {
            scope,
            catalog_version: gated_catalog_version,
            operations: &operations,
        },
    )
    .context("lower active wiring")?;

    Ok(ResolvedActiveWiring {
        version,
        catalog_version: gated_catalog_version,
        graph_hash: Arc::from(graph_hash),
        wiring,
        components: components.into(),
    })
}

/// Refuse to serve a component fact whose effects its own audited imports do
/// not derive.
///
/// This is the DELIVERY path. The ctl readers refuse the same row at
/// publication time, where an operator sees the failure and can act on it; here
/// a fabricated projection would simply be trusted. `wamn-0h0g.21.10` defaulted
/// every pre-migration row to `'[]'` — the positive claim of purity — so the
/// projection is re-derived from the row's own attested imports rather than
/// believed.
fn verify_served_effect_projections(components: &[AdmittedComponent]) -> anyhow::Result<()> {
    for component in components {
        wamn_catalog::verify_stored_effect_projection(component).with_context(|| {
            format!(
                "component {:?} stores an effect projection its audited imports do not derive; \
                 re-admit it through the validator",
                component.component
            )
        })?;
    }
    Ok(())
}

fn components_used_by_document(
    document: &WiringDocument,
    components: Vec<AdmittedComponent>,
) -> Vec<AdmittedComponent> {
    components
        .into_iter()
        .filter(|component| {
            document.nodes.values().any(|node| {
                node.component == component.component
                    && node.interface_version == component.interface_version
                    && node.operation == component.operation
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        ComponentCatalogScope, ComponentDeclaration, ComponentPortDeclaration,
        normalize_component_fact,
    };

    use super::*;

    fn component(name: &str, operation: &str, digest_byte: char) -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentCatalogScope {
                    tenant_id: "tenant-a".to_owned(),
                    catalog_id: "orders".to_owned(),
                    catalog_version: 7,
                },
                component: name.to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: operation.to_owned(),
                input_ports: vec![ComponentPortDeclaration {
                    name: "input".to_owned(),
                    schema: json!({}),
                }],
                output_ports: Vec::new(),
                parameters: Vec::new(),
                connections: Vec::new(),
            },
            format!("sha256:{}", digest_byte.to_string().repeat(64)),
            ["wasi:logging/logging@0.1.0".to_owned()],
            Vec::new(),
        )
        .expect("fixture component admits")
        .component
    }

    fn document() -> WiringDocument {
        WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": "create-order",
            "version": 3,
            "entry": "create",
            "nodes": {
                "create": {
                    "component": "entity",
                    "interface-version": "0.1.0",
                    "operation": "create"
                }
            }
        }))
        .expect("fixture wiring admits")
    }

    #[test]
    fn unrelated_catalog_component_is_absent_from_resolved_facts() {
        let used = component("entity", "create", 'a');
        let unrelated = component("logging", "write", 'b');

        let filtered = components_used_by_document(&document(), vec![used.clone(), unrelated]);

        assert_eq!(filtered, vec![used]);
    }

    #[test]
    fn active_query_selects_only_exact_graph_operation_tuples() {
        assert!(ACTIVE_WIRING_SQL.contains("jsonb_each(selected.graph_json -> 'nodes')"));
        assert!(ACTIVE_WIRING_SQL.contains("definition ->> 'component' = component.component"));
        assert!(
            ACTIVE_WIRING_SQL
                .contains("definition ->> 'interface-version' = component.interface_version")
        );
        assert!(ACTIVE_WIRING_SQL.contains("definition ->> 'operation' = component.operation"));
    }

    #[test]
    fn queued_query_uses_release_snapshot_and_never_the_active_pointer() {
        assert!(RELEASE_WIRING_SQL.contains("release_manifest_v2_snapshots"));
        assert!(RELEASE_WIRING_SQL.contains("release_components"));
        assert!(!RELEASE_WIRING_SQL.contains("wiring_activation"));
    }

    #[test]
    fn candidate_query_rederives_the_complete_binding_world_without_activation() {
        for predicate in [
            "wiring.gated_catalog_version = $6",
            "wiring.wiring_hash = $7",
            "binding.environment = $3",
            "binding.binding_status = 'active'",
            "binding.validation_status = 'valid'",
            "instance.lifecycle_status = 'enabled'",
            "generation.generation = instance.active_generation",
            "ORDER BY resolved.component_digest, resolved.store_alias",
            "binding_world.requirement_count",
            "binding_world.resolved_count",
            "binding_world.binding_world_json::text",
        ] {
            assert!(
                CANDIDATE_WIRING_SQL.contains(predicate),
                "missing candidate snapshot predicate {predicate:?}"
            );
        }
        assert!(!CANDIDATE_WIRING_SQL.contains("wiring_activation"));
        assert!(!CANDIDATE_WIRING_SQL.contains("release_components"));
    }

    #[test]
    fn readiness_query_requires_the_exact_component_grain_and_live_binding() {
        for predicate in [
            "requirement.artifact_hash IS NULL",
            "requirement.requirement_name IS NULL",
            "requirement.component_digest = ANY($5::text[])",
            "binding.catalog_id = $2",
            "binding.catalog_version = $3",
            "binding.environment = $4",
            "binding.artifact_hash IS NULL",
            "binding.requirement_name IS NULL",
            "binding.component_digest = requirement.component_digest",
            "binding.store_alias = requirement.store_alias",
            "binding.binding_status = 'active'",
            "binding.validation_status = 'valid'",
            "instance.lifecycle_status = 'enabled'",
            "generation.generation = instance.active_generation",
        ] {
            assert!(
                RELEASE_COMPONENT_BINDINGS_READY_SQL.contains(predicate),
                "missing readiness predicate {predicate:?}"
            );
        }
    }

    /// A component fact exactly as the wamn-0h0g.21.9 converge ALTER leaves
    /// one: the audited imports it was admitted with, and the `'[]'` the
    /// DEFAULT wrote over them. Built by hand because admission itself now
    /// refuses this shape.
    fn migration_defaulted(imports: &[&str]) -> AdmittedComponent {
        AdmittedComponent {
            scope: ComponentCatalogScope {
                tenant_id: "tenant-a".to_owned(),
                catalog_id: "orders".to_owned(),
                catalog_version: 7,
            },
            component: "transform".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operation: "map".to_owned(),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: imports.iter().map(|name| (*name).to_owned()).collect(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
        }
    }

    /// wamn-0h0g.21.11. The delivery path must refuse the fabricated purity
    /// claim, not merely the publication path. Deleting the call in
    /// `resolve_active_wiring` leaves this failing.
    #[test]
    fn the_serving_path_refuses_an_effect_projection_no_validator_derived() {
        let served = vec![migration_defaulted(&["wamn:postgres/client@0.1.0"])];

        let error = verify_served_effect_projections(&served)
            .expect_err("an underived purity claim is refused before it is served");

        assert!(
            error
                .to_string()
                .contains("re-admit it through the validator"),
            "unexpected refusal: {error}"
        );
    }

    /// The other half: a row whose `'[]'` is the value its own imports derive
    /// is not fabricated, and must keep serving. This is what scopes the
    /// refusal to exactly the rows a validator never produced.
    #[test]
    fn the_serving_path_admits_a_projection_its_imports_derive() {
        let served = vec![migration_defaulted(&["wasi:clocks/monotonic-clock@0.2.3"])];

        verify_served_effect_projections(&served).expect("a derived pure projection serves");
    }
}
