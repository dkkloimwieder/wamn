//! Ownership checks for an explicitly unpublished local application.

use crate::plugins::wamn_postgres::CandidateConnectionBinding;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use wamn_catalog::{
    AdmittedComponent, ComponentConnectionRequirement, ManifestDigest, ServingManifest,
    WiringDocument,
};
use wamn_engine::release_manifest::ReleaseScope;

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
    pub requirements: Vec<ComponentConnectionRequirement>,
    pub bindings: Vec<LocalBindingFacts>,
}

/// A portable requirement paired with an explicitly selected live instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalBindingFacts {
    pub package_id: String,
    pub requirement: ComponentConnectionRequirement,
    pub selection: CandidateConnectionBinding,
}

/// A checked local definition closure bound to one physical disposable target.
#[derive(Clone, Debug)]
pub struct LocalApplication {
    pub(crate) manifest: ServingManifest,
    pub(crate) scope: ReleaseScope,
    pub(crate) facts: LocalApplicationFacts,
    instance: u32,
    credentials: std::sync::Arc<std::sync::RwLock<Option<LocalConnectionReader>>>,
}

#[derive(Clone, Debug)]
struct LocalConnectionReader {
    credentials: std::sync::Arc<crate::connection_credentials::ConnectionCredentials>,
    project: String,
}

/// Bind an unpublished file closure to the owned runtime target.
pub async fn load_local_application(
    directory: &Path,
    manifest: &ServingManifest,
    scope: ReleaseScope,
    database_url: &str,
) -> anyhow::Result<LocalApplication> {
    let instance = read_local_target(database_url, &scope.env_key, &scope.environment).await?;
    let facts = load_local_facts(directory, manifest, &scope.env_key)?;
    Ok(LocalApplication {
        manifest: manifest.clone(),
        scope,
        facts,
        instance,
        credentials: std::sync::Arc::default(),
    })
}

impl LocalApplication {
    pub(crate) fn set_connection_credentials(
        &self,
        credentials: std::sync::Arc<crate::connection_credentials::ConnectionCredentials>,
        project: &str,
    ) {
        *self
            .credentials
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(LocalConnectionReader {
            credentials,
            project: project.to_owned(),
        });
    }

    fn connection_reader(&self) -> anyhow::Result<LocalConnectionReader> {
        self.credentials
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .context("local bindings require the connection reader")
    }

    fn binding_scope<'a>(
        &'a self,
        package: &'a str,
        reader: &'a LocalConnectionReader,
    ) -> LocalBindingScope<'a> {
        LocalBindingScope {
            env_key: &self.scope.env_key,
            environment: &self.scope.environment,
            manifest_digest: self.facts.manifest_digest.as_str(),
            package_id: package,
            project: &reader.project,
        }
    }

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
            let reader = self.connection_reader()?;
            let current = read_local_binding(
                client,
                &self.binding_scope(&binding.package_id, &reader),
                &binding.requirement,
                &binding.selection.instance_id,
                &reader.credentials,
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
        env_key: &str,
        lookup: &crate::plugins::wamn_postgres::ConnectionEffectLookup<'_>,
    ) -> anyhow::Result<Option<crate::plugins::wamn_postgres::ConnectionEffectSnapshot>> {
        self.require_instance(client).await?;
        anyhow::ensure!(
            env_key == self.scope.env_key
                && lookup.environment == self.scope.environment
                && lookup.manifest_digest == self.facts.manifest_digest.as_str(),
            "local connection release scope mismatch"
        );
        let Some((wiring_hash, entry_permitted)) =
            entry_facts(client, &self.manifest, &self.scope, &self.facts, lookup).await?
        else {
            return Ok(None);
        };
        let Some(component) = self.facts.components.iter().find(|component| {
            component.scope.package_id == lookup.package_id
                && component.component_digest == lookup.component_digest
        }) else {
            return Ok(None);
        };
        let Some(binding) = self.facts.bindings.iter().find(|binding| {
            binding.package_id == lookup.package_id
                && binding.requirement.component_digest() == lookup.component_digest
                && binding.requirement.store_alias() == lookup.store_alias
        }) else {
            return Ok(None);
        };
        let reader = self.connection_reader()?;
        let (current, definition, active_generation) = read_binding(
            client,
            &self.binding_scope(&binding.package_id, &reader),
            &binding.requirement,
            &binding.selection.instance_id,
            &reader.credentials,
        )
        .await?;
        let node_permitted = entry_permitted && component.operations.contains_key(lookup.operation);
        let valid = current == binding.selection
            && lookup
                .candidate_binding
                .is_none_or(|candidate| candidate == &current);
        Ok(Some(
            crate::plugins::wamn_postgres::ConnectionEffectSnapshot {
                connection_name: read_local_connection_name(client, &current.instance_id).await?,
                read_only: current.read_only,
                wiring_hash,
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
                credential_handle: current.credential_set_handle,
            },
        ))
    }
}

/// The entry half of a local effect snapshot: the wiring hash, and whether the
/// origin may enter here. `None` when the entry is not in this application.
///
/// The same entry rule as the Postgres snapshot: a route needs only its origin
/// export, and a wiring node must name the origin component.
async fn entry_facts(
    client: &tokio_postgres::Client,
    manifest: &ServingManifest,
    scope: &ReleaseScope,
    facts: &LocalApplicationFacts,
    lookup: &crate::plugins::wamn_postgres::ConnectionEffectLookup<'_>,
) -> anyhow::Result<Option<(Option<String>, bool)>> {
    let origin_exports = |origin: &AdmittedComponent| {
        origin.scope.package_id == lookup.origin_package_id
            && origin.component_digest == lookup.origin_component_digest
            && origin.component == lookup.origin_component
            && origin.interface_version == lookup.origin_interface_version
            && origin.operations.contains_key(lookup.origin_operation)
    };
    Ok(Some(match lookup.entry.wiring {
        None => {
            if !manifest
                .release
                .packages
                .iter()
                .any(|package| package.package_id() == lookup.entry.package_id)
            {
                return Ok(None);
            }
            (None, facts.components.iter().any(origin_exports))
        }
        Some(position) => {
            let Some(row) = client.query_opt(
                "SELECT graph_json::text FROM catalog.workflows WHERE env_key = $1 AND environment = $2 AND application = $3 AND workflow_id = $4 AND version = $5",
                &[&scope.env_key, &scope.environment, &lookup.entry.package_id, &position.wiring_id, &position.wiring_version],
            ).await? else { return Ok(None); };
            let value: serde_json::Value = serde_json::from_str(&row.get::<_, String>(0))?;
            let document = WiringDocument::parse(&value)?;
            let Some(application) = scope.applications.get(lookup.entry.package_id) else {
                return Ok(None);
            };
            let components = wamn_catalog::resolve_workflow_components(
                &scope.env_key,
                &application.packages,
                manifest,
                &document,
            )?;
            let permitted = components.get(position.node_id).is_some_and(origin_exports)
                && document
                    .nodes
                    .get(position.node_id)
                    .is_some_and(|node| node.operation == lookup.origin_operation);
            // The cluster snapshot derives the hash from the graph too
            // (claims.rs); wamn_http_admitter holds no grant on wiring_hash.
            (Some(document.wiring_hash().as_str().to_owned()), permitted)
        }
    }))
}

async fn read_local_connection_name(
    client: &tokio_postgres::Client,
    instance: &str,
) -> anyhow::Result<Option<String>> {
    Ok(client.query_one("SELECT i.name FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE c.id=$1::text::uuid", &[&instance]).await?.get(0))
}

/// Build the local selection from resolved administration facts.
pub fn selection(
    requirement: &ComponentConnectionRequirement,
    connection: &str,
    generation: i64,
    definition: &serde_json::Value,
    credential_set_handle: Option<String>,
    read_only: bool,
) -> CandidateConnectionBinding {
    let descriptor = requirement.requirement();
    let definition_hash = crate::connection_generation::definition_hash(definition);
    let requirement_hash = requirement.requirement_hash();
    let validation_hash = crate::connection_generation::definition_hash(
        &crate::connection_generation::binding_validation_subject(
            descriptor,
            &requirement_hash,
            &definition_hash,
        ),
    );
    CandidateConnectionBinding {
        read_only,
        component_digest: requirement.component_digest().into(),
        store_alias: requirement.store_alias().into(),
        requirement_hash,
        instance_id: connection.into(),
        instance_revision: generation,
        requirement_type: descriptor.requirement_type.clone(),
        contract: descriptor.contract.clone(),
        validation_hash,
        generation,
        definition_hash,
        credential_set_handle,
    }
}

/// The exact release and package that own a local binding.
#[derive(Debug)]
pub struct LocalBindingScope<'a> {
    pub env_key: &'a str,
    pub environment: &'a str,
    pub manifest_digest: &'a str,
    pub package_id: &'a str,
    pub project: &'a str,
}

/// Resolve a local selection from the same catalog records as released calls.
pub async fn read_local_binding(
    client: &tokio_postgres::Client,
    scope: &LocalBindingScope<'_>,
    requirement: &ComponentConnectionRequirement,
    connection: &str,
    credentials: &crate::connection_credentials::ConnectionCredentials,
) -> anyhow::Result<CandidateConnectionBinding> {
    read_binding(client, scope, requirement, connection, credentials)
        .await
        .map(|(binding, _, _)| binding)
}

async fn read_binding(
    client: &tokio_postgres::Client,
    scope: &LocalBindingScope<'_>,
    requirement: &ComponentConnectionRequirement,
    connection: &str,
    credentials: &crate::connection_credentials::ConnectionCredentials,
) -> anyhow::Result<(CandidateConnectionBinding, serde_json::Value, Option<i64>)> {
    let row=client.query_opt(
        "SELECT c.integration_type,q.mode,r.version FROM catalog.connection_requirement q JOIN catalog.connection_binding b USING(env_key,component_digest,package_id,name) JOIN catalog.release_connection r USING(env_key,manifest_digest,connection_id) JOIN platform.connection c ON c.id=b.connection_id JOIN platform.integration i ON i.id=c.integration_id WHERE q.env_key=$1 AND q.package_id=$2 AND q.component_digest=$3 AND q.name=$4 AND b.manifest_digest=$5 AND b.connection_id=$6::text::uuid AND c.status='active' AND i.status='active'",
        &[&scope.env_key,&scope.package_id,&requirement.component_digest(),&requirement.store_alias(),&scope.manifest_digest,&connection],
    ).await?.context("local connection binding unavailable")?;
    let integration_type: String = row.get(0);
    let descriptor = wamn_catalog::RequirementType::for_integration(&integration_type)
        .context("local binding requires a host integration")?
        .descriptor();
    anyhow::ensure!(
        &descriptor == requirement.requirement(),
        "local connection selection has another type or contract"
    );
    let configuration = credentials
        .version(scope.project, scope.environment, connection, row.get(2))
        .await?;
    let definition = configuration.parameters;
    let definition_hash = crate::connection_generation::definition_hash(&definition);
    let requirement_hash = requirement.requirement_hash();
    let validation_hash = crate::connection_generation::definition_hash(
        &crate::connection_generation::binding_validation_subject(
            &descriptor,
            &requirement_hash,
            &definition_hash,
        ),
    );
    let credential_set_handle = if integration_type == "gcs" {
        None
    } else {
        let name: String=client.query_one("SELECT i.name FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE c.id=$1::text::uuid",&[&connection]).await?.get(0);
        Some(credentials.connection_handle(&name, configuration.generation)?)
    };
    Ok((
        CandidateConnectionBinding {
            read_only: row.get::<_, String>(1) == "read",
            component_digest: requirement.component_digest().into(),
            store_alias: requirement.store_alias().into(),
            requirement_hash,
            instance_id: connection.into(),
            instance_revision: configuration.generation,
            requirement_type: descriptor.requirement_type,
            contract: descriptor.contract,
            validation_hash,
            generation: configuration.generation,
            definition_hash,
            credential_set_handle,
        },
        definition,
        Some(configuration.generation),
    ))
}

/// File transport for local facts; the canonical serving manifest stays separate.
pub const LOCAL_FACTS_FILE: &str = "local-admission.json";

/// The release and application map passed to the local host.
pub const LOCAL_RELEASE_FILE: &str = "local-release.json";

/// Local serving facts used by workflow authoring and enablement.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LocalRelease {
    #[serde(deserialize_with = "deserialize_manifest_digest")]
    pub manifest_digest: ManifestDigest,
    pub applications: wamn_catalog::EnvironmentApplications,
}

/// Read the serving facts beside the local admission file.
pub fn read_local_release(directory: &Path) -> anyhow::Result<LocalRelease> {
    let bytes =
        std::fs::read(directory.join(LOCAL_RELEASE_FILE)).context("read local release facts")?;
    serde_json::from_slice(&bytes).context("parse local release facts")
}

/// Load and check a local fact closure against its canonical manifest.
pub fn load_local_facts(
    directory: &Path,
    manifest: &ServingManifest,
    env_key: &str,
) -> anyhow::Result<LocalApplicationFacts> {
    let bytes =
        std::fs::read(directory.join(LOCAL_FACTS_FILE)).context("read local admission facts")?;
    let facts: LocalApplicationFacts =
        serde_json::from_slice(&bytes).context("parse local admission facts")?;
    validate_local_facts(&facts, manifest, env_key)?;
    Ok(facts)
}

/// Refuse missing, additional, or mismatched facts before a native host starts.
///
/// `env_key` is the tenant of the host scope: the manifest names none.
pub fn validate_local_facts(
    facts: &LocalApplicationFacts,
    manifest: &ServingManifest,
    env_key: &str,
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
            fact.scope.env_key == env_key
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
                    && component.descriptor.component == fact.component
                    && component.descriptor.interface_version == fact.interface_version
            })
            .context("local component is outside the manifest")?;
        anyhow::ensure!(
            projected.descriptor.component_digest.as_str() == fact.component_digest
                && projected.operations.len() == fact.operations.len(),
            "local component differs from its manifest projection"
        );
        for (name, operation) in &fact.operations {
            let selected = projected
                .operations
                .get(name)
                .context("local operation is outside the manifest")?;
            anyhow::ensure!(
                selected.carries(operation),
                "local operation differs from its manifest projection"
            );
        }
        wamn_catalog::verify_stored_effect_projection(fact)
            .context("validate local effect projection")?;
    }
    anyhow::ensure!(
        facts.requirements.len() == facts.bindings.len(),
        "local binding selection is missing"
    );
    for component in &manifest.components {
        for requirement in &component.descriptor.connections {
            anyhow::ensure!(
                facts
                    .bindings
                    .iter()
                    .filter(|binding| {
                        binding.package_id == component.package_id
                            && binding.requirement.component_digest()
                                == component.descriptor.component_digest
                            && binding.requirement.store_alias() == requirement.store_alias
                    })
                    .count()
                    == 1,
                "local binding does not cover its exact requirement"
            );
        }
    }
    for requirement in &facts.requirements {
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
            seen.insert((
                binding.package_id.as_str(),
                requirement.component_digest(),
                requirement.store_alias()
            )),
            "local connection requirement is repeated"
        );
        anyhow::ensure!(
            facts
                .components
                .iter()
                .any(
                    |component| component.component_digest == requirement.component_digest()
                        && component.scope.package_id == binding.package_id
                ),
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
pub fn local_target_marker(env_key: &str, environment: &str, instance: u32) -> String {
    let digest = wamn_execution_contract::canonical_json_sha256(&serde_json::json!({
        "env_key": env_key, "environment": environment, "database-instance": instance,
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
    env_key: &str,
    environment: &str,
) -> anyhow::Result<()> {
    read_local_target(database_url, env_key, environment)
        .await
        .map(|_| ())
}

/// Read the comment of the connected local target after checking its marker.
pub async fn read_local_target_comment(
    client: &impl tokio_postgres::GenericClient,
    env_key: &str,
    environment: &str,
) -> anyhow::Result<LocalTargetComment> {
    read_target_comment(client, env_key, environment)
        .await
        .map(|(_, comment)| comment)
}

async fn read_local_target(
    database_url: &str,
    env_key: &str,
    environment: &str,
) -> anyhow::Result<u32> {
    let (client, connection) = tokio_postgres::connect(database_url, tokio_postgres::NoTls)
        .await
        .context("connect to the local application target")?;
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = read_target_comment(&client, env_key, environment)
        .await
        .map(|(instance, _)| instance);
    drop(client);
    connection.abort();
    result
}

async fn read_target_comment(
    client: &impl tokio_postgres::GenericClient,
    env_key: &str,
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
        .filter(|comment| comment.marker == local_target_marker(env_key, environment, instance));
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
            PackageCoordinate, ServingComponent, ServingComponentOperation, ServingManifest,
            ServingRelease,
        };
        let component = wamn_catalog::normalize_component_fact(serde_json::from_value(serde_json::json!({
            "scope": {"env_key": "tenant-a", "package-id": "orders", "package-version": "1.0.0"}, "component": "transform", "interface-version": "0.1.0", "operations": {"run": {"registered-operation": null, "committed-result-schema": null, "input-ports": [{"name": "input", "schema": {}}], "output-ports": [], "parameters": []}}, "connections": []
        })).unwrap(), format!("sha256:{}", "7".repeat(64)), Vec::new(), Vec::new()).unwrap().component;
        let manifest = ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "orders".to_owned(),
                operations: BTreeMap::from([(
                    "run".to_owned(),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        statements: BTreeMap::new(),
                    },
                )]),
                descriptor: wamn_catalog::ComponentDescriptor::named(
                    "transform",
                    "0.1.0",
                    component.component_digest.clone(),
                ),
            }]),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap();
        let facts = super::LocalApplicationFacts {
            manifest_digest: manifest.digest(),
            components: vec![component.clone()],
            requirements: Vec::new(),
            bindings: Vec::new(),
        };
        (facts, manifest)
    }

    #[tokio::test]
    async fn a_route_entry_needs_only_its_origin_export_and_a_release_package() {
        use crate::plugins::wamn_postgres::{ConnectionEffectLookup, ConnectionEntryLookup};
        let _lock = wamn_test_postgres::lock();
        let database = wamn_catalog::test_database::tenant();
        let (client, connection) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        let task = tokio::spawn(connection);
        let scope = wamn_engine::release_manifest::ReleaseScope::new("tenant-a", "dev");
        let (facts, manifest) = fixture();
        let digest = facts.components[0].component_digest.clone();
        let route = ConnectionEffectLookup {
            entry: ConnectionEntryLookup {
                package_id: "orders",
                wiring: None,
            },
            origin_package_id: "orders",
            origin_component_digest: &digest,
            origin_component: "transform",
            origin_interface_version: "0.1.0",
            origin_operation: "run",
            package_id: "orders",
            operation: "run",
            manifest_digest: facts.manifest_digest.as_str(),
            environment: "dev",
            component_digest: &digest,
            store_alias: "store",
            candidate_binding: None,
        };
        assert_eq!(
            super::entry_facts(&client, &manifest, &scope, &facts, &route)
                .await
                .unwrap(),
            Some((None, true))
        );
        let unexported = ConnectionEffectLookup {
            origin_operation: "absent",
            ..route
        };
        assert_eq!(
            super::entry_facts(&client, &manifest, &scope, &facts, &unexported)
                .await
                .unwrap(),
            Some((None, false))
        );
        let foreign = ConnectionEffectLookup {
            entry: ConnectionEntryLookup {
                package_id: "absent",
                wiring: None,
            },
            ..route
        };
        assert_eq!(
            super::entry_facts(&client, &manifest, &scope, &facts, &foreign)
                .await
                .unwrap(),
            None
        );
        task.abort();
    }

    #[test]
    fn exact_local_facts_round_trip_and_refuse_missing_or_changed_closure() {
        let (facts, manifest) = fixture();
        let bytes = serde_json::to_vec(&facts).unwrap();
        let round_trip = serde_json::from_slice(&bytes).unwrap();
        super::validate_local_facts(&round_trip, &manifest, "tenant-a").unwrap();
        let mut missing = facts.clone();
        missing.components.clear();
        assert!(super::validate_local_facts(&missing, &manifest, "tenant-a").is_err());
        let mut duplicate = facts.clone();
        duplicate.components.push(facts.components[0].clone());
        assert!(super::validate_local_facts(&duplicate, &manifest, "tenant-a").is_err());
        let mut removed = facts.clone();
        removed.components[0].operations.clear();
        assert!(super::validate_local_facts(&removed, &manifest, "tenant-a").is_err());
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
            super::validate_local_facts(&facts, &manifest, "tenant-a")
                .unwrap_err()
                .to_string()
                .contains("selection is missing")
        );
    }

    #[test]
    fn local_facts_accept_the_http_descriptor() {
        let (mut facts, manifest) = fixture();
        let requirement = wamn_catalog::ComponentConnectionRequirement::new(
            &facts.components[0].component_digest,
            "erp",
            wamn_catalog::ConnectionTypeDescriptor::http_v1(),
        );
        facts.bindings.push(super::LocalBindingFacts {
            package_id: facts.components[0].scope.package_id.clone(),
            requirement: requirement.clone(),
            selection: crate::plugins::wamn_postgres::CandidateConnectionBinding {
                read_only: false,
                component_digest: requirement.component_digest().to_owned(),
                store_alias: "erp".to_owned(),
                requirement_hash: requirement.requirement_hash(),
                instance_id: "fixture-http".to_owned(),
                instance_revision: 1,
                requirement_type: "http".to_owned(),
                contract: "wamn:connection/http@0.1.0".to_owned(),
                validation_hash: "fixture-validation".to_owned(),
                generation: 1,
                definition_hash: "fixture-definition".to_owned(),
                credential_set_handle: Some("fixture-http".to_owned()),
            },
        });
        facts.requirements.push(requirement);
        super::validate_local_facts(&facts, &manifest, "tenant-a")
            .expect("local admission accepts the HTTP requirement and its binding");
        facts.bindings[0].package_id.clear();
        assert!(super::validate_local_facts(&facts, &manifest, "tenant-a").is_err());
    }

    #[tokio::test]
    async fn local_readiness_and_dispatch_refuse_a_forged_binding_mode() {
        use crate::plugins::wamn_postgres::{ConnectionEffectLookup, ConnectionEntryLookup};
        let _lock = wamn_test_postgres::lock();
        let mut authority = crate::connection_credentials::test_util::Fixture::new().await;
        authority.put("erp", "secret", &["project-a"]).await;
        let row=authority.admin.query_one("SELECT c.id::text,i.id::text FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.project='project-a' AND i.name='erp'",&[]).await.unwrap();
        let connection: String = row.get(0);
        let integration: String = row.get(1);
        let database = wamn_project_state::test_database::tenant_platform();
        let (client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(driver);
        let (mut facts, manifest) = fixture();
        let component = facts.components[0].clone();
        let requirement = wamn_catalog::ComponentConnectionRequirement::new(
            &component.component_digest,
            "erp",
            wamn_catalog::ConnectionTypeDescriptor::http_v1(),
        );
        client.batch_execute("BEGIN; SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',true),set_config('app.operation','admin:seed-local-binding',true)").await.unwrap();
        client.execute("INSERT INTO platform.integration(id,integration_type,name,status) VALUES($1::text::uuid,'http','erp','active');",&[&integration]).await.unwrap();
        client.execute("INSERT INTO platform.connection(id,integration_id,integration_type,status) VALUES($1::text::uuid,$2::text::uuid,'http','active')",&[&connection,&integration]).await.unwrap();
        client.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES('tenant-a',$1,$2)",&[&facts.manifest_digest.as_str(),&manifest.canonical_bytes()]).await.unwrap();
        client.execute("INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interfaces,mode) VALUES('tenant-a',$1,'orders','erp',ARRAY['wamn:connection/http@0.1.0'],'read')",&[&component.component_digest]).await.unwrap();
        client.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES('tenant-a',$1,$2::text::uuid,1)",&[&facts.manifest_digest.as_str(),&connection]).await.unwrap();
        client.execute("INSERT INTO catalog.connection_binding(env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES('tenant-a',$1,$2,'orders','erp',$3::text::uuid)",&[&facts.manifest_digest.as_str(),&component.component_digest,&connection]).await.unwrap();
        client.batch_execute("COMMIT").await.unwrap();
        let credentials = std::sync::Arc::new(authority.reader("project-a").await);
        let selection = super::read_local_binding(
            &client,
            &super::LocalBindingScope {
                env_key: "tenant-a",
                environment: "dev",
                manifest_digest: facts.manifest_digest.as_str(),
                package_id: "orders",
                project: "project-a",
            },
            &requirement,
            &connection,
            &credentials,
        )
        .await
        .unwrap();
        assert!(selection.read_only);
        facts.requirements.push(requirement.clone());
        facts.bindings.push(super::LocalBindingFacts {
            package_id: "orders".into(),
            requirement,
            selection: selection.clone(),
        });
        let scope = wamn_engine::release_manifest::ReleaseScope::new("tenant-a", "dev")
            .with_applications(
                serde_json::from_value(serde_json::json!({"app":{"packages":["orders"]}})).unwrap(),
            );
        let local = super::LocalApplication {
            manifest,
            scope,
            facts,
            instance: client
                .query_one(
                    "SELECT oid FROM pg_database WHERE datname=current_database()",
                    &[],
                )
                .await
                .unwrap()
                .get(0),
            credentials: std::sync::Arc::default(),
        };
        local.set_connection_credentials(credentials, "project-a");
        assert!(
            local
                .bindings_ready(&client, std::slice::from_ref(&component.component_digest))
                .await
                .unwrap()
        );
        let mut forged = selection;
        forged.read_only = false;
        let lookup = ConnectionEffectLookup {
            entry: ConnectionEntryLookup {
                package_id: "orders",
                wiring: None,
            },
            origin_package_id: "orders",
            origin_component_digest: &component.component_digest,
            origin_component: &component.component,
            origin_interface_version: &component.interface_version,
            origin_operation: "run",
            package_id: "orders",
            operation: "run",
            manifest_digest: local.facts.manifest_digest.as_str(),
            environment: "dev",
            component_digest: &component.component_digest,
            store_alias: "erp",
            candidate_binding: Some(&forged),
        };
        let snapshot = local
            .effect_snapshot(&client, "tenant-a", &lookup)
            .await
            .unwrap()
            .unwrap();
        assert!(snapshot.read_only);
        assert!(!snapshot.binding_active);
        let mut forged_local = local.clone();
        forged_local.facts.bindings[0].selection.read_only = false;
        assert!(
            !forged_local
                .bindings_ready(&client, std::slice::from_ref(&component.component_digest))
                .await
                .unwrap()
        );
        client.batch_execute("BEGIN; SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',true),set_config('app.operation','admin:remove-binding',true); DELETE FROM catalog.connection_binding; COMMIT").await.unwrap();
        assert!(
            local
                .bindings_ready(&client, std::slice::from_ref(&component.component_digest))
                .await
                .is_err()
        );
    }

    #[test]
    fn missing_local_admission_facts_refuse_before_loading() {
        let (mut facts, manifest) = fixture();
        let requirement = wamn_catalog::ComponentConnectionRequirement::new(
            &facts.components[0].component_digest,
            "objects",
            wamn_catalog::ConnectionTypeDescriptor::blobstore_v1(),
        );
        facts.bindings.push(super::LocalBindingFacts {
            package_id: facts.components[0].scope.package_id.clone(),
            requirement: requirement.clone(),
            selection: crate::plugins::wamn_postgres::CandidateConnectionBinding {
                read_only: false,
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
                credential_set_handle: Some("fixture-handle".to_owned()),
            },
        });
        facts.requirements.push(requirement);
        let directory =
            std::env::temp_dir().join(format!("wamn-local-integrity-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join(super::LOCAL_FACTS_FILE);
        std::fs::write(&path, serde_json::to_vec(&facts).unwrap()).unwrap();
        super::load_local_facts(&directory, &manifest, "tenant-a").unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(super::load_local_facts(&directory, &manifest, "tenant-a").is_err());
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
        assert!(super::validate_local_facts(&foreign, &manifest, "tenant-a").is_err());
    }

    #[test]
    fn local_ownership_is_bound_to_env_key_environment_and_database_creation() {
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
