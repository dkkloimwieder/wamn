use super::{AttachmentType, Mapping, ServingManifest, Value, ensure};
use anyhow::Context as _;

/// The Helm release name of an application: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/env/project/application`. The name is at most
/// 47 characters, inside Helm's bound of 53.
pub fn release_name(
    org: &str,
    project: &str,
    env: &str,
    application: &str,
) -> anyhow::Result<String> {
    wamn_control_provision::environment_names::application_release_name(
        org,
        project,
        env,
        application,
    )
    .context("the environment coordinate is not valid")
}

/// The name the release chart gives the WorkloadDeployment of `role` in
/// release `release_name`: `<release>-<role>`. The http role's Service has
/// the same name.
pub fn workload_name(release_name: &str, role: RoleName) -> String {
    wamn_registry::name::resource_suffix(release_name, role.as_str())
}

/// The application HTTP role exists when the manifest has an HTTP route attachment.
/// Each role's configuration derives from the coordinate.
pub fn roles(manifest: &ServingManifest, org: &str, project: &str, env: &str) -> Vec<Role> {
    let project_env_key = wamn_control_provision::project_env_key(org, project, env);
    let target = |extra: &[(&str, &str)]| -> Mapping {
        [
            ("wamn.project_env_key", project_env_key.as_str()),
            ("wamn.project", project),
            ("wamn.environment", env),
        ]
        .iter()
        .chain(extra)
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
    };
    let mut roles = Vec::new();
    if manifest
        .every_attachment()
        .any(|(_, attachment)| attachment.attachment_type() == AttachmentType::Http)
    {
        roles.push(Role {
            name: RoleName::Http,
            entry: manifest
                .composed_entry
                .as_ref()
                .map(|entry| entry.reference.clone()),
            config: {
                let mut config = target(&[]);
                config.insert(
                    wamn_catalog::http_routes::HTTP_ROUTES_CONFIG.into(),
                    serde_json::to_string(&wamn_catalog::http_routes::route_data(
                        manifest,
                        manifest.digest().as_str(),
                    ))
                    .expect("route data is JSON")
                    .into(),
                );
                config
            },
        });
    }
    roles
}

/// Render the install values of one application.
pub fn values(input: &ValuesInput) -> anyhow::Result<Value> {
    ensure!(
        input.applications.len() == 1,
        "a release must select exactly one application"
    );
    wamn_package_artifact::ensure_digest(&input.manifest_digest)?;
    let environment = wamn_registry::name::host_status(&input.org, &input.project, &input.env);
    let roles = input
        .roles
        .iter()
        .map(|role| {
            let mut entry = Mapping::new();
            entry.insert("name".into(), role.name.as_str().into());
            entry.insert("config".into(), Value::Mapping(role.config.clone()));
            if let Some(recorded) = &role.entry {
                let image = wamn_catalog::composed_entry_start_reference(
                    Some(&input.component_base),
                    recorded,
                )
                .context("the component artifact base resolves the entry")?;
                entry.insert("entry".into(), image.into());
            }

            Ok(Value::Mapping(entry))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut values = Mapping::new();
    values.insert(
        "release".into(),
        mapping([
            ("manifestDigest", input.manifest_digest.clone()),
            ("artifactBase", input.artifact_base.clone()),
        ]),
    );
    values.insert("environment".into(), environment.into());
    values.insert("routeHost".into(), input.route_host.clone().into());
    values.insert("platformUi".into(), "".into());
    values.insert(
        "applications".into(),
        serde_yaml::to_value(&input.applications).context("render the applications")?,
    );
    values.insert("roles".into(), Value::Sequence(roles));
    values.insert("audit".into(), mapping([("actor", input.actor.clone())]));
    values.insert(
        "hostGroup".into(),
        wamn_registry::name::environment_host_name(&input.org, &input.env)?.into(),
    );
    Ok(Value::Mapping(values))
}

pub(super) fn recovery(status: &str) -> Option<Recovery> {
    match status {
        "pending-install" => Some(Recovery::Uninstall),
        "pending-upgrade" | "pending-rollback" => Some(Recovery::Rollback),
        _ => None,
    }
}

pub(super) fn mapping<const N: usize>(entries: [(&str, String); N]) -> Value {
    Value::Mapping(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
    )
}

/// The platform image set: the host image and the role component images.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSet {
    /// `<repository>:<tag>@sha256:<hex>`.
    pub host: String,
    /// `<repository>@sha256:<hex>`.
    pub http: String,
}

/// One application HTTP workload with its configuration.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: RoleName,
    /// The HTTP entry path recorded by the release, relative to the component
    /// artifact base. [`values`] joins it with the component artifact base.
    pub entry: Option<String>,
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
    pub org: String,
    pub project: String,
    pub env: String,
    /// `sha256:<hex>` of the release manifest.
    pub manifest_digest: String,
    pub artifact_base: String,
    pub route_host: String,
    /// Exactly one application's membership, retained in Helm values for status reads.
    pub applications: wamn_catalog::EnvironmentApplications,
    pub roles: Vec<Role>,
    /// Who applied. It renders nothing and is kept with the revision (§9.2).
    pub actor: String,
    /// Registry repository used to resolve the composed HTTP entry.
    pub component_base: String,
}

/// What clears an operation that a killed writer left pending.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// A first install never finished: no revision ever served.
    Uninstall,
    /// An upgrade or rollback never finished: the previous revision serves.
    Rollback,
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
