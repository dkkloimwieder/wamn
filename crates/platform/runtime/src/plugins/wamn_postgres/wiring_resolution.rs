//! Released and candidate wiring resolution through the existing platform pool.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use tokio_postgres::types::ToSql;
use wamn_catalog::{AdmittedComponent, WiringDocument, validate_resolved_wiring_compatibility};
use wamn_run_state::AuthorityClass;

use super::{CandidateBindingWorld, WamnPostgres};

/// The immutable-version snapshot behind a released delivery. Admission
/// already froze the exact wiring version.
///
/// The release is its manifest digest (docs/plan/platform-deploy.md R1).
/// `catalog.releases` holds the verified manifest bytes under that digest, and
/// the package membership, the wiring membership and the component closure are
/// read from those bytes. Binds: `$1` tenant, `$2` package, `$3` wiring id,
/// `$4` wiring version, `$5` manifest digest.
pub const RELEASE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT convert_from(release.canonical_bytes, 'UTF8')::jsonb AS manifest \
      FROM catalog.releases AS release \
     WHERE release.tenant_id = $1 \
       AND release.manifest_digest = $5 \
), release_package AS MATERIALIZED ( \
    SELECT package.value ->> 'package-id' AS package_id, \
           package.value ->> 'package-version' AS package_version \
      FROM release_scope \
     CROSS JOIN LATERAL jsonb_array_elements(release_scope.manifest #> '{release,packages}') \
           AS package(value) \
), selected AS MATERIALIZED ( \
    SELECT wiring.version, release_package.package_version, \
           wiring.graph_json, wiring.wiring_hash \
      FROM release_scope \
      JOIN release_package ON release_package.package_id = $2 \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = $1 \
       AND wiring.package_id = $2 \
       AND wiring.package_version = release_package.package_version \
       AND wiring.wiring_id = $3 \
       AND wiring.version = $4 \
     WHERE EXISTS ( \
           SELECT 1 \
             FROM jsonb_array_elements( \
                    COALESCE(release_scope.manifest #> '{workflow,wirings}', '[]'::jsonb) \
                  ) AS member(value) \
            WHERE member.value ->> 'package-id' = $2 \
              AND member.value ->> 'wiring-id' = $3 \
              AND member.value ->> 'wiring-version' = $4::text \
              AND member.value ->> 'graph-hash' = wiring.wiring_hash \
       ) \
) \
SELECT selected.version, selected.package_version, \
       selected.graph_json::text, selected.wiring_hash, \
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
              FROM jsonb_array_elements(release_scope.manifest -> 'components') \
                   WITH ORDINALITY AS projected(definition, ordinality) \
              JOIN release_package \
                ON release_package.package_id = projected.definition ->> 'package-id' \
              JOIN catalog.component_library AS component \
                ON component.tenant_id = $1 \
               AND component.package_id = release_package.package_id \
               AND component.package_version = release_package.package_version \
               AND component.component = projected.definition ->> 'component' \
               AND component.interface_version = projected.definition ->> 'interface-version' \
               AND component.component_digest = projected.definition ->> 'digest'), \
           '[]'::jsonb \
       )::text AS components, \
       jsonb_array_length(release_scope.manifest -> 'components') AS manifest_component_count \
  FROM selected CROSS JOIN release_scope";

/// The complete component list of the carried release, for a route.
///
/// A route walks no wiring, so it reads the component list alone: the same
/// projection of the verified release bytes that [`RELEASE_WIRING_SQL`]
/// returns beside a wiring. The loaded application is one per release, so a
/// route and a wiring must load the same list. Binds: `$1` tenant, `$2`
/// manifest digest.
pub const RELEASE_COMPONENTS_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT convert_from(release.canonical_bytes, 'UTF8')::jsonb AS manifest \
      FROM catalog.releases AS release \
     WHERE release.tenant_id = $1 \
       AND release.manifest_digest = $2 \
), release_package AS MATERIALIZED ( \
    SELECT package.value ->> 'package-id' AS package_id, \
           package.value ->> 'package-version' AS package_version \
      FROM release_scope \
     CROSS JOIN LATERAL jsonb_array_elements(release_scope.manifest #> '{release,packages}') \
           AS package(value) \
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
              JOIN release_package \
                ON release_package.package_id = projected.definition ->> 'package-id' \
              JOIN catalog.component_library AS component \
                ON component.tenant_id = $1 \
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
/// serving release carried by this executor. A candidate run carries no release
/// pin, so its package version and bindings resolve under the release the
/// claiming executor carries, `$6`, its manifest digest. The run supplies every
/// other immutable coordinate that admission read from the same row. Parameter
/// eight is the frozen binding JSON for execution, or NULL to capture current
/// admission inputs.
pub const CANDIDATE_WIRING_SQL: &str = "\
WITH release_scope AS MATERIALIZED ( \
    SELECT package.value ->> 'package-version' AS package_version \
      FROM catalog.releases AS release \
     CROSS JOIN LATERAL jsonb_array_elements( \
           convert_from(release.canonical_bytes, 'UTF8')::jsonb #> '{release,packages}' \
       ) AS package(value) \
     WHERE release.tenant_id = $1 \
       AND release.manifest_digest = $6 \
       AND package.value ->> 'package-id' = $2 \
), selected AS MATERIALIZED ( \
    SELECT wiring.version, \
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
       AND binding.manifest_digest = $6 \
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
SELECT selected.version, \
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
 GROUP BY selected.version, selected.package_version, \
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
              AND binding.manifest_digest = $2 \
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
    /// The release the wiring resolved under, as its manifest digest.
    pub manifest_digest: String,
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
    /// The release bytes cached under the carried digest scope the version.
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
        manifest_digest: &str,
        wiring_id: &str,
        wiring_version: u32,
    ) -> anyhow::Result<Option<ResolvedActiveWiring>> {
        anyhow::ensure!(wiring_version > 0, "release-wiring-version-zero");
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

        let params: [&(dyn ToSql + Sync); 5] = [
            &tenant_id,
            &package_id,
            &wiring_id,
            &wiring_version,
            &manifest_digest,
        ];
        let result = if let Some(local) = &self.local_application {
            async {
                local.require_instance(&connection).await?;
                anyhow::ensure!(
                    local.scope.tenant_id == tenant_id
                        && local.scope.environment == environment
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
                            package_version: fact.scope.package_version.clone(),
                            graph_hash: fact.document.wiring_hash().as_str().to_owned(),
                            document: fact.document.clone(),
                        },
                        manifest_digest,
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
                Ok(Some(row)) => {
                    decode_released_wiring(package_id, manifest_digest, wiring_id, &row).map(Some)
                }
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
    /// refuses a release that `catalog.releases` does not hold.
    pub async fn resolve_release_components(
        &self,
        project: &str,
        tenant_id: &str,
        environment: &str,
        manifest_digest: &str,
    ) -> anyhow::Result<Vec<AdmittedComponent>> {
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
                    local.scope.tenant_id == tenant_id
                        && local.scope.environment == environment
                        && local.facts.manifest_digest.as_str() == manifest_digest,
                    "local release scope mismatch"
                );
                Ok(local.facts.components.clone())
            }
            .await
        } else {
            async {
                let row = connection
                    .query_opt(RELEASE_COMPONENTS_SQL, &[&tenant_id, &manifest_digest])
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
    ///
    /// A candidate carries no release pin. `manifest_digest` is the release the
    /// claiming executor carries, under which its package and bindings resolve.
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
        manifest_digest: &str,
        wiring_id: &str,
        wiring_version: u32,
        wiring_hash: &str,
        expected_binding_world: &CandidateBindingWorld,
    ) -> anyhow::Result<CandidateWiringResolution> {
        anyhow::ensure!(wiring_version > 0, "candidate-wiring-version-zero");
        anyhow::ensure!(!wiring_hash.is_empty(), "candidate-wiring-hash-empty");
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
            &manifest_digest,
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
                            match decode_active_wiring(manifest_digest, wiring_id, &row) {
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
        manifest_digest: &str,
        environment: &str,
        component_digests: &[String],
    ) -> anyhow::Result<bool> {
        if component_digests.is_empty() {
            return Ok(true);
        }
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
            &manifest_digest,
            &environment,
            &component_digests,
        ];
        let result = if let Some(local) = &self.local_application {
            if local.scope.tenant_id != tenant_id
                || local.scope.environment != environment
                || local.facts.manifest_digest.as_str() != manifest_digest
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

struct DecodedWiring {
    version: u32,
    package_version: String,
    graph_hash: String,
    document: WiringDocument,
}

fn decode_wiring(wiring_id: &str, row: &tokio_postgres::Row) -> anyhow::Result<DecodedWiring> {
    let version: i32 = row.try_get(0).context("decode active wiring version")?;
    let version = u32::try_from(version).context("active wiring version is not positive")?;
    let package_version: String = row.try_get(1).context("decode package version")?;
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
    Ok(DecodedWiring {
        version,
        package_version,
        graph_hash,
        document,
    })
}

fn decode_released_wiring(
    package_id: &str,
    manifest_digest: &str,
    wiring_id: &str,
    row: &tokio_postgres::Row,
) -> anyhow::Result<ResolvedActiveWiring> {
    let decoded = decode_wiring(wiring_id, row)?;
    let components = decode_release_components(row, 4)?;
    let node_components = release_node_components(&decoded.document, package_id, &components)?;
    resolved_wiring(decoded, manifest_digest, node_components, components)
}

/// Bind each node of a released wiring to its component in the release closure.
///
/// The release names its components, not the node each one serves, so this
/// repeats the binding publish made: a node runs a component of the wiring's own
/// package, and a node that invokes a dependency runs the component of the
/// release package whose registered operation is the dependency's operation.
/// Each node binds exactly one component, or the wiring is refused.
fn release_node_components(
    document: &WiringDocument,
    package_id: &str,
    components: &[AdmittedComponent],
) -> anyhow::Result<BTreeMap<String, AdmittedComponent>> {
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let mut matches =
            components.iter().filter(|component| {
                component.component == node.component
                    && component.interface_version == node.interface_version
                    && component.operations.get(&node.operation).is_some_and(
                        |operation| match &node.operation_dependency {
                            None => component.scope.package_id == package_id,
                            Some(dependency) => dependency.operation.split_once('.').is_some_and(
                                |(module, local)| {
                                    operation.registered_operation.as_deref()
                                        == Some(
                                            wamn_catalog::operation_token(
                                                &component.scope.package_id,
                                                &component.scope.package_version,
                                                module,
                                                local,
                                            )
                                            .as_str(),
                                        )
                                },
                            ),
                        },
                    )
            });
        let component = matches
            .next()
            .ok_or_else(|| anyhow::anyhow!("release-wiring-node-closure-incomplete"))?;
        anyhow::ensure!(
            matches.next().is_none(),
            "release-wiring-node-component-ambiguous"
        );
        resolved.insert(node_id.clone(), component.clone());
    }
    Ok(resolved)
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
    manifest_digest: &str,
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
        .try_get(4)
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
    resolved_wiring(decoded, manifest_digest, node_components, components)
}

/// Check each node's admitted component against the document, and keep both.
fn resolved_wiring(
    decoded: DecodedWiring,
    manifest_digest: &str,
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
        manifest_digest: manifest_digest.to_owned(),
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
                    package_version: "1.2.0".to_owned(),
                    graph_hash: document.wiring_hash().as_str().to_owned(),
                    document: document.clone(),
                },
                "sha256:release",
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
                package_version: "2.0.0".to_owned(),
                graph_hash,
                document,
            },
            "sha256:release",
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
    fn queued_query_uses_the_release_bytes_and_never_the_active_pointer() {
        assert!(RELEASE_WIRING_SQL.contains("catalog.releases"));
        assert!(RELEASE_WIRING_SQL.contains("release.manifest_digest = $5"));
        assert!(!RELEASE_WIRING_SQL.contains("wiring_activation"));
    }

    #[test]
    fn candidate_query_rederives_the_complete_binding_world_without_activation() {
        for predicate in [
            "release.manifest_digest = $6",
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
            "binding.manifest_digest = $2",
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
