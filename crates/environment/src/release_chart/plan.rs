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
) -> Vec<Role> {
    let project_env_key = wamn_provision::project_env_key(org, project, env);
    let target = |extra: &[(&str, &str)]| -> Mapping {
        [
            ("wamn.project_env_key", project_env_key.as_str()),
            ("wamn.project", project.as_str()),
            ("wamn.environment", env.as_str()),
        ]
        .iter()
        .chain(extra)
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
    };
    let application = application_config(
        manifest,
        org,
        project,
        env,
        application,
        grant_cache_seconds,
    );
    let mut config = target(&[]);
    config.insert(
        wamn_catalog::http_routes::HTTP_ROUTES_CONFIG.into(),
        serde_json::to_string(&wamn_catalog::http_routes::route_data(
            manifest,
            &manifest.digest(),
        ))
        .expect("route data is JSON")
        .into(),
    );
    config.insert(
        wamn_catalog::application_config::APPLICATION_CONFIG.into(),
        serde_json::to_string(&application)
            .expect("application config is JSON")
            .into(),
    );
    vec![Role {
        name: RoleName::Http,
        config,
    }]
}

fn application_config(
    manifest: &ServingManifest,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_catalog::ApplicationName,
    grant_cache_seconds: u32,
) -> wamn_catalog::application_config::ApplicationConfig {
    use wamn_catalog::application_config::{
        ApplicationComponent, ApplicationConfig, ApplicationOperation, ApplicationOperationType,
    };
    ApplicationConfig {
        org: org.clone(),
        project: project.clone(),
        environment: env.clone(),
        application: application.clone(),
        grant_cache_seconds,
        components: manifest
            .components
            .iter()
            .map(|component| ApplicationComponent {
                name: component_name(component),
                package: component.package_id.clone(),
                schema: component.runtime_schema.clone(),
                operations: component
                    .descriptor
                    .operations
                    .iter()
                    .map(|(name, operation)| {
                        let route = manifest.route(
                            &component.package_id,
                            &component.descriptor.component,
                            name,
                        );
                        (
                            name.clone(),
                            ApplicationOperation {
                                component: component_name(component),
                                public: operation.registered_operation.is_some(),
                                host_transaction: operation.host_transaction,
                                type_: route.map(|route| match route.type_ {
                                    wamn_catalog::OperationType::Delete => {
                                        ApplicationOperationType::Delete
                                    }
                                    type_ if type_.is_read() => ApplicationOperationType::Read,
                                    _ => ApplicationOperationType::Write,
                                }),
                                idempotency_key: route.filter(|route| !route.type_.is_read()).map(
                                    |route| {
                                        route
                                            .idempotency
                                            .clone()
                                            .unwrap_or_else(|| "request_id".to_owned())
                                    },
                                ),
                            },
                        )
                    })
                    .collect(),
            })
            .collect(),
    }
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
