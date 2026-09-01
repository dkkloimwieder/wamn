//! Host-owned active-wiring resolution through the existing platform pool.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
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
    SELECT wiring.version, head.effective_release_id, member.package_version, \
           wiring.graph_json, wiring.wiring_hash \
      FROM catalog.wiring_activation AS active \
      JOIN catalog.effective_release_heads AS head \
        ON head.tenant_id = active.tenant_id \
       AND head.environment = active.environment \
      JOIN catalog.effective_release_packages AS member \
        ON member.tenant_id = head.tenant_id \
       AND member.effective_release_id = head.effective_release_id \
       AND member.package_id = active.package_id \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = active.tenant_id \
       AND wiring.package_id = active.package_id \
       AND wiring.package_version = member.package_version \
       AND wiring.wiring_id = active.wiring_id \
       AND wiring.version = $5 \
       AND wiring.wiring_hash = active.confirmed_definition_hash \
     WHERE active.tenant_id = $1 \
       AND active.package_id = $2 \
       AND active.environment = $3 \
       AND active.wiring_id = $4 \
       AND active.enabled \
       AND NOT EXISTS ( \
           SELECT 1 \
             FROM catalog.wiring_tombstones AS dead \
            WHERE dead.tenant_id = active.tenant_id \
              AND dead.package_id = active.package_id \
              AND dead.environment = active.environment \
              AND dead.wiring_id = active.wiring_id \
       ) \
) \
SELECT selected.version, \
       selected.effective_release_id, \
       selected.package_version, \
       selected.graph_json::text, \
       selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'node-id', member.node_id, \
                   'component', jsonb_build_object( \
                       'scope', jsonb_build_object( \
                           'tenant-id', $1::text, \
                           'package-id', component.package_id, \
                           'package-version', component.package_version \
                       ), \
                       'component', component.component, \
                       'interface-version', component.interface_version, \
                       'operation', component.operation, \
                       'registered-operation', component.registered_operation, \
                       'component-digest', component.component_digest, \
                       'imports', component.imports, \
                       'imports-fingerprint', component.imports_fingerprint, \
                       'effects', component.effects, \
                       'input-ports', component.input_ports, \
                       'output-ports', component.output_ports, \
                       'parameters', component.parameters \
                   ) \
               ) ORDER BY member.node_id COLLATE \"C\" \
           ) FILTER (WHERE member.node_id IS NOT NULL), \
           '[]'::jsonb \
       )::text AS node_components \
  FROM selected \
  LEFT JOIN catalog.release_components AS member \
    ON member.tenant_id = $1 \
   AND member.effective_release_id = selected.effective_release_id \
   AND member.wiring_package_id = $2 \
   AND member.wiring_package_version = selected.package_version \
   AND member.wiring_id = $4 \
   AND member.wiring_version = $5 \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = member.tenant_id \
   AND component.package_id = member.package_id \
   AND component.package_version = member.package_version \
   AND component.component_digest = member.component_digest \
 GROUP BY selected.version, selected.effective_release_id, \
          selected.package_version, \
          selected.graph_json, selected.wiring_hash";

/// The immutable-version snapshot behind a queued delivery. Unlike
/// [`ACTIVE_WIRING_SQL`], this deliberately does not consult the mutable
/// activation pointer: admission already froze the exact wiring version.
/// Exact package membership and the verified format-3 snapshot keep that
/// historical version scoped to the carried tenant/package/environment release.
pub const RELEASE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT snapshot.effective_release_id, member.package_version \
      FROM catalog.release_manifest_v3_snapshots AS snapshot \
      JOIN catalog.effective_release_packages AS member \
        ON member.tenant_id = snapshot.tenant_id \
       AND member.effective_release_id = snapshot.effective_release_id \
       AND member.package_id = $2 \
     WHERE snapshot.tenant_id = $1 \
       AND snapshot.effective_release_id = $6 \
       AND snapshot.manifest_digest = $7 \
       AND convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb \
             #>> '{release,environment}' = $3 \
), selected AS MATERIALIZED ( \
    SELECT wiring.version, release_scope.effective_release_id, \
           release_scope.package_version, \
           wiring.graph_json, wiring.wiring_hash, \
           release_scope.effective_release_id AS release_id \
      FROM release_scope \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = $1 \
       AND wiring.package_id = $2 \
       AND wiring.package_version = release_scope.package_version \
       AND wiring.wiring_id = $4 \
       AND wiring.version = $5 \
     WHERE EXISTS ( \
           SELECT 1 \
             FROM catalog.release_components AS member \
            WHERE member.tenant_id = $1 \
              AND member.effective_release_id = release_scope.effective_release_id \
              AND member.wiring_package_id = $2 \
              AND member.wiring_package_version = release_scope.package_version \
              AND member.wiring_id = $4 \
              AND member.wiring_version = $5 \
       ) \
) \
SELECT selected.version, selected.effective_release_id, \
       selected.package_version, \
       selected.graph_json::text, selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'node-id', member.node_id, \
                   'component', jsonb_build_object( \
                       'scope', jsonb_build_object( \
                           'tenant-id', $1::text, \
                           'package-id', component.package_id, \
                           'package-version', component.package_version \
                       ), \
                       'component', component.component, \
                       'interface-version', component.interface_version, \
                       'operation', component.operation, \
                       'registered-operation', component.registered_operation, \
                       'component-digest', component.component_digest, \
                       'imports', component.imports, \
                       'imports-fingerprint', component.imports_fingerprint, \
                       'effects', component.effects, \
                       'input-ports', component.input_ports, \
                       'output-ports', component.output_ports, \
                       'parameters', component.parameters \
                   ) \
               ) ORDER BY member.node_id COLLATE \"C\" \
           ) FILTER (WHERE member.node_id IS NOT NULL), \
           '[]'::jsonb \
       )::text AS node_components \
  FROM selected \
  LEFT JOIN catalog.release_components AS member \
    ON member.tenant_id = $1 \
   AND member.effective_release_id = selected.release_id \
   AND member.wiring_package_id = $2 \
   AND member.wiring_package_version = selected.package_version \
   AND member.wiring_id = $4 \
   AND member.wiring_version = $5 \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = member.tenant_id \
   AND component.package_id = member.package_id \
   AND component.package_version = member.package_version \
   AND component.component_digest = member.component_digest \
 GROUP BY selected.version, selected.effective_release_id, selected.package_version, \
          selected.graph_json, selected.wiring_hash";

/// Exact immutable candidate wiring selected by private management admission.
///
/// A candidate is neither the active environment pointer nor a member of the
/// serving release carried by this executor. The run supplies every immutable
/// coordinate that admission read from the same row.
pub const CANDIDATE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT member.package_version \
      FROM catalog.effective_release_packages AS member \
     WHERE member.tenant_id = $1 \
       AND member.effective_release_id = $6 \
       AND member.package_id = $2 \
), selected AS MATERIALIZED ( \
    SELECT wiring.version, $6::int AS effective_release_id, \
           release_scope.package_version, \
           wiring.graph_json, wiring.wiring_hash \
      FROM release_scope \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = $1 \
       AND wiring.package_id = $2 \
       AND wiring.package_version = release_scope.package_version \
     WHERE wiring.tenant_id = $1 \
       AND wiring.wiring_id = $4 \
       AND wiring.version = $5 \
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
       AND component.package_id = $2 \
       AND component.package_version = selected.package_version \
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
       AND binding.effective_release_id = $6 \
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
SELECT selected.version, selected.effective_release_id, \
       selected.package_version, \
       selected.graph_json::text, selected.wiring_hash, \
       COALESCE( \
           jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'package-id', $2::text, \
                       'package-version', component.package_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operation', component.operation, \
                   'registered-operation', component.registered_operation, \
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
   AND component.package_id = $2 \
   AND component.package_version = selected.package_version \
   AND EXISTS ( \
       SELECT 1 \
         FROM jsonb_each(selected.graph_json -> 'nodes') AS node(node_id, definition) \
        WHERE definition ->> 'component' = component.component \
          AND definition ->> 'interface-version' = component.interface_version \
          AND definition ->> 'operation' = component.operation \
   ) \
 GROUP BY selected.version, selected.effective_release_id, selected.package_version, \
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
       AND requirement.component_digest = ANY($4::text[]) \
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
              AND binding.effective_release_id = $2 \
              AND binding.environment = $3 \
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
    pub effective_release_id: u32,
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
        package_id: &str,
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
            &package_id,
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
                decode_released_wiring(tenant_id, package_id, environment, wiring_id, &row)
                    .map(Some)
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
    /// accepted by the queue. The exact format-3 release snapshot scopes the
    /// version to the carried environment and release identity.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frozen release and wiring coordinates are independent trusted facts"
    )]
    pub async fn resolve_release_wiring(
        &self,
        project: &str,
        tenant_id: &str,
        package_id: &str,
        environment: &str,
        effective_release_id: u32,
        manifest_digest: &str,
        wiring_id: &str,
        wiring_version: u32,
    ) -> anyhow::Result<Option<ResolvedActiveWiring>> {
        anyhow::ensure!(effective_release_id > 0, "effective-release-id-zero");
        anyhow::ensure!(wiring_version > 0, "release-wiring-version-zero");
        let effective_release_id = i32::try_from(effective_release_id)
            .context("effective release id exceeds PostgreSQL int")?;
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
            &package_id,
            &environment,
            &wiring_id,
            &wiring_version,
            &effective_release_id,
            &manifest_digest,
        ];
        let selected = connection
            .query_opt(RELEASE_WIRING_SQL, &params)
            .await
            .context("query exact release wiring");
        let result = match selected {
            Ok(None) => Ok(None),
            Ok(Some(row)) => {
                decode_released_wiring(tenant_id, package_id, environment, wiring_id, &row)
                    .map(Some)
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
        package_id: &str,
        environment: &str,
        effective_release_id: u32,
        wiring_id: &str,
        wiring_version: u32,
        wiring_hash: &str,
        expected_binding_world: &CandidateBindingWorld,
    ) -> anyhow::Result<CandidateWiringResolution> {
        anyhow::ensure!(
            effective_release_id > 0,
            "candidate-effective-release-id-zero"
        );
        anyhow::ensure!(wiring_version > 0, "candidate-wiring-version-zero");
        anyhow::ensure!(!wiring_hash.is_empty(), "candidate-wiring-hash-empty");
        let effective_release_id = i32::try_from(effective_release_id)
            .context("candidate effective release id exceeds PostgreSQL int")?;
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
            &package_id,
            &environment,
            &wiring_id,
            &wiring_version,
            &effective_release_id,
            &wiring_hash,
        ];
        let selected = connection
            .query_opt(CANDIDATE_WIRING_SQL, &params)
            .await
            .context("query exact candidate wiring");
        let result = match selected {
            Ok(None) => Ok(CandidateWiringResolution::Missing),
            Ok(Some(row)) => (|| -> anyhow::Result<CandidateWiringResolution> {
                let node_count: i64 = row.try_get(6).context("decode candidate node count")?;
                let invalid_node_count: i64 = row
                    .try_get(7)
                    .context("decode invalid candidate node count")?;
                let requirement_count: i64 = row
                    .try_get(8)
                    .context("decode candidate requirement count")?;
                let resolved_count: i64 = row
                    .try_get(9)
                    .context("decode resolved candidate requirement count")?;
                let live_binding_world: String = row
                    .try_get(10)
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
                                package_id,
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
        effective_release_id: u32,
        environment: &str,
        component_digests: &[String],
    ) -> anyhow::Result<bool> {
        if component_digests.is_empty() {
            return Ok(true);
        }
        anyhow::ensure!(effective_release_id > 0, "effective-release-id-zero");
        let effective_release_id = i32::try_from(effective_release_id)
            .context("effective release id exceeds PostgreSQL int")?;
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

        let params: [&(dyn ToSql + Sync); 4] = [
            &tenant_id,
            &effective_release_id,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ResolvedNodeComponent {
    node_id: String,
    component: AdmittedComponent,
}

struct DecodedWiring {
    version: u32,
    effective_release_id: u32,
    package_version: String,
    graph_hash: String,
    document: WiringDocument,
}

fn decode_wiring(wiring_id: &str, row: &tokio_postgres::Row) -> anyhow::Result<DecodedWiring> {
    let version: i32 = row.try_get(0).context("decode active wiring version")?;
    let version = u32::try_from(version).context("active wiring version is not positive")?;
    let effective_release_id: i32 = row.try_get(1).context("decode effective release id")?;
    let effective_release_id =
        u32::try_from(effective_release_id).context("effective release id is not positive")?;
    let package_version: String = row.try_get(2).context("decode package version")?;
    let graph_json: String = row.try_get(3).context("decode wiring document JSON")?;
    let graph_json = serde_json::from_str(&graph_json).context("parse wiring document JSON")?;
    let document = WiringDocument::parse(&graph_json).context("validate wiring document")?;
    anyhow::ensure!(document.wiring_id == wiring_id, "active-wiring-id-mismatch");
    anyhow::ensure!(
        document.version == version,
        "active-wiring-version-mismatch"
    );
    let graph_hash: String = row.try_get(4).context("decode wiring graph hash")?;
    anyhow::ensure!(
        document.wiring_hash().as_str() == graph_hash,
        "active-wiring-hash-mismatch"
    );
    Ok(DecodedWiring {
        version,
        effective_release_id,
        package_version,
        graph_hash,
        document,
    })
}

fn decode_released_wiring(
    tenant_id: &str,
    package_id: &str,
    environment: &str,
    wiring_id: &str,
    row: &tokio_postgres::Row,
) -> anyhow::Result<ResolvedActiveWiring> {
    let decoded = decode_wiring(wiring_id, row)?;
    let node_components: String = row
        .try_get(5)
        .context("decode release wiring node component facts")?;
    let node_components: Vec<ResolvedNodeComponent> = serde_json::from_str(&node_components)
        .context("parse release wiring node component facts")?;
    let node_components = node_components
        .into_iter()
        .map(|binding| (binding.node_id, binding.component))
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        node_components.len() == decoded.document.nodes.len(),
        "release-wiring-node-closure-incomplete"
    );
    lower_resolved_wiring(tenant_id, package_id, environment, decoded, node_components)
}

fn decode_active_wiring(
    tenant_id: &str,
    package_id: &str,
    environment: &str,
    wiring_id: &str,
    row: &tokio_postgres::Row,
) -> anyhow::Result<ResolvedActiveWiring> {
    let decoded = decode_wiring(wiring_id, row)?;
    anyhow::ensure!(
        decoded
            .document
            .nodes
            .values()
            .all(|node| node.operation_dependency.is_none()),
        "candidate-operation-dependency-unresolved"
    );
    let components: String = row
        .try_get(5)
        .context("decode candidate wiring component facts")?;
    let components: Vec<AdmittedComponent> =
        serde_json::from_str(&components).context("parse candidate wiring component facts")?;
    let mut node_components = BTreeMap::new();
    for (node_id, node) in &decoded.document.nodes {
        let mut matches = components.iter().filter(|component| {
            node.component == component.component
                && node.interface_version == component.interface_version
                && node.operation == component.operation
        });
        let component = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("candidate-wiring-node-component-missing"))?;
        anyhow::ensure!(
            matches.next().is_none(),
            "candidate-wiring-node-component-ambiguous"
        );
        node_components.insert(node_id.clone(), component.clone());
    }
    lower_resolved_wiring(tenant_id, package_id, environment, decoded, node_components)
}

fn lower_resolved_wiring(
    tenant_id: &str,
    package_id: &str,
    environment: &str,
    decoded: DecodedWiring,
    resolved: BTreeMap<String, AdmittedComponent>,
) -> anyhow::Result<ResolvedActiveWiring> {
    let mut executable = decoded.document.clone();
    let mut operations = Vec::with_capacity(resolved.len());
    let mut components = Vec::new();
    for (node_id, component) in resolved {
        let node = decoded
            .document
            .nodes
            .get(&node_id)
            .ok_or_else(|| anyhow::anyhow!("release-wiring-node-binding-extra"))?;
        anyhow::ensure!(
            node.component == component.component
                && node.interface_version == component.interface_version
                && node.operation == component.operation,
            "release-wiring-node-binding-mismatch"
        );
        let runtime_key = component.component_digest.clone();
        executable
            .nodes
            .get_mut(&node_id)
            .expect("the resolved node belongs to the cloned document")
            .component = runtime_key.clone();
        let mut operation = project_component_operation(&component);
        operation.component = runtime_key;
        if !operations.contains(&operation) {
            operations.push(operation);
        }
        if !components.contains(&component) {
            components.push(component.clone());
        }
    }
    verify_served_effect_projections(&components)?;
    let scope = WiringScope {
        tenant_id,
        package_id,
        environment,
    };
    let wiring = lower_active_wiring(
        GatedActiveWiring {
            scope,
            package_version: &decoded.package_version,
            document: &executable,
        },
        ScopedWiringOperationFacts {
            scope,
            package_version: &decoded.package_version,
            operations: &operations,
        },
    )
    .context("lower active wiring")?;

    Ok(ResolvedActiveWiring {
        version: decoded.version,
        effective_release_id: decoded.effective_release_id,
        graph_hash: Arc::from(decoded.graph_hash),
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        ComponentDeclaration, ComponentPackageScope, ComponentPortDeclaration,
        normalize_component_fact,
    };

    use super::*;

    fn component(name: &str, operation: &str, digest_byte: char) -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentPackageScope {
                    tenant_id: "tenant-a".to_owned(),
                    package_id: "orders".to_owned(),
                    package_version: "1.2.0".to_owned(),
                },
                component: name.to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: operation.to_owned(),
                registered_operation: None,
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

    #[test]
    fn same_local_name_nodes_keep_exact_target_package_identity() {
        let document = WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": "compose-orders",
            "version": 3,
            "entry": "base",
            "nodes": {
                "base": {
                    "component": "entity",
                    "interface-version": "0.1.0",
                    "operation": "create"
                },
                "overlay": {
                    "component": "entity",
                    "interface-version": "0.1.0",
                    "operation": "create",
                    "terminal": "respond"
                }
            },
            "edges": [{
                "from": "base",
                "from-port": "error",
                "to": "overlay",
                "to-port": "input"
            }]
        }))
        .expect("cross-package fixture wiring admits");
        let mut base = component("entity", "create", 'a');
        base.scope.package_id = "base".to_owned();
        base.scope.package_version = "1.0.0".to_owned();
        base.registered_operation = Some("base@1.0.0::entity.create".to_owned());
        let mut overlay = component("entity", "create", 'b');
        overlay.scope.package_id = "overlay".to_owned();
        overlay.scope.package_version = "2.0.0".to_owned();
        overlay.registered_operation = Some("overlay@2.0.0::entity.create".to_owned());
        let graph_hash = document.wiring_hash().as_str().to_owned();

        let resolved = lower_resolved_wiring(
            "tenant-a",
            "overlay",
            "prod",
            DecodedWiring {
                version: 3,
                effective_release_id: 7,
                package_version: "2.0.0".to_owned(),
                graph_hash,
                document,
            },
            BTreeMap::from([
                ("base".to_owned(), base.clone()),
                ("overlay".to_owned(), overlay.clone()),
            ]),
        )
        .expect("exact node targets lower");

        assert_eq!(
            resolved.wiring.node("base").unwrap().component,
            base.component_digest
        );
        assert_eq!(
            resolved.wiring.node("overlay").unwrap().component,
            overlay.component_digest
        );
        assert!(resolved.components.contains(&base));
        assert!(resolved.components.contains(&overlay));
    }

    #[test]
    fn queued_query_uses_release_snapshot_and_never_the_active_pointer() {
        assert!(RELEASE_WIRING_SQL.contains("release_manifest_v3_snapshots"));
        assert!(RELEASE_WIRING_SQL.contains("effective_release_packages"));
        assert!(RELEASE_WIRING_SQL.contains("release_components"));
        assert!(!RELEASE_WIRING_SQL.contains("wiring_activation"));
    }

    #[test]
    fn candidate_query_rederives_the_complete_binding_world_without_activation() {
        for predicate in [
            "member.effective_release_id = $6",
            "wiring.package_version = release_scope.package_version",
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
            "requirement.component_digest = ANY($4::text[])",
            "binding.effective_release_id = $2",
            "binding.environment = $3",
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
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "orders".to_owned(),
                package_version: "1.2.0".to_owned(),
            },
            component: "transform".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operation: "map".to_owned(),
            registered_operation: None,
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
