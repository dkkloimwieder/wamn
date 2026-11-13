use super::{AttachmentType, Duration, Mapping, ServingManifest, Value, ensure, release_label};
use anyhow::Context as _;

/// The Helm release and host group name of an application: `r-<slug>-<hash>`.
///
/// The slug is the env name cut to 24 characters. The hash is the first 20 hex
/// characters of SHA-256 over `org/project/env/application`. The name is at most
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
    format!("{release_name}-{}", role.as_str())
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
            statements: manifest
                .composing_member()
                .map_or_else(Vec::new, |application| {
                    if application.statement_instances.is_empty() {
                        vec!["sql-0".to_owned()]
                    } else {
                        application.statement_instances.keys().cloned().collect()
                    }
                }),
            config: target(&[]),
        });
    }
    roles
}

/// The canonical JSON of the declared applications, as `--applications`
/// carries it.
pub(super) fn applications_argument(
    applications: &wamn_catalog::EnvironmentApplications,
) -> anyhow::Result<String> {
    let value = serde_json::to_value(applications).context("encode the applications")?;
    String::from_utf8(wamn_digest::canonical_json_bytes(&value))
        .context("the canonical applications are UTF-8")
}

/// The host group's `plugins` entry for the materializer's `events` binding
/// (docs/plan/alignment.md R6). Its grants are the environment's own stream and
/// subjects, and its replies use the environment user's inbox. It names no
/// `servers`, so it inherits the data connection and its certificate (R5).
/// `workloadConfig: deny` keeps every connection key and grant here.
pub fn events_plugin(org: &str, project: &str, env: &str) -> anyhow::Result<Value> {
    let target = wamn_control_registry::Triple::new(org, project, env);
    let source = wamn_broker::events::source_stream_config(&target, 1, Duration::from_secs(120));
    let project_env_key = wamn_control_registry::project_env_key(org, project, env);
    let config = mapping([
        (
            "inbox-prefix",
            format!(
                "_INBOX_{}",
                wamn_control_registry::environment_nats_user(&project_env_key)
            ),
        ),
        ("stream-allow", source.name),
        ("subject-allow", source.subjects.join(",")),
        ("subscription-capacity-bytes", "4194304".to_owned()),
        ("subscription-capacity", "64".to_owned()),
        ("max-in-flight", "64".to_owned()),
    ]);
    let mut binding = Mapping::new();
    binding.insert("config".into(), config);
    let mut bindings = Mapping::new();
    bindings.insert("events".into(), Value::Mapping(binding));
    let mut entry = Mapping::new();
    entry.insert("id".into(), "wasmcloud-nats".into());
    entry.insert("workloadConfig".into(), "deny".into());
    entry.insert("bindings".into(), Value::Mapping(bindings));
    Ok(Value::Sequence(vec![Value::Mapping(entry)]))
}

/// Render the install values of one application.
pub fn values(input: &ValuesInput) -> anyhow::Result<Value> {
    ensure!(
        MATERIALIZER_DRAIN_BOUND_SECONDS < input.drain_bound_seconds,
        "materializer_drain_bound_seconds ({MATERIALIZER_DRAIN_BOUND_SECONDS}) must be shorter than drain_bound_seconds ({})",
        input.drain_bound_seconds
    );
    ensure!(
        input.applications.len() == 1,
        "a release must select exactly one application"
    );
    let application = input.applications.keys().next().expect("one application");
    let name = release_name(&input.org, &input.project, &input.env, application)?;
    let label = release_label(&input.manifest_digest)
        .context("the release manifest digest is not a sha256 digest")?;
    for key in ["name", "namespace", "service", "extraArgs", "image"] {
        ensure!(
            !input.host_group.contains_key(key),
            "the host group body sets `{key}`, which the release chart derives"
        );
    }
    let environment = format!("{}/{}/{}", input.org, input.project, input.env);
    let mut group = input.host_group.clone();
    group.insert("name".into(), name.into());
    let variables = group
        .entry("env".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the host group body's `env` is not a list")?;
    ensure!(
        !variables.iter().any(|variable| {
            variable["name"]
                .as_str()
                .is_some_and(|name| REFUSED_VARIABLES.contains(&name))
        }),
        "the host group body sets a target variable, which the release chart derives"
    );
    // The chart appends the group's env to its runtime defaults without
    // merging by name, and the API server refuses a repeated name.
    let defaults = runtime_default_variables()?;
    if let Some(name) = variables
        .iter()
        .filter_map(|variable| variable["name"].as_str())
        .find(|name| defaults.iter().any(|default| default == name))
    {
        anyhow::bail!(
            "the host group body sets {name}, which the release chart's runtime defaults set on every host"
        );
    }
    // The http Workload image is the entry path joined with the base the
    // hosts resolve members against.
    let component_base = variables
        .iter()
        .find(|variable| variable["name"].as_str() == Some("WAMN_COMPONENT_ARTIFACT_BASE"))
        .and_then(|variable| variable["value"].as_str())
        .map(str::to_owned);
    for (variable, value) in HOST_VARIABLES.into_iter().zip([
        input.org.clone(),
        input.project.clone(),
        input.drain_bound_seconds.to_string(),
        MATERIALIZER_DRAIN_BOUND_SECONDS.to_string(),
    ]) {
        variables.push(mapping([("name", variable.to_owned()), ("value", value)]));
    }
    for (variable, value) in TELEMETRY_VARIABLES {
        variables.push(mapping([
            ("name", variable.to_owned()),
            ("value", value.to_owned()),
        ]));
    }
    group.insert(
        "extraArgs".into(),
        Value::Sequence(vec![
            format!("--release-artifact-base={}", input.artifact_base).into(),
            format!("--release-manifest-digest={}", input.manifest_digest).into(),
            format!("--environment={}", input.env).into(),
            "--files-addr=0.0.0.0:8082".into(),
            format!(
                "--applications={}",
                applications_argument(&input.applications)?
            )
            .into(),
        ]),
    );
    group
        .entry("ports".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the host group body's `ports` is not a list")?
        .push(Value::Mapping(Mapping::from_iter([
            ("name".into(), "files".into()),
            ("containerPort".into(), 8082.into()),
            ("protocol".into(), "TCP".into()),
        ])));
    group
        .entry("networking".into())
        .or_insert_with(|| Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .context("the host group body's `networking` is not a mapping")?
        .entry("socketEgress".into())
        .or_insert_with(|| SOCKET_EGRESS.into());
    ensure!(
        !group.contains_key("plugins"),
        "the host group body sets `plugins`, which the release chart derives"
    );
    group.insert(
        "plugins".into(),
        events_plugin(&input.org, &input.project, &input.env)?,
    );
    let roles = input
        .roles
        .iter()
        .map(|role| {
            let mut entry = Mapping::new();
            entry.insert("name".into(), role.name.as_str().into());
            entry.insert("config".into(), Value::Mapping(role.config.clone()));
            if let Some(recorded) = &role.entry {
                let image = wamn_catalog::composed_entry_start_reference(
                    component_base.as_deref(),
                    recorded,
                )
                .context("the host group's WAMN_COMPONENT_ARTIFACT_BASE resolves the entry")?;
                entry.insert("entry".into(), image.into());
            }
            entry.insert(
                "statements".into(),
                Value::Sequence(role.statements.iter().cloned().map(Value::String).collect()),
            );
            Ok(Value::Mapping(entry))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut runtime = Mapping::new();
    runtime.insert(
        "terminationGracePeriodSeconds".into(),
        input.drain_bound_seconds.into(),
    );
    runtime.insert("podLabels".into(), mapping([("wamn.release", label)]));
    runtime.insert(
        "podAnnotations".into(),
        mapping([
            ("wamn.release-digest", input.manifest_digest.clone()),
            ("wamn.environment", environment.clone()),
            ("wamn.application", application.clone()),
        ]),
    );
    let mut account = Mapping::new();
    account.insert(
        "name".into(),
        wamn_control_registry::service_account_name(&input.org, &input.project, &input.env).into(),
    );
    account.insert("create".into(), false.into());
    runtime.insert("serviceAccount".into(), Value::Mapping(account));
    runtime.insert("automountServiceAccountToken".into(), true.into());
    runtime.insert(
        "hostGroups".into(),
        Value::Sequence(vec![Value::Mapping(group)]),
    );
    let mut operator = Mapping::new();
    operator.insert("runtime".into(), Value::Mapping(runtime));
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
    values.insert("runtime-operator".into(), Value::Mapping(operator));
    // The host's data connection is the environment's NATS user
    // (docs/plan/alignment.md R5).
    let project_env_key =
        wamn_control_registry::project_env_key(&input.org, &input.project, &input.env);
    let mut global = Mapping::new();
    global.insert(
        "certificates".into(),
        mapping([("dataCertSecretName", format!("wamn-nats-{project_env_key}"))]),
    );
    values.insert("global".into(), Value::Mapping(global));
    Ok(Value::Mapping(values))
}

pub(super) fn recovery(status: &str) -> Option<Recovery> {
    match status {
        "pending-install" => Some(Recovery::Uninstall),
        "pending-upgrade" | "pending-rollback" => Some(Recovery::Rollback),
        _ => None,
    }
}

/// Split `<repository>:<tag>@sha256:<hex>` into the repository and
/// `<tag>@sha256:<hex>`, the form the subchart's split image fields take.
pub(super) fn split_host_image(image: &str) -> anyhow::Result<(String, String)> {
    let (name, digest) = image
        .rsplit_once('@')
        .with_context(|| format!("the host image {image} is not pinned by digest"))?;
    ensure!(
        digest.starts_with("sha256:"),
        "the host image {image} is not pinned by a sha256 digest"
    );
    let (repository, tag) = name
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .with_context(|| format!("the host image {image} has no tag"))?;
    Ok((repository.to_owned(), format!("{tag}@{digest}")))
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
    /// `<repository>@sha256:<hex>`.
    pub materializer: String,
}

/// One application HTTP workload with its configuration.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: RoleName,
    /// The HTTP entry path recorded by the release, relative to the component
    /// artifact base. [`values`] joins it with the host group's base.
    pub entry: Option<String>,
    /// The entry's named SQL import instances.
    pub statements: Vec<String>,
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
    /// Exactly one application's membership, carried in `--applications` and
    /// retained in Helm values for status reads.
    pub applications: wamn_catalog::EnvironmentApplications,
    pub roles: Vec<Role>,
    /// The drain bound of the environment policy, rendered as the host pods'
    /// `terminationGracePeriodSeconds` and as the host's
    /// `WAMN_DRAIN_BOUND_SECONDS` (R20).
    pub drain_bound_seconds: u32,
    /// Who applied. It renders nothing and is kept with the revision (§9.2).
    pub actor: String,
    /// The body of the one host group entry: env, volumes, volumeMounts,
    /// replicas, http, resources, ociCaPaths. The name, namespace, service and
    /// release arguments are this module's and are refused here.
    pub host_group: Mapping,
}

/// What clears an operation that a killed writer left pending.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    /// A first install never finished: no revision ever served.
    Uninstall,
    /// An upgrade or rollback never finished: the previous revision serves.
    Rollback,
}

/// The host variables that carry the environment coordinate (R1). The host
/// derives its project from the coordinate, because the manifest names none.
/// The route host is not one of them: it lives only on the http workload's
/// `config.host` (R2). The environment name is not one of them either: the
/// operator chart sets `WASMCLOUD_HOST_ENVIRONMENT` to the pod namespace,
/// which every environment shares (R6), and a second entry of one name is
/// refused by server-side apply. The release passes `--environment` again in
/// `extraArgs`, after the chart's, and the host keeps the last value as its
/// release target. The host reads `WASMCLOUD_HOST_ENVIRONMENT` once, as its
/// host namespace, which its heartbeat names and where the operator places
/// the release's workloads; it is never the environment.
/// `WAMN_DRAIN_BOUND_SECONDS` is the policy's drain bound, the one budget of
/// the host's shutdown sequence, from the same value as the pods'
/// `terminationGracePeriodSeconds` (R20).
pub(super) const HOST_VARIABLES: [&str; 4] = [
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WAMN_DRAIN_BOUND_SECONDS",
    "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS",
];

/// The materializer's grace: when its host drains, the host stops the
/// materializer and answers the operator's stop within this time, before it
/// closes admission. It must be shorter than the drain bound, so a host pod
/// never leaves a materializer replica that the operator cannot stop.
pub const MATERIALIZER_DRAIN_BOUND_SECONDS: u32 = 10;

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

/// The env names that `runtime-operator.runtime.env` of the release chart's
/// values sets on every host (the one host default, D1-5).
pub(super) fn runtime_default_variables() -> anyhow::Result<Vec<String>> {
    let chart: Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/platform/release/values.yaml"
    ))
    .context("decode the release chart values")?;
    Ok(chart["runtime-operator"]["runtime"]["env"]
        .as_sequence()
        .context("the release chart values have runtime env")?
        .iter()
        .filter_map(|variable| variable["name"].as_str().map(str::to_owned))
        .collect())
}

/// The host variables a host group body must not set: the derived ones, the
/// telemetry ones, and the chart's environment variable, which the release
/// replaces by argument. The runtime defaults are refused beside them.
pub(super) const REFUSED_VARIABLES: [&str; 9] = [
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WAMN_DRAIN_BOUND_SECONDS",
    "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_SERVICE_NAME",
    "OTEL_TRACES_SAMPLER",
    "OTEL_TRACES_SAMPLER_ARG",
    "WASMCLOUD_HOST_ENVIRONMENT",
];

/// The host group's raw-socket egress mode. Chart 2.11.0 defaults it to
/// enforce, as does WAMN unless the host group body names a mode.
pub const SOCKET_EGRESS: &str = "enforce";
