use super::{Mapping, ServingManifest, Value, ensure};
use anyhow::Context as _;

/// The Helm release name of an application: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/env/project/application`. The name is at most
/// 47 characters, inside Helm's bound of 53.
pub fn release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_catalog::ApplicationName,
) -> anyhow::Result<String> {
    Ok(
        wamn_provision::environment_names::application_release_name(
            org,
            project,
            env,
            application,
        )?,
    )
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
    manifest: &ServingManifest,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_catalog::ApplicationName,
    grant_cache_seconds: u32,
    bindings: &[crate::environment::bindings::Binding],
    integration_repository: &str,
) -> anyhow::Result<Vec<Role>> {
    use wamn_catalog::application_config::{
        ApplicationComponent, ApplicationConfig, ApplicationConnection, ApplicationOperation,
        ApplicationOperationType,
    };
    let mut components = Vec::new();
    let mut typed_operations = std::collections::BTreeSet::new();
    for component in &manifest.components {
        let name = component_name(component);
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
                        wamn_catalog::ConnectionMode::ByBinding(packages) => packages
                            .values()
                            .any(|aliases| aliases.contains_key(&alias)),
                        _ => true,
                    }
            });
            let (selector, mode) = if binding.components.contains(&name) {
                (
                    wamn_catalog::ComponentConnectionAlias::Name(alias.clone()),
                    match binding.mode {
                        wamn_catalog::manifest::ConnectionMode::Read => {
                            wamn_catalog::ConnectionMode::Read
                        }
                        wamn_catalog::manifest::ConnectionMode::Write => {
                            wamn_catalog::ConnectionMode::Write
                        }
                    },
                )
            } else if let Some(connection) = selector {
                (connection.store_alias.clone(), connection.mode.clone())
            } else {
                continue;
            };
            let integration = if binding.runtime_type == wamn_catalog::RuntimeType::Wasm {
                let base = wamn_catalog::parse_component_artifact_base(integration_repository)?;
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
            let route = manifest.route(
                &component.package_id,
                &component.descriptor.component,
                operation_name,
            );
            let type_ = route.map(|route| match route.type_ {
                wamn_catalog::OperationType::Delete => ApplicationOperationType::Delete,
                type_ if type_.is_read() => ApplicationOperationType::Read,
                _ => ApplicationOperationType::Write,
            });
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
                    signed_result: wamn_catalog::signed_file_reads(operation)?,
                },
            );
        }
        components.push(ApplicationComponent {
            name,
            package: component.package_id.clone(),
            digest: component.descriptor.component_digest,
            connections,
            schema: component.runtime_schema.clone(),
            operations,
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
        wamn_catalog::http_routes::HTTP_ROUTES_CONFIG.into(),
        serde_json::to_string(&wamn_catalog::http_routes::route_data(
            manifest,
            &manifest.digest(),
        ))?
        .into(),
    );
    config.insert(
        wamn_catalog::application_config::APPLICATION_CONFIG.into(),
        serde_json::to_string(&application)?.into(),
    );
    Ok(vec![Role {
        name: RoleName::Http,
        config,
        host_interfaces: host_interfaces(manifest, bindings)?,
    }])
}

/// Host imports: platform packages are unnamed; integration declarations keep their labels.
pub fn host_interfaces(
    manifest: &ServingManifest,
    bindings: &[crate::environment::bindings::Binding],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut packages =
        std::collections::BTreeMap::<(&str, &str), (&str, std::collections::BTreeSet<&str>)>::new();
    let mut named = std::collections::BTreeSet::new();
    for component in &manifest.components {
        for import in &component.descriptor.imports {
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
        for binding in bindings.iter().filter(|binding| {
            binding.components.contains(&component_name(component))
                && binding.runtime_type == wamn_catalog::RuntimeType::Wasm
        }) {
            named.insert((binding.alias.as_str(), binding.interface.as_str()));
        }
    }
    let mut interfaces: Vec<_> = packages.into_iter().map(|((namespace, package), (version, interfaces))| serde_json::json!({"namespace":namespace,"package":package,"version":version,"interfaces":interfaces})).collect();
    for (name, interface) in named {
        let (package, rest) = interface
            .split_once('/')
            .context("integration interface package")?;
        let (namespace, package) = package
            .split_once(':')
            .context("integration interface namespace")?;
        let (interface, version) = rest
            .rsplit_once('@')
            .context("integration interface version")?;
        interfaces.push(serde_json::json!({"name":name,"namespace":namespace,"package":package,"version":version,"interfaces":[interface]}));
    }
    Ok(interfaces)
}

/// The sibling name used by both chart components and operation dispatch facts.
pub fn component_name(component: &wamn_catalog::ServingComponent) -> String {
    format!(
        "{}-{}",
        component.package_id, component.descriptor.component
    )
}

/// Render the install values of one application.
pub fn values(input: &ValuesInput) -> anyhow::Result<Value> {
    ensure!(
        input.applications.len() == 1,
        "a release must select exactly one application"
    );

    let environment = wamn_registry::name::host_status(&input.org, &input.project, &input.env);
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
    values.insert("routeHost".into(), input.route_host.clone().into());
    values.insert("platformUi".into(), Value::Null);
    values.insert(
        "applications".into(),
        serde_yaml::to_value(&input.applications).context("render the applications")?,
    );
    values.insert("roles".into(), Value::Sequence(roles));
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
pub struct ValuesInput {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    /// `sha256:<hex>` of the release manifest.
    pub manifest_digest: wamn_digest::Digest,
    pub route_host: String,
    /// Exactly one application's membership, retained in Helm values for status reads.
    pub applications: wamn_catalog::EnvironmentApplications,
    pub roles: Vec<Role>,
    /// Who applied. It renders nothing and is kept with the revision (§9.2).
    pub actor: String,
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
