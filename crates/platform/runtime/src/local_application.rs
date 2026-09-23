//! Ownership checks for an explicitly unpublished local application.

use crate::plugins::wamn_postgres::CandidateConnectionBinding;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use wamn_catalog::{
    AdmittedComponent, ComponentConnectionRequirement, ComponentPackageScope, ManifestDigest,
    ServingManifest, WiringDocument,
};

fn deserialize_manifest_digest<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ManifestDigest, D::Error> {
    let value = String::deserialize(deserializer)?;
    ManifestDigest::parse(value).map_err(serde::de::Error::custom)
}

/// Existing admission facts carried only by an unpublished local candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalApplicationFacts {
    #[serde(deserialize_with = "deserialize_manifest_digest")]
    pub manifest_digest: ManifestDigest,
    pub components: Vec<AdmittedComponent>,
    pub wirings: Vec<LocalWiringFacts>,
    pub requirements: Vec<ComponentConnectionRequirement>,
    pub bindings: Vec<LocalBindingFacts>,
}

/// The exact generated document and component selection used by release mint.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalWiringFacts {
    pub scope: ComponentPackageScope,
    pub document: WiringDocument,
    pub node_components: BTreeMap<String, AdmittedComponent>,
}

/// A portable requirement paired with an explicitly selected live instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalBindingFacts {
    pub requirement: ComponentConnectionRequirement,
    pub selection: CandidateConnectionBinding,
}

/// A checked local definition closure bound to one physical disposable target.
#[derive(Clone, Debug)]
pub struct LocalApplication {
    pub(crate) manifest: ServingManifest,
    pub(crate) facts: LocalApplicationFacts,
    instance: u32,
}

/// Bind an unpublished file closure to the owned runtime target.
pub async fn load_local_application(
    directory: &Path,
    manifest: &ServingManifest,
    database_url: &str,
    admission_digest: &str,
) -> anyhow::Result<LocalApplication> {
    let instance = read_local_target(
        database_url,
        &manifest.release.tenant_id,
        &manifest.release.environment,
    )
    .await?;
    let facts = load_local_facts(directory, manifest, admission_digest)?;
    Ok(LocalApplication {
        manifest: manifest.clone(),
        facts,
        instance,
    })
}

impl LocalApplication {
    pub(crate) async fn require_instance(
        &self,
        client: &tokio_postgres::Client,
    ) -> anyhow::Result<()> {
        let instance: u32 = client.query_one("SELECT oid FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()", &[]).await?.try_get(0)?;
        anyhow::ensure!(
            instance == self.instance,
            "local application target was recreated; load the new candidate"
        );
        Ok(())
    }

    pub(crate) async fn bindings_ready(
        &self,
        client: &tokio_postgres::Client,
        digests: &[String],
    ) -> anyhow::Result<bool> {
        self.require_instance(client).await?;
        anyhow::ensure!(
            digests.iter().all(|digest| self
                .facts
                .components
                .iter()
                .any(|component| &component.component_digest == digest)),
            "local readiness selects an unknown component"
        );
        for binding in &self.facts.bindings {
            if !digests
                .iter()
                .any(|digest| digest == binding.requirement.component_digest())
            {
                continue;
            }
            let current = read_local_binding(
                client,
                &self.manifest.release.tenant_id,
                &self.manifest.release.environment,
                &binding.requirement,
                &binding.selection.instance_id,
            )
            .await?;
            if current != binding.selection {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) async fn effect_snapshot(
        &self,
        client: &tokio_postgres::Client,
        tenant: &str,
        lookup: &crate::plugins::wamn_postgres::ConnectionEffectLookup<'_>,
    ) -> anyhow::Result<Option<crate::plugins::wamn_postgres::ConnectionEffectSnapshot>> {
        self.require_instance(client).await?;
        anyhow::ensure!(
            tenant == self.manifest.release.tenant_id
                && lookup.environment == self.manifest.release.environment
                && u32::try_from(lookup.effective_release_id)?
                    == self.manifest.release.effective_release_id.get(),
            "local connection release scope mismatch"
        );
        let Some(wiring) = self.facts.wirings.iter().find(|wiring| {
            wiring.scope.package_id == lookup.wiring_package_id
                && wiring.document.wiring_id == lookup.wiring_id
                && i32::try_from(wiring.document.version).ok() == Some(lookup.wiring_version)
        }) else {
            return Ok(None);
        };
        let Some(component) = self.facts.components.iter().find(|component| {
            component.scope.package_id == lookup.package_id
                && component.component_digest == lookup.component_digest
        }) else {
            return Ok(None);
        };
        let Some(binding) = self.facts.bindings.iter().find(|binding| {
            binding.requirement.component_digest() == lookup.component_digest
                && binding.requirement.store_alias() == lookup.store_alias
        }) else {
            return Ok(None);
        };
        let (current, definition, active_generation) = read_binding(
            client,
            tenant,
            lookup.environment,
            &binding.requirement,
            &binding.selection.instance_id,
            Some(&binding.selection),
        )
        .await?;
        let node_permitted = wiring
            .node_components
            .get(lookup.node_id)
            .is_some_and(|origin| {
                origin.scope.package_id == lookup.origin_package_id
                    && origin.component_digest == lookup.origin_component_digest
                    && origin.component == lookup.origin_component
                    && origin.interface_version == lookup.origin_interface_version
                    && origin.operations.contains_key(lookup.origin_operation)
            })
            && wiring
                .document
                .nodes
                .get(lookup.node_id)
                .is_some_and(|node| node.operation == lookup.origin_operation)
            && component.operations.contains_key(lookup.operation);
        let valid = current == binding.selection
            && lookup
                .candidate_binding
                .is_none_or(|candidate| candidate == &current);
        Ok(Some(
            crate::plugins::wamn_postgres::ConnectionEffectSnapshot {
                wiring_hash: wiring.document.wiring_hash().as_str().to_owned(),
                component: Some(component.component.clone()),
                interface_version: Some(component.interface_version.clone()),
                operation: Some(lookup.operation.to_owned()),
                registered_operation: component
                    .operations
                    .get(lookup.operation)
                    .and_then(|operation| operation.registered_operation.clone()),
                requirement_json: Some(serde_json::to_value(&binding.requirement)?),
                requirement_hash: Some(current.requirement_hash.clone()),
                node_permitted,
                binding_active: valid,
                binding_valid: valid,
                instance_id: Some(current.instance_id),
                validation_hash: Some(current.validation_hash),
                requirement_type: Some(current.requirement_type),
                contract: Some(current.contract),
                instance_enabled: true,
                active_generation,
                pinned_generation: Some(current.generation),
                instance_revision: Some(current.instance_revision),
                generation: Some(current.generation),
                definition: Some(definition),
                definition_hash: Some(current.definition_hash),
                credential_handle: Some(current.credential_set_handle),
            },
        ))
    }
}

/// Resolve an explicit local selection through the live instance and generation.
pub async fn read_local_binding(
    client: &tokio_postgres::Client,
    tenant: &str,
    environment: &str,
    requirement: &ComponentConnectionRequirement,
    instance_id: &str,
) -> anyhow::Result<CandidateConnectionBinding> {
    read_binding(client, tenant, environment, requirement, instance_id, None)
        .await
        .map(|(binding, _, _)| binding)
}

async fn read_binding(
    client: &tokio_postgres::Client,
    tenant: &str,
    environment: &str,
    requirement: &ComponentConnectionRequirement,
    instance_id: &str,
    pin: Option<&CandidateConnectionBinding>,
) -> anyhow::Result<(CandidateConnectionBinding, serde_json::Value, Option<i64>)> {
    let generation = pin.map(|binding| binding.generation);
    let row = client.query_opt("SELECT instance.revision, instance.requirement_type, instance.contract, generation.generation, generation.definition_json::text, generation.definition_hash, generation.credential_set_handle, instance.active_generation FROM catalog.connection_instances AS instance JOIN catalog.connection_generations AS generation ON generation.tenant_id = instance.tenant_id AND generation.environment = instance.environment AND generation.instance_id = instance.instance_id AND generation.generation = COALESCE($4::bigint, instance.active_generation) WHERE instance.tenant_id = $1 AND instance.environment = $2 AND instance.instance_id = $3 AND instance.lifecycle_status = 'enabled'", &[&tenant, &environment, &instance_id, &generation]).await?.context("local connection selection has no enabled instance and active generation")?;
    if let Some(pin) = pin {
        anyhow::ensure!(
            row.try_get::<_, i64>(0)? >= pin.instance_revision,
            "local connection revision predates its admission pin"
        );
    }
    let requirement_type: String = row.try_get(1)?;
    let contract: String = row.try_get(2)?;
    anyhow::ensure!(
        requirement_type == requirement.requirement().requirement_type
            && contract == requirement.requirement().contract,
        "local connection selection has another type or contract"
    );
    let definition: serde_json::Value = serde_json::from_str(&row.try_get::<_, String>(4)?)?;
    let definition_hash: String = row.try_get(5)?;
    anyhow::ensure!(
        crate::connection_generation::definition_hash(&definition) == definition_hash,
        "local connection generation definition hash mismatch"
    );
    let credential_set_handle: String = row.try_get(6)?;
    anyhow::ensure!(
        !credential_set_handle.is_empty(),
        "local connection generation has no credential handle"
    );
    let requirement_hash = requirement.requirement_hash();
    let validation_hash = crate::connection_generation::definition_hash(
        &crate::connection_generation::binding_validation_subject(
            requirement.requirement(),
            &requirement_hash,
            &definition_hash,
        ),
    );
    Ok((
        CandidateConnectionBinding {
            component_digest: requirement.component_digest().to_owned(),
            store_alias: requirement.store_alias().to_owned(),
            requirement_hash,
            instance_id: instance_id.to_owned(),
            instance_revision: pin.map_or(row.try_get(0)?, |binding| binding.instance_revision),
            requirement_type,
            contract,
            validation_hash,
            generation: row.try_get(3)?,
            definition_hash,
            credential_set_handle,
        },
        definition,
        row.try_get(7)?,
    ))
}

/// File transport for local facts; the canonical serving manifest stays separate.
pub const LOCAL_FACTS_FILE: &str = "local-admission.json";

/// Load and check a local fact closure against its canonical manifest.
pub fn load_local_facts(
    directory: &Path,
    manifest: &ServingManifest,
    admission_digest: &str,
) -> anyhow::Result<LocalApplicationFacts> {
    let bytes =
        std::fs::read(directory.join(LOCAL_FACTS_FILE)).context("read local admission facts")?;
    anyhow::ensure!(
        crate::component_admission::component_digest(&bytes) == admission_digest,
        "local admission bytes changed after validation"
    );
    let facts: LocalApplicationFacts =
        serde_json::from_slice(&bytes).context("parse local admission facts")?;
    validate_local_facts(&facts, manifest)?;
    Ok(facts)
}

/// Refuse missing, additional, or mismatched facts before a native host starts.
pub fn validate_local_facts(
    facts: &LocalApplicationFacts,
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        facts.manifest_digest == manifest.digest(),
        "local admission names another manifest"
    );
    anyhow::ensure!(
        facts.components.len() == manifest.components.len(),
        "local component closure is incomplete"
    );
    let mut seen = BTreeSet::new();
    for fact in &facts.components {
        anyhow::ensure!(
            seen.insert((
                &fact.scope.package_id,
                &fact.component,
                &fact.interface_version
            )),
            "local component is repeated"
        );
        anyhow::ensure!(
            fact.scope.tenant_id == manifest.release.tenant_id
                && manifest
                    .release
                    .packages
                    .iter()
                    .any(|package| package.package_id() == fact.scope.package_id
                        && package.package_version() == fact.scope.package_version),
            "local component package is outside the manifest"
        );
        let projected = manifest
            .components
            .iter()
            .find(|component| {
                component.package_id == fact.scope.package_id
                    && component.component == fact.component
                    && component.interface_version == fact.interface_version
            })
            .context("local component is outside the manifest")?;
        anyhow::ensure!(
            projected.digest.as_str() == fact.component_digest
                && projected.operations.len() == fact.operations.len(),
            "local component differs from its manifest projection"
        );
        for (name, operation) in &fact.operations {
            let selected = projected
                .operations
                .get(name)
                .context("local operation is outside the manifest")?;
            let schema = operation.committed_result_schema.as_ref().map(|schema| {
                String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                    &schema.schema,
                ))
                .expect("canonical JSON is UTF-8")
            });
            anyhow::ensure!(
                selected.committed_result_schema == schema
                    && selected.pre_commit == operation.pre_commit
                    && selected.registered_operation == operation.registered_operation
                    && selected.fresh_only == operation.fresh_only
                    && selected.dependencies == operation.dependencies
                    && selected.statements == operation.statements,
                "local operation differs from its manifest projection"
            );
        }
        wamn_catalog::verify_stored_effect_projection(fact)
            .context("validate local effect projection")?;
    }
    anyhow::ensure!(
        facts.wirings.len() == manifest.wirings.len(),
        "local wiring closure is incomplete"
    );
    let mut seen = BTreeSet::new();
    for fact in &facts.wirings {
        let document = WiringDocument::parse(&serde_json::to_value(&fact.document)?)?;
        anyhow::ensure!(
            seen.insert((
                fact.scope.package_id.clone(),
                document.wiring_id.clone(),
                document.version
            )),
            "local wiring is repeated"
        );
        anyhow::ensure!(
            fact.scope.tenant_id == manifest.release.tenant_id
                && manifest
                    .release
                    .packages
                    .iter()
                    .any(|package| package.package_id() == fact.scope.package_id
                        && package.package_version() == fact.scope.package_version),
            "local wiring package is outside the manifest"
        );
        anyhow::ensure!(
            manifest
                .wirings
                .iter()
                .any(|wiring| wiring.package_id == fact.scope.package_id
                    && wiring.wiring_id == document.wiring_id
                    && wiring.wiring_version == document.version
                    && wiring.graph_hash == document.wiring_hash()),
            "local wiring differs from its manifest projection"
        );
        anyhow::ensure!(
            document.nodes.len() == fact.node_components.len()
                && fact
                    .node_components
                    .values()
                    .all(|component| facts.components.contains(component)),
            "local wiring component closure is incomplete"
        );
        wamn_catalog::validate_resolved_wiring_compatibility(&document, &fact.node_components)?;
    }
    anyhow::ensure!(
        facts.requirements.len() == facts.bindings.len(),
        "local binding selection is missing"
    );
    for requirement in &facts.requirements {
        anyhow::ensure!(
            facts
                .bindings
                .iter()
                .filter(|binding| &binding.requirement == requirement)
                .count()
                == 1,
            "local binding does not cover its exact requirement"
        );
        let descriptor = requirement.requirement();
        anyhow::ensure!(
            descriptor == &wamn_catalog::ConnectionTypeDescriptor::http_v1()
                || descriptor == &wamn_catalog::ConnectionTypeDescriptor::blobstore_v1(),
            "local connection descriptor differs from its platform contract"
        );
    }
    let mut seen = BTreeSet::new();
    for binding in &facts.bindings {
        let requirement = &binding.requirement;
        let selected = &binding.selection;
        anyhow::ensure!(
            seen.insert((requirement.component_digest(), requirement.store_alias())),
            "local connection requirement is repeated"
        );
        anyhow::ensure!(
            facts
                .components
                .iter()
                .any(|component| component.component_digest == requirement.component_digest()),
            "local requirement is outside the component closure"
        );
        anyhow::ensure!(
            selected.component_digest == requirement.component_digest()
                && selected.store_alias == requirement.store_alias()
                && selected.requirement_hash == requirement.requirement_hash()
                && selected.requirement_type == requirement.requirement().requirement_type
                && selected.contract == requirement.requirement().contract,
            "local binding differs from its admitted requirement"
        );
    }
    Ok(())
}

/// Mark one physical database creation as belonging to a local tenant session.
pub fn local_target_marker(tenant: &str, environment: &str, instance: u32) -> String {
    let digest = wamn_execution_contract::canonical_json_sha256(&serde_json::json!({
        "tenant": tenant, "environment": environment, "database-instance": instance,
    }));
    format!("wamn-local-target:{digest}")
}

/// The marker and current package manifest hashes in a local target database comment.
///
/// The comment is the marker alone, or the marker, one space, and a JSON object.
/// The object maps each `<package-id>@<package-version>` that the local apply
/// applied to the manifest sha256 that it applied last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTargetComment {
    pub marker: String,
    pub manifests: BTreeMap<String, String>,
}

impl std::fmt::Display for LocalTargetComment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.marker)?;
        if !self.manifests.is_empty() {
            let manifests =
                serde_json::to_string(&self.manifests).expect("a map of strings serializes");
            write!(formatter, " {manifests}")?;
        }
        Ok(())
    }
}

/// Parse a local target database comment that has exactly its written spelling.
pub fn parse_local_target_comment(comment: &str) -> anyhow::Result<LocalTargetComment> {
    let parsed = match comment.split_once(' ') {
        Some((marker, manifests)) => LocalTargetComment {
            marker: marker.to_owned(),
            manifests: serde_json::from_str(manifests)
                .context("parse the manifest hashes of the local target comment")?,
        },
        None => LocalTargetComment {
            marker: comment.to_owned(),
            manifests: BTreeMap::new(),
        },
    };
    anyhow::ensure!(
        parsed.to_string() == comment,
        "the local target comment does not have its written spelling"
    );
    Ok(parsed)
}

/// Check the marker through the selected runtime connection before local loading.
///
/// The normal runtime credential and capability checks remain mandatory.
pub async fn require_local_target(
    database_url: &str,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<()> {
    read_local_target(database_url, tenant, environment)
        .await
        .map(|_| ())
}

/// Read the comment of the connected local target after checking its marker.
pub async fn read_local_target_comment(
    client: &impl tokio_postgres::GenericClient,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<LocalTargetComment> {
    read_target_comment(client, tenant, environment)
        .await
        .map(|(_, comment)| comment)
}

async fn read_local_target(
    database_url: &str,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<u32> {
    let (client, connection) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .context("connect to the local application target")?;
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = read_target_comment(&client, tenant, environment)
        .await
        .map(|(instance, _)| instance);
    drop(client);
    connection.abort();
    result
}

async fn read_target_comment(
    client: &impl tokio_postgres::GenericClient,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<(u32, LocalTargetComment)> {
    let row = client
        .query_one(
            "SELECT oid, pg_catalog.shobj_description(oid, 'pg_database') \
             FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()",
            &[],
        )
        .await
        .context("read the local target ownership marker")?;
    let instance: u32 = row.try_get(0)?;
    let comment: Option<String> = row.try_get(1)?;
    let comment = comment
        .as_deref()
        .and_then(|comment| parse_local_target_comment(comment).ok())
        .filter(|comment| comment.marker == local_target_marker(tenant, environment, instance));
    let Some(comment) = comment else {
        anyhow::bail!("local application requires an owned disposable target created by wamn dev");
    };
    Ok((instance, comment))
}

#[cfg(test)]
mod tests {
    use super::local_target_marker;

    fn fixture() -> (super::LocalApplicationFacts, wamn_catalog::ServingManifest) {
        use std::collections::{BTreeMap, BTreeSet};
        use wamn_catalog::{
            ArtifactHash, EffectiveReleaseId, PackageCoordinate, ServingComponent,
            ServingComponentOperation, ServingManifest, ServingRelease, ServingWiring,
            WiringDocument,
        };
        let component = wamn_catalog::normalize_component_fact(serde_json::from_value(serde_json::json!({
            "scope": {"tenant-id": "tenant-a", "package-id": "orders", "package-version": "1.0.0"}, "component": "transform", "interface-version": "0.1.0", "operations": {"run": {"registered-operation": null, "committed-result-schema": null, "input-ports": [{"name": "input", "schema": {}}], "output-ports": [], "parameters": []}}, "connections": []
        })).unwrap(), format!("sha256:{}", "7".repeat(64)), Vec::new(), Vec::new()).unwrap().component;
        let document = WiringDocument::parse(&serde_json::json!({"format-version": "0.1", "wiring-id": "run", "version": 1, "entry": "node", "nodes": {"node": {"component": "transform", "interface-version": "0.1.0", "operation": "run"}}})).unwrap();
        let manifest = ServingManifest::new(
            ServingRelease {
                tenant_id: "tenant-a".to_owned(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "dev".to_owned(),
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                package_id: "orders".to_owned(),
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                digest: ArtifactHash::parse(&component.component_digest).unwrap(),
                operations: BTreeMap::from([(
                    "run".to_owned(),
                    ServingComponentOperation {
                        pre_commit: None,
                        committed_result_schema: None,
                        registered_operation: None,
                        fresh_only: false,
                        dependencies: Vec::new(),
                        statements: BTreeMap::new(),
                    },
                )]),
            }]),
            BTreeSet::from([ServingWiring {
                package_id: "orders".to_owned(),
                wiring_id: document.wiring_id.clone(),
                wiring_version: document.version,
                graph_hash: document.wiring_hash(),
            }]),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap();
        let facts = super::LocalApplicationFacts {
            manifest_digest: manifest.digest(),
            components: vec![component.clone()],
            wirings: vec![super::LocalWiringFacts {
                scope: component.scope.clone(),
                document,
                node_components: BTreeMap::from([("node".to_owned(), component)]),
            }],
            requirements: Vec::new(),
            bindings: Vec::new(),
        };
        (facts, manifest)
    }

    #[test]
    fn exact_local_facts_round_trip_and_refuse_missing_or_changed_closure() {
        let (facts, manifest) = fixture();
        let bytes = serde_json::to_vec(&facts).unwrap();
        let round_trip = serde_json::from_slice(&bytes).unwrap();
        super::validate_local_facts(&round_trip, &manifest).unwrap();
        let mut missing = facts.clone();
        missing.components.clear();
        assert!(super::validate_local_facts(&missing, &manifest).is_err());
        let mut duplicate = facts.clone();
        duplicate.components.push(facts.components[0].clone());
        assert!(super::validate_local_facts(&duplicate, &manifest).is_err());
        let mut removed = facts.clone();
        removed.components[0].operations.clear();
        assert!(super::validate_local_facts(&removed, &manifest).is_err());
        let mut stale = facts;
        stale.wirings[0].document.version += 1;
        assert!(super::validate_local_facts(&stale, &manifest).is_err());
    }

    #[test]
    fn declared_local_requirement_cannot_disappear_from_readiness() {
        let (mut facts, manifest) = fixture();
        facts
            .requirements
            .push(wamn_catalog::ComponentConnectionRequirement::new(
                &facts.components[0].component_digest,
                "objects",
                wamn_catalog::ConnectionTypeDescriptor::blobstore_v1(),
            ));
        assert!(
            super::validate_local_facts(&facts, &manifest)
                .unwrap_err()
                .to_string()
                .contains("selection is missing")
        );
    }

    #[test]
    fn changed_or_missing_local_admission_bytes_refuse_before_loading() {
        let (mut facts, manifest) = fixture();
        let requirement = wamn_catalog::ComponentConnectionRequirement::new(
            &facts.components[0].component_digest,
            "objects",
            wamn_catalog::ConnectionTypeDescriptor::blobstore_v1(),
        );
        facts.bindings.push(super::LocalBindingFacts {
            requirement: requirement.clone(),
            selection: crate::plugins::wamn_postgres::CandidateConnectionBinding {
                component_digest: requirement.component_digest().to_owned(),
                store_alias: "objects".to_owned(),
                requirement_hash: requirement.requirement_hash(),
                instance_id: "fixture".to_owned(),
                instance_revision: 2,
                requirement_type: requirement.requirement().requirement_type.clone(),
                contract: requirement.requirement().contract.clone(),
                validation_hash: "fixture-validation".to_owned(),
                generation: 1,
                definition_hash: "fixture-definition".to_owned(),
                credential_set_handle: "fixture-handle".to_owned(),
            },
        });
        facts.requirements.push(requirement);
        let directory =
            std::env::temp_dir().join(format!("wamn-local-integrity-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join(super::LOCAL_FACTS_FILE);
        let bytes = serde_json::to_vec(&facts).unwrap();
        let digest = crate::component_admission::component_digest(&bytes);
        std::fs::write(&path, &bytes).unwrap();
        super::load_local_facts(&directory, &manifest, &digest).unwrap();
        let mut changed = serde_json::to_value(&facts).unwrap();
        changed["bindings"] = serde_json::json!([]);
        changed["requirements"] = serde_json::json!([]);
        // Even a semantically valid rewrite must match the coordinator's exact bytes.
        std::fs::write(&path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
        assert!(
            super::load_local_facts(&directory, &manifest, &digest)
                .unwrap_err()
                .to_string()
                .contains("bytes changed")
        );
        std::fs::remove_file(path).unwrap();
        assert!(super::load_local_facts(&directory, &manifest, &digest).is_err());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn malformed_or_foreign_local_manifest_identity_refuses() {
        let (facts, manifest) = fixture();
        let mut value = serde_json::to_value(&facts).unwrap();
        value["manifest-digest"] = serde_json::json!("not-a-digest");
        assert!(serde_json::from_value::<super::LocalApplicationFacts>(value.clone()).is_err());
        value["manifest-digest"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
        let foreign = serde_json::from_value(value).unwrap();
        assert!(super::validate_local_facts(&foreign, &manifest).is_err());
    }

    #[test]
    fn local_ownership_is_bound_to_tenant_environment_and_database_creation() {
        let marker = local_target_marker("tenant-a", "dev", 17);
        assert_ne!(marker, local_target_marker("tenant-b", "dev", 17));
        assert_ne!(marker, local_target_marker("tenant-a", "other", 17));
        assert_ne!(marker, local_target_marker("tenant-a", "dev", 18));
    }

    #[test]
    fn a_local_target_comment_reads_only_its_written_spelling() {
        let marker = local_target_marker("tenant-a", "dev", 17);
        let bare = super::parse_local_target_comment(&marker).unwrap();
        assert_eq!(bare.marker, marker);
        assert!(bare.manifests.is_empty());
        let mut recorded = bare;
        recorded.manifests.insert(
            "platform_fixture@1.0.0".to_owned(),
            format!("sha256:{}", "1".repeat(64)),
        );
        recorded
            .manifests
            .insert("fixture_overlay@1 beta".to_owned(), "sha256:2".to_owned());
        let written = recorded.to_string();
        assert_eq!(
            super::parse_local_target_comment(&written).unwrap(),
            recorded
        );
        for foreign in [
            format!("{marker} {{}}"),
            format!("{marker}  {{\"a@1\":\"b\"}}"),
            format!("{marker} {{ \"a@1\": \"b\" }}"),
            format!("{marker} not-json"),
            format!("{written} "),
        ] {
            assert!(
                super::parse_local_target_comment(&foreign).is_err(),
                "{foreign:?} is not a written local target comment"
            );
        }
    }
}
