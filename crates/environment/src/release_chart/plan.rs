use super::{Mapping, Value, ensure};
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
const INGRESS_BINDING: &str = "ingress";
const WORKFLOW_BINDING: &str = "workflow";
use wamn_component_policy::{ComponentDescriptor, PackageManifest};
use wamn_package::PackageName;
use wamn_package::canonical_name::CanonicalName;
use wamn_package::manifest::deploy::PublishedComponent;
use wamn_registry::configuration::Client;
use wamn_registry::platform_domain::PlatformDomain;

/// The Helm release name of an application: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/env/project/application`. The name is at most
/// 47 characters, inside Helm's bound of 53.
pub fn release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<String> {
    Ok(wamn_provision::environment_names::application_release_name(
        org,
        project,
        env,
        application,
    )?)
}

/// The name the release chart gives the WorkloadDeployment of `role` in
/// release `release_name`: `<release>-<role>`. The http role's Service has
/// the same name.
pub fn workload_name(release_name: &str, role: RoleName) -> String {
    wamn_registry::name::resource_suffix(release_name, role.as_str())
}

/// Every application has one route workload, including applications without HTTP routes.
/// Each role's configuration derives from the coordinate.
pub fn roles(
    package_manifests: &BTreeMap<PackageName, PackageManifest>,
    package_digest: &wamn_digest::Digest,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_registry::ApplicationName,
    grant_cache_seconds: u32,
    bindings: &[crate::environment::bindings::Binding],
    integration_repository: &str,
) -> anyhow::Result<Vec<Role>> {
    use wamn_engine::application_config::{
        ApplicationComponent, ApplicationConfig, ApplicationConnection, ApplicationDataChange,
        ApplicationOperation, ApplicationOperationType, DataChangeModel,
    };
    let mut components = Vec::new();
    let mut declared_connections = std::collections::BTreeMap::new();
    let mut integrations = std::collections::BTreeMap::new();
    let mut connection_ports = BTreeSet::new();
    let component_names: BTreeSet<_> = published_components(package_manifests)
        .map(|(package, component)| component_name(package, &component.descriptor.component))
        .chain(std::iter::once("ingress".to_owned()))
        .collect();
    let stream = wamn_registry::name::stream_name(org, project, env);
    let mut subscriptions = BTreeSet::new();
    let mut handlers = BTreeMap::new();
    let mut typed_operations = BTreeSet::new();
    let route_contracts = wamn_publish::publish_release::route_contracts(package_manifests)?;
    let routes = wamn_publish::publish_release::deployment_routes(
        package_manifests,
        &route_contracts,
        application,
        package_digest,
    )?;
    let schemas = package_manifests
        .iter()
        .map(|(id, package)| {
            Ok((
                id,
                wamn_publish::publish_release::deploy_runtime_schema(package, package_manifests)?,
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    for (package_id, component) in published_components(package_manifests) {
        let name = component_name(package_id, &component.descriptor.component);
        let mut connections = Vec::new();
        for binding in bindings
            .iter()
            .filter(|binding| &binding.application == application)
        {
            let alias = binding.alias.parse()?;
            let selector = component.descriptor.connections.iter().find(|connection| {
                connection.store_alias.accepts(&alias)
                    && binding.interface.starts_with(&format!(
                        "{}/",
                        connection.requirement_type.import_package()
                    ))
                    && match &connection.mode {
                        wamn_package::ConnectionMode::ByBinding(packages) => packages
                            .values()
                            .any(|aliases| aliases.contains_key(&alias)),
                        _ => true,
                    }
            });
            let (selector, mode) = if binding.components.contains(&name) {
                (
                    wamn_package::ComponentConnectionAlias::Name(alias.clone()),
                    match binding.mode {
                        wamn_package::manifest::ConnectionMode::Read => {
                            wamn_package::ConnectionMode::Read
                        }
                        wamn_package::manifest::ConnectionMode::Write => {
                            wamn_package::ConnectionMode::Write
                        }
                    },
                )
            } else if let Some(connection) = selector {
                (connection.store_alias.clone(), connection.mode.clone())
            } else {
                continue;
            };
            ensure!(
                !component_names.contains(&binding.alias),
                "declared connection {} conflicts with a component name",
                binding.alias
            );
            if let Some(previous) =
                declared_connections.insert(&binding.alias, &binding.connection_id)
            {
                ensure!(
                    previous == &binding.connection_id,
                    "declared name {} is bound to two connections in one workload",
                    binding.alias
                );
            }
            let integration = if binding.runtime_type == wamn_connection_type::RuntimeType::Wasm {
                let base = wamn_oci_reference::parse_component_artifact_base(integration_repository)?;
                Some(format!(
                    "{}/{}@{}",
                    base.registry(),
                    base.repository(),
                    binding
                        .digest
                        .as_ref()
                        .context("wasm integration has no digest")?
                ))
            } else {
                None
            };
            if let Some(image) = &integration {
                let authority = binding
                    .authority
                    .as_deref()
                    .context("wasm connection has no fixed authority")?;
                let port =
                    crate::connections::administration::wasm_authority_port(authority)
                        .with_context(|| format!("wasm connection {}", binding.alias))?;
                integrations.entry(&binding.alias).or_insert_with(|| {
                    serde_json::json!({
                        "name": binding.alias,
                        "image": image,
                        "authority": authority,
                        "egressPort": port,
                        "connection": binding.connection_id,
                    })
                });
            }
            if let Some(resolved) = binding.resolved.as_ref().filter(|resolved| {
                resolved.requirement_type == wamn_connection_type::RequirementType::Http
            }) {
                connection_ports.extend(
                    http_connection_ports(&resolved.definition)
                        .with_context(|| format!("http connection {}", binding.alias))?
                        .into_iter()
                        .flatten(),
                );
            }
            connections.push(ApplicationConnection {
                name: alias,
                selector,
                connection_id: binding.connection_id.clone(),
                mode,
                interface: if binding.interface.starts_with("wasmcloud:blobstore/") {
                    "wasmcloud:blobstore".into()
                } else {
                    binding.interface.clone()
                },
                integration_type: binding.integration_type.clone(),
                runtime_type: binding.runtime_type,
                exclusive: binding.exclusive,
                target_type: binding.target_type.clone(),
                integration,
            });
        }
        let mut operations = std::collections::BTreeMap::new();
        for (operation_name, operation) in &component.descriptor.operations {
            let route = route_contracts
                .get(&(package_id.clone(), operation_name.clone()))
                .filter(|route| route.type_ != wamn_package::OperationType::EventHandler);
            let type_ =
                route.and_then(|route| ApplicationOperationType::from_operation(route.type_));
            if type_.is_some() {
                ensure!(
                    typed_operations.insert(operation_name),
                    "operation {operation_name} is declared by two components"
                );
            }
            operations.insert(
                operation_name.clone(),
                ApplicationOperation {
                    component: name.clone(),
                    public: operation.registered_operation.is_some(),
                    host_transaction: operation.host_transaction,
                    type_,
                    idempotency_key: route.filter(|route| !route.type_.is_read()).map(|route| {
                        route
                            .idempotency
                            .clone()
                            .unwrap_or_else(|| "request_id".to_owned())
                    }),
                    signed_result: wamn_component_policy::signed_file_reads(operation)?,
                },
            );
        }
        let package = &package_manifests[package_id];
        let declaration = package
            .components
            .get(component.descriptor.component.as_str())
            .and_then(|declaration| declaration.data_change.as_ref());
        ensure!(
            component.descriptor.data_change == declaration.is_some(),
            "component {name} has a data change export that differs from its verified package manifest"
        );
        let data_change = if let Some(declaration) = declaration {
            let mut models = Vec::with_capacity(declaration.models.len());
            for (model, ops) in &declaration.models {
                let relation = package
                    .models
                    .get(model)
                    .with_context(|| format!("component {name} handles unknown model {model}"))?;
                // CDC publishes the package-local model name as the entity segment.
                let entity = model;
                for op in ops {
                    let op = op.as_str();
                    if let Some(previous) = handlers.insert(
                        (&relation.owner, model, op),
                        (package_id, &component.descriptor.component),
                    ) {
                        anyhow::bail!(
                            "components {} and {name} both handle data changes for {}/{model}/{op}",
                            component_name(previous.0, previous.1),
                            relation.owner,
                        );
                    }
                    let subject = wamn_registry::name::event_subject(
                        org,
                        project,
                        env,
                        &format!("{entity}.{op}"),
                    );
                    subscriptions.insert(format!(
                        "{stream}:{subject}:new:{application}-{entity}-{op}"
                    ));
                }
                models.push(DataChangeModel {
                    owner_package: relation.owner.clone(),
                    model: model.clone(),
                    entity: entity.clone(),
                    ops: ops.clone(),
                });
            }
            Some(ApplicationDataChange {
                role: declaration.role.clone(),
                host_transaction: declaration.host_transaction,
                models,
            })
        } else {
            None
        };
        components.push(ApplicationComponent {
            name,
            package: package_id.clone(),
            digest: component.descriptor.component_digest,
            connections,
            schema: schemas[package_id].clone(),
            operations,
            data_change,
        });
    }
    let application = ApplicationConfig {
        org: org.clone(),
        project: project.clone(),
        environment: env.clone(),
        application: application.clone(),
        grant_cache_seconds,
        components,
    };
    let mut config = Mapping::new();
    for (key, value) in [
        (
            "wamn.project_env_key",
            wamn_provision::project_env_key(org, project, env).to_string(),
        ),
        ("wamn.project", project.to_string()),
        ("wamn.environment", env.to_string()),
    ] {
        config.insert(key.into(), value.into());
    }
    config.insert(
        wamn_routes::HTTP_ROUTES_CONFIG.into(),
        serde_json::to_string(&routes)?.into(),
    );
    config.insert(
        wamn_engine::application_config::APPLICATION_CONFIG.into(),
        serde_json::to_string(&application)?.into(),
    );
    let descriptors =
        published_components(package_manifests).map(|(_, component)| &component.descriptor);
    let mut host_interfaces = host_interfaces(descriptors, !integrations.is_empty())?;
    let subjects = wamn_registry::name::stream_subjects(org, project, env);
    host_interfaces.push(serde_json::json!({
        "namespace": "wasmcloud",
        "package": "nats",
        "version": "0.1.3",
        "interfaces": ["jetstream"],
        "config": {"stream-allow": stream, "subject-allow": subjects},
    }));
    host_interfaces.push(serde_json::json!({
        "namespace": "wasmcloud",
        "package": "nats",
        "version": "0.1.3",
        "name": WORKFLOW_BINDING,
        "interfaces": ["jetstream"],
        "config": {"stream-allow": stream, "subject-allow": subjects},
    }));
    if !subscriptions.is_empty() {
        host_interfaces.push(serde_json::json!({
            "namespace": "wasmcloud",
            "package": "nats",
            "version": "0.1.3",
            "name": INGRESS_BINDING,
            "interfaces": ["jetstream-handler"],
            "config": {
                "component": "ingress",
                "stream-allow": stream,
                "subject-allow": subjects,
                "jetstream-subscriptions": subscriptions.into_iter().collect::<Vec<_>>().join(","),
            },
        }));
    }
    Ok(vec![Role {
        name: RoleName::Http,
        config,
        host_interfaces,
        integration_components: integrations.into_values().collect(),
        connection_ports,
    }])
}

/// The TCP ports of one host HTTP connection: its primary authority and its
/// OAuth `token_authority` when present, each with the scheme's default port.
fn http_connection_ports(definition: &serde_json::Value) -> anyhow::Result<[Option<u16>; 2]> {
    use wamn_authority::connection_authority::{
        TlsPolicy, parse_http_connection_authority, parse_http_definition,
    };
    let primary = parse_http_definition(definition)?.authority().port();
    let token = definition
        .pointer("/auth/token_authority")
        .and_then(serde_json::Value::as_str)
        .map(|authority| {
            parse_http_connection_authority(
                &format!("https://{authority}/"),
                TlsPolicy::VerifyAuthority,
            )
        })
        .transpose()?
        .map(|authority| authority.authority().port());
    Ok([Some(primary), token])
}

/// Unnamed platform imports, including bound when the workload has integration siblings.
pub fn host_interfaces<'a>(
    components: impl IntoIterator<Item = &'a ComponentDescriptor>,
    has_integrations: bool,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut packages =
        std::collections::BTreeMap::<(&str, &str), (&str, std::collections::BTreeSet<&str>)>::new();
    for component in components {
        for import in &component.imports {
            let Some((package, rest)) = import.split_once('/') else {
                continue;
            };
            if !["wamn:postgres", "wamn:connection", "wasmcloud:blobstore"].contains(&package) {
                continue;
            }
            let (namespace, package) = package.split_once(':').context("host import namespace")?;
            let (interface, version) = rest.rsplit_once('@').context("host import version")?;
            let entry = packages
                .entry((namespace, package))
                .or_insert((version, std::collections::BTreeSet::new()));
            ensure!(
                entry.0 == version,
                "two versions of host interface package {namespace}:{package}"
            );
            entry.1.insert(interface);
        }
    }
    let mut interfaces: Vec<_> = packages.into_iter().map(|((namespace, package), (version, interfaces))| serde_json::json!({"namespace":namespace,"package":package,"version":version,"interfaces":interfaces})).collect();
    if has_integrations {
        interfaces.push(serde_json::json!({"namespace":"wamn","package":"integration","version":"0.1.0","interfaces":["bound"]}));
    }
    Ok(interfaces)
}

pub(crate) fn published_components(
    packages: &BTreeMap<PackageName, PackageManifest>,
) -> impl Iterator<
    Item = (
        &PackageName,
        &PublishedComponent<wamn_component_policy::ComponentDescriptor>,
    ),
> {
    packages.iter().flat_map(|(package, manifest)| {
        manifest
            .deploy
            .iter()
            .flat_map(|deploy| deploy.components.values())
            .map(move |component| (package, component))
    })
}

/// The sibling name used by both chart components and operation dispatch facts.
pub fn component_name(package: &PackageName, component: &CanonicalName) -> String {
    format!("{package}-{component}")
}

/// Render the install values of one application.
pub fn values(input: &ValuesInput<'_>) -> anyhow::Result<Value> {
    ensure!(
        input.applications.len() == 1,
        "a release must select exactly one application"
    );

    let environment = wamn_registry::name::host_status(&input.org, &input.project, &input.env);
    let route_host =
        wamn_registry::name::application_host(input.route_label, &input.org, input.platform_domain);
    ensure!(
        route_host.len() <= 253,
        "application host exceeds 253 bytes"
    );
    let roles = input
        .roles
        .iter()
        .map(|role| {
            let mut entry = Mapping::new();
            entry.insert("name".into(), role.name.as_str().into());
            entry.insert("config".into(), Value::Mapping(role.config.clone()));

            Ok(Value::Mapping(entry))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut values = Mapping::new();
    values.insert(
        "release".into(),
        mapping([("manifestDigest", input.manifest_digest.to_string())]),
    );
    values.insert("environment".into(), environment.into());
    values.insert(
        "webClients".into(),
        serde_yaml::to_value(
            input
                .client
                .into_iter()
                .map(|client| (&route_host, &client.package))
                .collect::<BTreeMap<_, _>>(),
        )?,
    );
    values.insert(
        "webDigests".into(),
        serde_yaml::to_value(
            input
                .client
                .into_iter()
                .map(|client| (&route_host, &client.web))
                .collect::<BTreeMap<_, _>>(),
        )?,
    );
    values.insert("routeHost".into(), route_host.into());
    values.insert("org".into(), input.org.to_string().into());
    values.insert("namespace".into(), input.org.as_str().into());
    values.insert("project".into(), input.project.to_string().into());
    values.insert(
        "applications".into(),
        serde_yaml::to_value(&input.applications).context("render the applications")?,
    );
    values.insert("roles".into(), Value::Sequence(roles));
    values.insert(
        "components".into(),
        serde_yaml::to_value(
            input
                .roles
                .iter()
                .flat_map(|role| &role.integration_components)
                .collect::<Vec<_>>(),
        )?,
    );
    values.insert(
        "connectionPorts".into(),
        serde_yaml::to_value(
            input
                .roles
                .iter()
                .flat_map(|role| &role.connection_ports)
                .collect::<BTreeSet<_>>(),
        )?,
    );
    values.insert(
        "hostInterfaces".into(),
        serde_yaml::to_value(
            input
                .roles
                .iter()
                .flat_map(|role| &role.host_interfaces)
                .collect::<Vec<_>>(),
        )?,
    );
    values.insert(
        "hostGroup".into(),
        wamn_registry::name::environment_host_name(&input.org, &input.env).into(),
    );
    Ok(Value::Mapping(values))
}

pub(super) fn mapping<const N: usize>(entries: [(&str, String); N]) -> Value {
    Value::Mapping(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
    )
}

/// One application HTTP workload with its configuration.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: RoleName,
    /// `localResources.config` of the role's component.
    pub config: Mapping,
    pub host_interfaces: Vec<serde_json::Value>,
    pub integration_components: Vec<serde_json::Value>,
    /// The ports of the bound host HTTP connections, opened to public IPv4.
    pub connection_ports: BTreeSet<u16>,
}

/// The roles a release can imply (R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleName {
    Http,
}

impl RoleName {
    /// The suffix of the role's object names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
        }
    }
}

/// The inputs of one application's install values.
#[derive(Debug, Clone)]
pub struct ValuesInput<'a> {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    /// `sha256:<hex>` of the release manifest.
    pub manifest_digest: wamn_digest::Digest,
    pub route_label: &'a str,
    pub platform_domain: &'a PlatformDomain,
    pub client: Option<&'a Client>,
    /// Exactly one application's membership, retained in Helm values for status reads.
    pub applications: wamn_registry::EnvironmentApplications,
    pub roles: Vec<Role>,
}

/// The host samples traces at their start and exports to the cluster's collector.
/// The sampler keeps sampled parents and 10% of new traces (R5, R6).
/// These variables are not application facts. The chart sets them on every host.
pub const TELEMETRY_VARIABLES: [(&str, &str); 4] = [
    (
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "http://otel-collector.platform.svc.cluster.local:4317",
    ),
    ("OTEL_SERVICE_NAME", "wamn-host"),
    ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
    ("OTEL_TRACES_SAMPLER_ARG", "0.1"),
];

#[cfg(test)]
mod tests {
    use super::{BTreeMap, PackageManifest, PackageName, roles};
    use crate::environment::bindings::Binding;

    const OPERATION: &str = "source-fixture:item/get@1.0.0";

    /// One admitted component that exports `operations` and imports `imports`.
    fn component(
        name: &str,
        operations: &[&str],
        imports: &[&str],
        data_change: bool,
    ) -> serde_json::Value {
        let digest = wamn_digest::bytes_sha256(name.as_bytes());
        serde_json::json!({
            "descriptor": {
                "data-change": data_change,
                "component": name,
                "interface-version": "1.0.0",
                "component-digest": digest,
                "operations": operations.iter().map(|operation| ((*operation).to_owned(), serde_json::json!({
                    "input-ports": [], "output-ports": [], "parameters": []
                }))).collect::<serde_json::Map<_, _>>(),
                "imports": imports,
                "imports-fingerprint": digest,
                "effects": [],
                "connections": [],
            },
            "image_digest": digest,
        })
    }

    /// The fixture package with `components`, each declared with the data
    /// change operations it handles on the fixture model.
    fn packages(
        components: &[(serde_json::Value, &[&str])],
    ) -> BTreeMap<PackageName, PackageManifest> {
        let name = |component: &serde_json::Value| {
            component["descriptor"]["component"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let mut document =
            serde_json::to_value(wamn_test_infrastructure::publish::source_manifest()).unwrap();
        document["components"] = components
            .iter()
            .map(|(component, ops)| {
                let declaration = if ops.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"data_change": {"role": "admin", "models": {"item": ops}}})
                };
                (name(component), declaration)
            })
            .collect();
        document["deploy"] = serde_json::json!({
            "routes": {},
            "components": components
                .iter()
                .map(|(component, _)| (name(component), component.clone()))
                .collect::<serde_json::Map<_, _>>(),
        });
        let manifest: PackageManifest = serde_json::from_value(document).unwrap();
        BTreeMap::from([(manifest.package.id.clone(), manifest)])
    }

    /// A host HTTP connection bound by `alias` to component `component`.
    fn binding(alias: &str, connection_id: &str, component: &str) -> Binding {
        Binding {
            application: "app".parse().unwrap(),
            alias: alias.to_owned(),
            interface: "wamn:connection/http@0.1.0".to_owned(),
            components: [component.to_owned()].into(),
            runtime_type: wamn_connection_type::RuntimeType::Host,
            target_type: "cloud".to_owned(),
            connection_id: connection_id.to_owned(),
            authority: None,
            integration_type: "http".parse().unwrap(),
            exclusive: false,
            generation: 1,
            mode: wamn_package::manifest::ConnectionMode::Write,
            digest: None,
            exports: Vec::new(),
            write_interfaces: Vec::new(),
            resolved: None,
        }
    }

    fn render(
        packages: &BTreeMap<PackageName, PackageManifest>,
        bindings: &[Binding],
    ) -> anyhow::Result<Vec<super::Role>> {
        roles(
            packages,
            &wamn_digest::bytes_sha256(b"package"),
            &"acme".parse().unwrap(),
            &"shop".parse().unwrap(),
            &"dev".parse().unwrap(),
            &"app".parse().unwrap(),
            60,
            bindings,
            "registry.example/integrations",
        )
    }

    /// The sibling name of `component` in the one fixture package.
    fn sibling(packages: &BTreeMap<PackageName, PackageManifest>, component: &str) -> String {
        let (package, _) = packages.first_key_value().unwrap();
        super::component_name(package, &component.parse().unwrap())
    }

    #[test]
    fn a_connection_name_equal_to_a_component_name_is_refused() {
        let packages = packages(&[(component("a", &[], &[], false), &[])]);
        let a = sibling(&packages, "a");
        render(&packages, &[binding("store", "c1", &a)]).unwrap();
        for name in [a.as_str(), "ingress"] {
            assert!(
                render(&packages, &[binding(name, "c1", &a)]).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_name_bound_to_two_connections_is_refused() {
        let packages = packages(&[
            (component("a", &[], &[], false), &[]),
            (component("b", &[], &[], false), &[]),
        ]);
        let (a, b) = (sibling(&packages, "a"), sibling(&packages, "b"));
        render(
            &packages,
            &[binding("store", "c1", &a), binding("store", "c1", &b)],
        )
        .unwrap();
        assert!(
            render(
                &packages,
                &[binding("store", "c1", &a), binding("store", "c2", &b)]
            )
            .is_err()
        );
    }

    #[test]
    fn a_typed_operation_exported_by_two_components_is_refused() {
        // Palette exports have no operation contract and are exempt.
        render(
            &packages(&[
                (component("a", &[OPERATION], &[], false), &[]),
                (component("b", &["palette"], &[], false), &[]),
                (component("c", &["palette"], &[], false), &[]),
            ]),
            &[],
        )
        .unwrap();
        let duplicate = packages(&[
            (component("a", &[OPERATION], &[], false), &[]),
            (component("b", &[OPERATION], &[], false), &[]),
        ]);
        assert!(render(&duplicate, &[]).is_err());
    }

    #[test]
    fn two_data_change_handlers_of_one_model_and_operation_are_refused() {
        render(
            &packages(&[
                (component("a", &[], &[], true), &["insert"]),
                (component("b", &[], &[], true), &["update"]),
            ]),
            &[],
        )
        .unwrap();
        let duplicate = packages(&[
            (component("a", &[], &[], true), &["insert"]),
            (component("b", &[], &[], true), &["insert", "update"]),
        ]);
        assert!(render(&duplicate, &[]).is_err());
    }

    #[test]
    fn the_connection_ports_are_the_bound_http_authority_ports() {
        let packages = packages(&[(component("a", &[], &[], false), &[])]);
        let a = sibling(&packages, "a");
        let definitions = [
            serde_json::json!({
                "primary-authority": "https://api.example.com/v1",
                "tls-verification": "verify-authority",
                "auth": {"token_authority": "auth.example.com:8443"},
            }),
            serde_json::json!({
                "primary-authority": "http://plain.example.com:8080/",
                "tls-verification": "disabled",
            }),
            serde_json::json!({
                "primary-authority": "https://other.example.com/",
                "tls-verification": "verify-authority",
            }),
        ];
        let expected: std::collections::BTreeSet<u16> = definitions
            .iter()
            .flat_map(|definition| {
                let primary = definition["primary-authority"].as_str().unwrap();
                let token = definition
                    .pointer("/auth/token_authority")
                    .map(|authority| format!("https://{}/", authority.as_str().unwrap()));
                std::iter::once(primary.to_owned()).chain(token)
            })
            .map(|url| url::Url::parse(&url).unwrap().port_or_known_default().unwrap())
            .collect();
        let bindings: Vec<_> = definitions
            .into_iter()
            .enumerate()
            .map(|(index, definition)| Binding {
                resolved: Some(crate::org_connection::ResolvedConnection {
                    requirement_type: wamn_connection_type::RequirementType::Http,
                    definition,
                    credential_handle: None,
                    org_connection_name: None,
                }),
                ..binding(&format!("api{index}"), &format!("c{index}"), &a)
            })
            .collect();
        let roles = render(&packages, &bindings).unwrap();
        assert_eq!(roles[0].connection_ports, expected);
    }

    #[test]
    fn two_versions_of_one_host_interface_package_are_refused() {
        let statements = "wamn:postgres/statements@0.1.0";
        let (current, next) = ("wamn:postgres/client@0.1.0", "wamn:postgres/client@0.2.0");
        render(
            &packages(&[
                (component("a", &[], &[statements], false), &[]),
                (component("b", &[], &[current], false), &[]),
            ]),
            &[],
        )
        .unwrap();
        let mixed = packages(&[
            (component("a", &[], &[statements], false), &[]),
            (component("b", &[], &[next], false), &[]),
        ]);
        assert!(render(&mixed, &[]).is_err());
    }
}
