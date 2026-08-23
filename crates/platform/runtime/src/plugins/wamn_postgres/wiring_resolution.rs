//! Host-owned active-wiring resolution through the existing platform pool.

use std::sync::Arc;

use anyhow::Context as _;
use tokio_postgres::types::ToSql;
use wamn_catalog::{AdmittedComponent, WiringDocument};
use wamn_router::Wiring;

use crate::wiring_lowering::{
    GatedActiveWiring, ScopedWiringOperationFacts, WiringScope, lower_active_wiring,
    project_component_operation,
};

use super::WamnPostgres;

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

/// A typed active wiring ready for the router and component source.
#[derive(Debug, Clone)]
pub struct ResolvedActiveWiring {
    pub version: u32,
    pub catalog_version: u32,
    pub graph_hash: Arc<str>,
    pub wiring: Wiring,
    pub components: Arc<[AdmittedComponent]>,
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
            .checkout_platform(project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
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
            .checkout_platform(project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
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
            },
            format!("sha256:{}", digest_byte.to_string().repeat(64)),
            ["wasi:logging/logging@0.1.0".to_owned()],
        )
        .expect("fixture component admits")
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
}
