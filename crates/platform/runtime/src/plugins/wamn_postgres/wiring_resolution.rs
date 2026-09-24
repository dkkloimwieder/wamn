//! Released and candidate wiring resolution through the existing platform pool.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use tokio_postgres::types::ToSql;
use wamn_catalog::{AdmittedComponent, WiringDocument, validate_resolved_wiring_compatibility};
use wamn_run_state::AuthorityClass;

use super::{CandidateBindingWorld, WamnPostgres};

/// The immutable-version snapshot behind a released delivery. Admission
/// already froze the exact wiring version.
/// Exact package membership and the verified format-1 snapshot keep that
/// historical version scoped to the carried tenant/package/environment release.
pub const RELEASE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT snapshot.effective_release_id, member.package_version, \
           convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb AS manifest \
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
           wiring.graph_json, wiring.wiring_hash, release_scope.manifest, \
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
           (SELECT jsonb_agg( \
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
                       'operations', component.operations, \
                       'component-digest', component.component_digest, \
                       'imports', component.imports, \
                       'imports-fingerprint', component.imports_fingerprint, \
                       'effects', component.effects \
                   ) \
               ) ORDER BY member.node_id COLLATE \"C\" \
            ) \
              FROM catalog.release_components AS member \
              JOIN catalog.component_library AS component \
                ON component.tenant_id = member.tenant_id \
               AND component.package_id = member.package_id \
               AND component.package_version = member.package_version \
               AND component.component_digest = member.component_digest \
             WHERE member.tenant_id = $1 \
               AND member.effective_release_id = selected.release_id \
               AND member.wiring_package_id = $2 \
               AND member.wiring_package_version = selected.package_version \
               AND member.wiring_id = $4 \
               AND member.wiring_version = $5), \
           '[]'::jsonb \
       )::text AS node_components, \
       COALESCE( \
           (SELECT jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'package-id', component.package_id, \
                       'package-version', component.package_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operations', component.operations, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects \
               ) ORDER BY projected.ordinality \
            ) \
              FROM jsonb_array_elements(selected.manifest -> 'components') \
                   WITH ORDINALITY AS projected(definition, ordinality) \
              JOIN catalog.effective_release_packages AS release_package \
                ON release_package.tenant_id = $1 \
               AND release_package.effective_release_id = selected.release_id \
               AND release_package.package_id = projected.definition ->> 'package-id' \
              JOIN catalog.component_library AS component \
                ON component.tenant_id = release_package.tenant_id \
               AND component.package_id = release_package.package_id \
               AND component.package_version = release_package.package_version \
               AND component.component = projected.definition ->> 'component' \
               AND component.interface_version = projected.definition ->> 'interface-version' \
               AND component.component_digest = projected.definition ->> 'digest'), \
           '[]'::jsonb \
       )::text AS components, \
       jsonb_array_length(selected.manifest -> 'components') AS manifest_component_count \
  FROM selected";

/// The complete component list of the carried release, for a route.
///
/// A route walks no wiring, so it reads the component list alone: the same
/// projection of the verified release snapshot that [`RELEASE_WIRING_SQL`]
/// returns beside a wiring. The loaded application is one per release, so a
/// route and a wiring must load the same list.
pub const RELEASE_COMPONENTS_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT snapshot.effective_release_id, \
           convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb AS manifest \
      FROM catalog.release_manifest_v3_snapshots AS snapshot \
     WHERE snapshot.tenant_id = $1 \
       AND snapshot.effective_release_id = $3 \
       AND snapshot.manifest_digest = $4 \
       AND convert_from(snapshot.canonical_bytes, 'UTF8')::jsonb \
             #>> '{release,environment}' = $2 \
) \
SELECT COALESCE( \
           (SELECT jsonb_agg( \
               jsonb_build_object( \
                   'scope', jsonb_build_object( \
                       'tenant-id', $1::text, \
                       'package-id', component.package_id, \
                       'package-version', component.package_version \
                   ), \
                   'component', component.component, \
                   'interface-version', component.interface_version, \
                   'operations', component.operations, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects \
               ) ORDER BY projected.ordinality \
            ) \
              FROM jsonb_array_elements(release_scope.manifest -> 'components') \
                   WITH ORDINALITY AS projected(definition, ordinality) \
              JOIN catalog.effective_release_packages AS release_package \
                ON release_package.tenant_id = $1 \
               AND release_package.effective_release_id = release_scope.effective_release_id \
               AND release_package.package_id = projected.definition ->> 'package-id' \
              JOIN catalog.component_library AS component \
                ON component.tenant_id = release_package.tenant_id \
               AND component.package_id = release_package.package_id \
               AND component.package_version = release_package.package_version \
               AND component.component = projected.definition ->> 'component' \
               AND component.interface_version = projected.definition ->> 'interface-version' \
               AND component.component_digest = projected.definition ->> 'digest'), \
           '[]'::jsonb \
       )::text AS components, \
       jsonb_array_length(release_scope.manifest -> 'components') AS manifest_component_count \
  FROM release_scope";

/// Exact immutable candidate wiring selected by private management admission.
///
/// A candidate is neither the active environment pointer nor a member of the
/// serving release carried by this executor. The run supplies every immutable
/// coordinate that admission read from the same row. Parameter eight is the
/// frozen binding JSON for execution, or NULL to capture current admission inputs.
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
       AND component.operations ? (node.value ->> 'operation') \
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
           COALESCE((pin.value ->> 'instance-revision')::bigint, instance.revision) AS instance_revision, instance.requirement_type, \
           instance.contract, binding.validation_hash, generation.generation, \
           generation.definition_hash, generation.credential_set_handle \
      FROM requirements AS requirement \
      LEFT JOIN jsonb_array_elements(COALESCE($8::text::jsonb, '[]'::jsonb)) AS pin(value) \
        ON pin.value ->> 'component-digest' = requirement.component_digest \
       AND pin.value ->> 'store-alias' = requirement.store_alias \
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
       AND ($8::text IS NULL OR instance.instance_id = pin.value ->> 'instance-id') \
       AND ($8::text IS NULL OR instance.revision >= (pin.value ->> 'instance-revision')::bigint) \
       AND instance.active_generation IS NOT NULL \
      JOIN catalog.connection_generations AS generation \
        ON generation.tenant_id = instance.tenant_id \
       AND generation.environment = instance.environment \
       AND generation.instance_id = instance.instance_id \
       AND generation.generation = CASE WHEN $8::text IS NULL THEN instance.active_generation ELSE (pin.value ->> 'generation')::bigint END \
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
                   'operations', component.operations, \
                   'component-digest', component.component_digest, \
                   'imports', component.imports, \
                   'imports-fingerprint', component.imports_fingerprint, \
                   'effects', component.effects \
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
          AND component.operations ? (definition ->> 'operation') \
   ) \
 GROUP BY selected.version, selected.effective_release_id, selected.package_version, \
          selected.graph_json, selected.wiring_hash, \
          node_summary.node_count, node_summary.invalid_node_count, \
          binding_world.requirement_count, binding_world.resolved_count, \
          binding_world.binding_world_json";

/// Check that every component-grain requirement in the synchronous release
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

/// An immutable wiring and its resolved catalog facts.
///
/// The workflow layer lowers `document` into the graph that the router walks.
#[derive(Debug, Clone)]
pub struct ResolvedActiveWiring {
    pub version: u32,
    pub effective_release_id: u32,
    pub graph_hash: Arc<str>,
    pub package_version: String,
    pub document: WiringDocument,
    /// Complete admitted facts selected by each exact wiring node.
    pub node_components: Arc<BTreeMap<String, AdmittedComponent>>,
    pub components: Arc<[AdmittedComponent]>,
}

/// Exact outcome of re-reading a frozen candidate before component execution.
#[derive(Debug)]
pub enum CandidateWiringResolution {
    Resolved(Box<ResolvedActiveWiring>),
    Missing,
    InvalidDefinition,
    BindingWorldUnavailable,
    BindingWorldDrift,
}

impl ResolvedActiveWiring {
    /// The admitted fact whose digest selects one wiring node.
    pub fn component_by_digest(&self, digest: &str) -> Option<&AdmittedComponent> {
        self.components
            .iter()
            .find(|component| component.component_digest == digest)
    }
}

impl WamnPostgres {
    /// Resolve the exact immutable wiring version frozen onto a delivery.
    ///
    /// The format-1 release snapshot scopes the version to the carried
    /// environment and release identity.
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
        let result = if let Some(local) = &self.local_application {
            async {
                local.require_instance(&connection).await?;
                anyhow::ensure!(
                    local.manifest.release.tenant_id == tenant_id
                        && local.manifest.release.environment == environment
                        && local.manifest.release.effective_release_id.get()
                            == u32::try_from(effective_release_id)?
                        && local.facts.manifest_digest.as_str() == manifest_digest,
                    "local release scope mismatch"
                );
                let fact = local.facts.wirings.iter().find(|fact| {
                    fact.scope.package_id == package_id
                        && fact.document.wiring_id == wiring_id
                        && fact.document.version
                            == u32::try_from(wiring_version).expect("validated wiring version")
                });
                fact.map(|fact| {
                    resolved_wiring(
                        DecodedWiring {
                            version: fact.document.version,
                            effective_release_id: local.manifest.release.effective_release_id.get(),
                            package_version: fact.scope.package_version.clone(),
                            graph_hash: fact.document.wiring_hash().as_str().to_owned(),
                            document: fact.document.clone(),
                        },
                        fact.node_components.clone(),
                        local.facts.components.clone(),
                    )
                })
                .transpose()
            }
            .await
        } else {
            let selected = connection
                .query_opt(RELEASE_WIRING_SQL, &params)
                .await
                .context("query exact release wiring");
            match selected {
                Ok(None) => Ok(None),
                Ok(Some(row)) => decode_released_wiring(wiring_id, &row).map(Some),
                Err(error) => Err(error),
            }
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

    /// Read the complete component list of the carried release.
    ///
    /// A route loads the released application from this list. The query
    /// refuses a release whose snapshot does not match every coordinate.
    pub async fn resolve_release_components(
        &self,
        project: &str,
        tenant_id: &str,
        environment: &str,
        effective_release_id: u32,
        manifest_digest: &str,
    ) -> anyhow::Result<Vec<AdmittedComponent>> {
        anyhow::ensure!(effective_release_id > 0, "effective-release-id-zero");
        let effective_release_id = i32::try_from(effective_release_id)
            .context("effective release id exceeds PostgreSQL int")?;
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
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let result = if let Some(local) = &self.local_application {
            async {
                local.require_instance(&connection).await?;
                anyhow::ensure!(
                    local.manifest.release.tenant_id == tenant_id
                        && local.manifest.release.environment == environment
                        && local.manifest.release.effective_release_id.get()
                            == u32::try_from(effective_release_id)?
                        && local.facts.manifest_digest.as_str() == manifest_digest,
                    "local release scope mismatch"
                );
                Ok(local.facts.components.clone())
            }
            .await
        } else {
            async {
                let row = connection
                    .query_opt(
                        RELEASE_COMPONENTS_SQL,
                        &[
                            &tenant_id,
                            &environment,
                            &effective_release_id,
                            &manifest_digest,
                        ],
                    )
                    .await
                    .context("query release components")?
                    .ok_or_else(|| anyhow::anyhow!("release-snapshot-not-found"))?;
                decode_release_components(&row, 0)
            }
            .await
        };
        let result = result.and_then(|components| {
            verify_served_effect_projections(&components)?;
            Ok(components)
        });

        match result {
            Ok(components) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(error).context("commit release component snapshot");
                }
                Ok(components)
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
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }

        let pinned_bindings = expected_binding_world.to_json()?;
        let params: [&(dyn ToSql + Sync); 8] = [
            &tenant_id,
            &package_id,
            &environment,
            &wiring_id,
            &wiring_version,
            &effective_release_id,
            &wiring_hash,
            &pinned_bindings,
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
                            match decode_active_wiring(wiring_id, &row) {
                                Ok(resolved) => {
                                    Ok(CandidateWiringResolution::Resolved(Box::new(resolved)))
                                }
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
        let result = if let Some(local) = &self.local_application {
            if local.manifest.release.tenant_id != tenant_id
                || local.manifest.release.environment != environment
                || i32::try_from(local.manifest.release.effective_release_id.get()).ok()
                    != Some(effective_release_id)
            {
                Err(anyhow::anyhow!("local readiness release scope mismatch"))
            } else {
                local.bindings_ready(&connection, &component_digests).await
            }
        } else {
            connection
                .query_one(RELEASE_COMPONENT_BINDINGS_READY_SQL, &params)
                .await
                .context("query synchronous release connection bindings")
                .and_then(|row| row.try_get(0).context("decode release binding readiness"))
        };

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
    let components = decode_release_components(row, 6)?;
    resolved_wiring(decoded, node_components, components)
}

/// The release component list at column `first`, and its manifest count after it.
fn decode_release_components(
    row: &tokio_postgres::Row,
    first: usize,
) -> anyhow::Result<Vec<AdmittedComponent>> {
    let components: String = row
        .try_get(first)
        .context("decode release manifest component closure")?;
    let components: Vec<AdmittedComponent> =
        serde_json::from_str(&components).context("parse release manifest component closure")?;
    let expected_component_count: i32 = row
        .try_get(first + 1)
        .context("decode release manifest component count")?;
    anyhow::ensure!(
        usize::try_from(expected_component_count).ok() == Some(components.len()),
        "release-manifest-component-closure-incomplete"
    );
    Ok(components)
}

fn decode_active_wiring(
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
                && component.operations.contains_key(&node.operation)
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
    let components = node_components.values().cloned().collect();
    resolved_wiring(decoded, node_components, components)
}

/// Check each node's admitted component against the document, and keep both.
fn resolved_wiring(
    decoded: DecodedWiring,
    resolved: BTreeMap<String, AdmittedComponent>,
    components: Vec<AdmittedComponent>,
) -> anyhow::Result<ResolvedActiveWiring> {
    if decoded.document.response.is_some() {
        validate_resolved_wiring_compatibility(&decoded.document, &resolved)
            .context("validate resolved response contracts")?;
    }
    for (node_id, component) in &resolved {
        let node = decoded
            .document
            .nodes
            .get(node_id)
            .ok_or_else(|| anyhow::anyhow!("release-wiring-node-binding-extra"))?;
        anyhow::ensure!(
            node.component == component.component
                && node.interface_version == component.interface_version
                && component.operations.contains_key(&node.operation)
                && components.contains(component),
            "release-wiring-node-binding-mismatch"
        );
    }
    verify_served_effect_projections(&components)?;

    Ok(ResolvedActiveWiring {
        version: decoded.version,
        effective_release_id: decoded.effective_release_id,
        graph_hash: Arc::from(decoded.graph_hash),
        package_version: decoded.package_version,
        document: decoded.document,
        node_components: Arc::new(resolved),
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
        AdmittedComponentOperation, ComponentDeclaration, ComponentOperationDeclaration,
        ComponentPackageScope, ComponentPortDeclaration, normalize_component_fact,
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
                operations: BTreeMap::from([(
                    operation.to_owned(),
                    ComponentOperationDeclaration {
                        pre_commit: None,
                        pre_commit_required: false,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        dependencies: Vec::new(),
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".to_owned(),
                            schema: json!({}),
                        }],
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                    },
                )]),
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
    fn resolved_response_preserves_the_frozen_contract_and_requires_committed_facts() {
        let registered = "orders:entity/create@1.2.0";
        let document = WiringDocument::parse(&json!({
            "format-version": "0.1", "wiring-id": "create-order", "version": 1,
            "entry": "write",
            "nodes": {"write": {
                "component": "entity", "interface-version": "0.1.0",
                "operation": registered, "terminal": "respond"
            }},
            "response": {"node": "write", "schema": {"type": "array"}, "committed-result": "write"}
        }))
        .unwrap();
        let mut admitted = component("entity", "create", 'a');
        let mut operation = admitted.operations.remove("create").unwrap();
        operation.registered_operation = Some(registered.to_owned());
        admitted.operations.insert(registered.to_owned(), operation);
        let resolve = |admitted: AdmittedComponent| {
            resolved_wiring(
                DecodedWiring {
                    version: 1,
                    effective_release_id: 7,
                    package_version: "1.2.0".to_owned(),
                    graph_hash: document.wiring_hash().as_str().to_owned(),
                    document: document.clone(),
                },
                BTreeMap::from([("write".to_owned(), admitted.clone())]),
                vec![admitted],
            )
        };
        assert!(resolve(admitted.clone()).is_err());
        let schema = json!({"type": "array"});
        admitted
            .operations
            .get_mut(registered)
            .unwrap()
            .committed_result_schema = Some(wamn_catalog::ComponentSchema {
            schema_digest: wamn_execution_contract::canonical_json_sha256(&schema),
            schema,
        });
        let resolved = resolve(admitted.clone()).expect("admitted committed result resolves");
        assert_eq!(resolved.document.response, document.response);
        assert_eq!(resolved.components.as_ref(), &[admitted]);
    }

    #[test]
    fn same_digest_nodes_keep_exact_target_package_identity() {
        let document = WiringDocument::parse(&json!({
            "format-version": "0.1",
            "wiring-id": "compose-orders",
            "version": 3,
            "entry": "base",
            "nodes": {
                "base": {
                    "component": "entity",
                    "interface-version": "0.1.0",
                    "operation": "base:entity/create@1.0.0"
                },
                "overlay": {
                    "component": "entity",
                    "interface-version": "0.1.0",
                    "operation": "overlay:entity/create@2.0.0",
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
        let mut base_operation = base.operations.remove("create").expect("base operation");
        base_operation.registered_operation = Some("base:entity/create@1.0.0".to_owned());
        base.operations
            .insert("base:entity/create@1.0.0".to_owned(), base_operation);
        let mut overlay = component("entity", "create", 'a');
        overlay.scope.package_id = "overlay".to_owned();
        overlay.scope.package_version = "2.0.0".to_owned();
        let mut overlay_operation = overlay
            .operations
            .remove("create")
            .expect("overlay operation");
        overlay_operation.registered_operation = Some("overlay:entity/create@2.0.0".to_owned());
        overlay
            .operations
            .insert("overlay:entity/create@2.0.0".to_owned(), overlay_operation);
        let mut dependency = component("dependency", "nested", 'c');
        dependency.scope.package_id = "base".to_owned();
        dependency.scope.package_version = "1.0.0".to_owned();
        let graph_hash = document.wiring_hash().as_str().to_owned();

        let resolved = resolved_wiring(
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
            vec![base.clone(), overlay.clone(), dependency.clone()],
        )
        .expect("exact node targets resolve");

        assert_eq!(base.component_digest, overlay.component_digest);
        assert_ne!(base, overlay);
        assert_eq!(
            resolved.node_components.as_ref(),
            &BTreeMap::from([
                ("base".to_owned(), base.clone()),
                ("overlay".to_owned(), overlay.clone()),
            ])
        );
        assert!(resolved.components.contains(&base));
        assert!(resolved.components.contains(&overlay));
        assert!(resolved.components.contains(&dependency));
        assert!(
            !resolved
                .node_components
                .values()
                .any(|fact| fact == &dependency)
        );
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
            "ELSE (pin.value ->> 'generation')::bigint END",
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
            operations: BTreeMap::from([(
                "map".to_owned(),
                AdmittedComponentOperation {
                    pre_commit: None,
                    pre_commit_required: false,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: imports.iter().map(|name| (*name).to_owned()).collect(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
        }
    }

    /// wamn-0h0g.21.11. The delivery path must refuse the fabricated purity
    /// claim, not merely the publication path. Deleting the call in
    /// `resolved_wiring` leaves this failing.
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
