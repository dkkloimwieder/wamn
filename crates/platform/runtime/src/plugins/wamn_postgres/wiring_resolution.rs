//! Released and candidate wiring resolution through the existing platform pool.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use tokio_postgres::types::ToSql;
use wamn_catalog::{AdmittedComponent, WiringDocument, validate_resolved_wiring_compatibility};
use wamn_run_state::AuthorityClass;

use super::WamnPostgres;

/// The component descriptors of the release. Binds: tenant and manifest digest.
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
                       'package-id', release_package.package_id, \
                       'package-version', release_package.package_version \
                   ), \
                   'component', descriptor.value -> 'component', \
                   'interface-version', descriptor.value -> 'interface-version', \
                   'operations', descriptor.value -> 'operations', \
                   'component-digest', descriptor.value -> 'component-digest', \
                   'imports', descriptor.value -> 'imports', \
                   'imports-fingerprint', descriptor.value -> 'imports-fingerprint', \
                   'effects', descriptor.value -> 'effects' \
               ) ORDER BY projected.ordinality \
            ) \
              FROM jsonb_array_elements(release_scope.manifest -> 'components') \
                   WITH ORDINALITY AS projected(definition, ordinality) \
             CROSS JOIN LATERAL (SELECT projected.definition -> 'descriptor') AS descriptor(value) \
              JOIN release_package \
                ON release_package.package_id = projected.definition ->> 'package-id'), \
           '[]'::jsonb \
       )::text AS components, \
       jsonb_array_length(release_scope.manifest -> 'components') AS manifest_component_count \
  FROM release_scope";

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

impl ResolvedActiveWiring {
    /// The admitted fact whose digest selects one wiring node.
    pub fn component_by_digest(&self, digest: &str) -> Option<&AdmittedComponent> {
        self.components
            .iter()
            .find(|component| component.component_digest == digest)
    }
}

impl WamnPostgres {
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
                false,
                tenant_id,
                None,
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
                false,
                tenant_id,
                None,
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

pub(super) struct DecodedWiring {
    pub(super) version: u32,
    pub(super) package_version: String,
    pub(super) graph_hash: String,
    pub(super) document: WiringDocument,
}

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

/// Check each node's admitted component against the document, and keep both.
pub(super) fn resolved_wiring(
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
                        committed_result_schema: None,
                        registered_operation: None,
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
            .committed_result_schema = Some(wamn_catalog::ComponentSchema { schema });
        let resolved = resolve(admitted.clone()).expect("admitted committed result resolves");
        assert_eq!(resolved.document.response, document.response);
        assert_eq!(resolved.components.as_ref(), &[admitted]);
    }

    #[test]
    fn component_facts_come_from_the_release_descriptors() {
        assert!(RELEASE_COMPONENTS_SQL.contains("-> 'descriptor'"));
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
                    committed_result_schema: None,
                    registered_operation: None,
                    host_transaction: false,
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
