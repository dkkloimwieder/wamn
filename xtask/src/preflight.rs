//! Preflight before a kind platform deploy: the charts render, their images
//! resolve, the wamn containers accept their arguments, and the Kubernetes API
//! server accepts every object. It checks Pods and supported built-in workload
//! templates under the target namespace's Pod Security policy.
//!
//! The values come from the product's builders. Values that exist only at deploy
//! time are stand-ins:
//! - the project instance suffix (`STAND_IN_INSTANCE`);
//! - the NATS public keys behind the host's workflow and ingress inboxes (`STAND_IN_NATS_KEY`);
//! - the release package digest and the component image digests (`helm_size` placeholders);
//! - with `--print-cluster-requests` only, the registry endpoint and the API server addresses.
//!
//! Cluster steps use kube-rs, not kubectl: kube-rs is already a dependency, it
//! sends one server-side apply per object without a process for each, and it
//! reports the API status of each refusal.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use clap::Args as ClapArgs;
use k8s_openapi::api::core::v1::{Namespace, Pod, Secret, ServiceAccount};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, Patch, PatchParams, PostParams};
use kube::core::GroupVersionKind;
use kube::discovery::{ApiCapabilities, ApiResource, Scope};
use serde_json::{Value, json};
use wamn_env::env::document::EnvFile;
use wamn_env::env::{ensure as host, lifecycle, operation};
use wamn_env::platform_env;
use wamn_env::provision_project_env::workload;
use wamn_kubernetes::org_resources;
use wamn_platform_install::release_chart::POD_REGISTRY;
use wamn_platform_install::release_chart::bundle;
use wamn_platform_install::{deployment, env_install, org_install, platform_install};
use wamn_postgres::database::{capture_configuration, connection_for};
use wamn_registry::{OrgId, name};

use crate::kind::RegistryEndpoint;

const CONTEXT: &str = "kind-wamn-local";
const LOCAL_REGISTRY: &str = "localhost:5001";
const PLATFORM: &str = "platform";
const INFRASTRUCTURE_RELEASE: &str = "wamn-infrastructure";
const FIELD_MANAGER: &str = "helm";
const CRD_MANAGER: &str = "wamn-preflight";
const PREFLIGHT_ACCOUNT: &str = "wamn-preflight";
const ENFORCE_LABEL: &str = "pod-security.kubernetes.io/enforce";
const WARN_LABEL: &str = "pod-security.kubernetes.io/warn";
const DEADLINE: Duration = Duration::from_secs(120);
const RUN_DEADLINE: Duration = Duration::from_secs(300);
const ORG: &str = "local";
const OWNER: &str = "owner@example.com";
const APP: &str = "apps/wamn_receiving";
/// The kind example of docs/operations/deployment.md.
const ENV: &str = "org: local
env: dev
target: cloud
revocation_seconds: 60
database:
  instances: 1
  storage: 2Gi
  cpu: 200m
  memory: 256Mi
projects:
  - name: receiving
    apps:
      wamn_receiving:
        label: receiving
";
/// Stand-in for the suffix that env apply reserves.
const STAND_IN_INSTANCE: &str = "00000000";
/// Stand-in for the public key of a host NATS user.
const STAND_IN_NATS_KEY: &str = "UAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
/// Stand-ins for the kind network when the cluster is not read.
const STAND_IN_REGISTRY_ADDRESS: &str = "172.18.0.2";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// A bundle from cargo xtask build-platform. Without it, one is built under target/preflight.
    #[arg(long)]
    chart: Option<PathBuf>,
    /// Print the cluster requests instead of sending them. Nothing reads the cluster.
    #[arg(long)]
    print_cluster_requests: bool,
}

/// The objects of one rendered chart and the namespace Helm gives the others.
struct Rendered {
    label: String,
    namespace: String,
    objects: Vec<Value>,
}

/// Result counts for each step.
#[derive(Default)]
struct Tally {
    counts: BTreeMap<&'static str, [usize; 3]>,
}

impl Tally {
    fn check(&mut self, step: &'static str, ok: bool, text: impl Display) {
        let counts = self.counts.entry(step).or_default();
        counts[usize::from(!ok)] += 1;
        println!("{} {step} {text}", if ok { "ok  " } else { "FAIL" });
    }

    fn note(&mut self, step: &'static str, label: &str, text: impl Display) {
        self.counts.entry(step).or_default()[2] += 1;
        println!("note {step} {label}: {text}");
    }

    fn failed(&self) -> usize {
        self.counts.values().map(|counts| counts[1]).sum()
    }
}

pub async fn run(root: &Path, args: Args) -> anyhow::Result<()> {
    let directory = root.join("target/preflight");
    std::fs::create_dir_all(directory.join("render"))?;
    let bundle = if let Some(chart) = args.chart {
        chart
    } else {
        let out = directory.join("bundle");
        if out.exists() {
            std::fs::remove_dir_all(&out)?;
        }
        crate::platform::run(crate::platform::Args {
            registry: LOCAL_REGISTRY.into(),
            out: out.clone(),
            repository: root.to_owned(),
            stamp_only: false,
            cluster: None,
        })
        .await?;
        out
    };
    let (endpoint, client) = if args.print_cluster_requests {
        let endpoint = RegistryEndpoint {
            address: STAND_IN_REGISTRY_ADDRESS.into(),
            port: 5000,
        };
        (endpoint, None)
    } else {
        let endpoint = crate::kind::create_cluster(root).await?;
        let options = kube::config::KubeConfigOptions {
            context: Some(CONTEXT.into()),
            ..Default::default()
        };
        let config = kube::Config::from_kubeconfig(&options).await?;
        (endpoint, Some(kube::Client::try_from(config)?))
    };
    let api_addresses = match &client {
        Some(client) => org_resources::api_addresses(client.clone()).await?,
        // The kind defaults: the kubernetes Service and its control plane endpoint.
        None => BTreeSet::from([([10, 96, 0, 1].into(), 443), ([172, 18, 0, 3].into(), 6443)]),
    };
    let api_server = org_install::api_destinations(api_addresses)?;

    let mut tally = Tally::default();
    let (rendered, deploy_bases) = render(
        root,
        &bundle,
        &endpoint,
        &api_server,
        &directory,
        &mut tally,
    )
    .await?;
    images(&rendered, &deploy_bases, &mut tally).await?;
    arguments(&rendered, &mut tally).await?;
    match client {
        Some(client) => cluster(&client, &rendered, &mut tally).await?,
        None => print_cluster(&rendered, &mut tally),
    }

    println!("summary:");
    for (step, [ok, failed, noted]) in &tally.counts {
        println!("  {step}: {ok} ok, {failed} FAIL, {noted} noted");
    }
    let failed = tally.failed();
    ensure!(failed == 0, "preflight: {failed} checks failed");
    Ok(())
}

/// Render each chart with the values that the product renders on kind.
/// Returns the rendered charts and the repositories that deploy pushes to.
async fn render(
    root: &Path,
    bundle_directory: &Path,
    endpoint: &RegistryEndpoint,
    api_server: &[Value],
    directory: &Path,
    tally: &mut Tally,
) -> anyhow::Result<(Vec<Rendered>, Vec<String>)> {
    let chart = bundle::select(bundle_directory)?;
    let charts = chart.directory().join("platform");
    let mut rendered = Vec::new();

    // wamn platform deploy: the kind values file with the registryEndpoint block.
    let mut platform_values: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(charts.join("platform/values-kind.yaml"))?)?;
    platform_values["registryEndpoint"] = serde_yaml::to_value(json!({
        "addressType": "IPv4", "address": endpoint.address, "port": endpoint.port,
    }))?;
    let platform_file = directory.join("platform-kind.yaml");
    std::fs::write(&platform_file, serde_yaml::to_string(&platform_values)?)?;
    let chart_reference = std::fs::read_to_string(chart.directory().join("chart.ref"))
        .context("read chart.ref of the bundle")?;
    let resolved = json!({
        "appChart": chart_reference.trim(),
        "registryEgress": platform_install::registry_egress(&platform_file)?,
        "apiServer": api_server,
    });
    let resolved_file = write_json(directory, "platform-resolved.json", &resolved)?;
    rendered.push(Rendered {
        label: "platform deploy".into(),
        namespace: PLATFORM.into(),
        objects: vec![platform_install::platform_namespace(PLATFORM)],
    });
    rendered.push(
        template(
            directory,
            "infrastructure",
            INFRASTRUCTURE_RELEASE,
            &charts.join("infra"),
            PLATFORM,
            &[&platform_file],
            true,
            tally,
        )
        .await?,
    );
    let platform = template(
        directory,
        "platform",
        "wamn",
        &charts.join("platform"),
        PLATFORM,
        &[&platform_file, &resolved_file],
        true,
        tally,
    )
    .await?;

    // wamn org create: org values from the rendered platform inputs.
    let org: OrgId = ORG.parse()?;
    let inputs = &find(&platform.objects, "ConfigMap", "wamn-org-inputs")?["data"];
    let sql_config = &find(&platform.objects, "ConfigMap", "wamn-system-sql")?["data"];
    let database: wamn_postgres::database::ConnectionConfig = serde_json::from_str(
        sql_config["database.json"]
            .as_str()
            .context("wamn-system-sql has no database.json")?,
    )?;
    let org_values = wamn_platform_install::org::org_values(inputs, &database, &org, api_server)?;
    let helm_namespace = wamn_kubernetes::helm::org_release_namespace(ORG).into_owned();
    let org_file = write_json(directory, "org-values.json", &org_values)?;
    let org_chart = template(
        directory,
        "org",
        "wamn-org",
        &charts.join("org"),
        &helm_namespace,
        &[&org_file],
        false,
        tally,
    )
    .await?;
    let control = &org_values["control"]["configuration"];
    let image = control["worker_image"]
        .as_str()
        .context("org values have no control image")?;
    let org_objects = org_create_objects(
        directory,
        &org,
        &helm_namespace,
        image,
        &database,
        inputs,
        &org_values,
    )?;
    rendered.push(org_chart);
    rendered.push(Rendered {
        label: "org create".into(),
        namespace: PLATFORM.into(),
        objects: org_objects,
    });

    // wamn env apply: the environment host and the project release.
    let env_file = directory.join("env.yaml");
    std::fs::write(&env_file, ENV)?;
    let file = EnvFile::compile(&env_file)?;
    let sizing = serde_json::to_value(&file.database)?;
    let mut document = file
        .into_projects()
        .next()
        .context("the environment has one project")?;
    let triple = document.triple();
    let env_namespace = name::env_key(&triple.org, &triple.env);
    // Stand-in database settings. Provisioning itself reads the committed quota.
    let default_cpu_millicores = sql_config[env_install::ENV_DEFAULT_CPU_MILLICORES_FILE]
        .as_str()
        .map(str::parse::<i64>)
        .transpose()
        .context("decode the platform env CPU request")?
        .unwrap_or(200);
    let quota = serde_json::from_value(json!({
        "pods": 20, "jobs": 10, "persistent_volume_claims": 10, "secrets": 100,
        "config_maps": 100, "services": 20, "clusters": 10, "workload_deployments": 20,
        "storage_bytes": 10_i64 << 30, "cpu_millicores": 4000,
        "memory_request_bytes": 4_i64 << 30, "memory_limit_bytes": 8_i64 << 30,
        "default_cpu_millicores": default_cpu_millicores, "default_memory_request_bytes": 256 << 20,
        "default_memory_limit_bytes": 1 << 30,
    }))?;
    let mut env_objects = env_install::manifests(
        &triple.org,
        &triple.env,
        [
            wamn_platform_install::org::field(inputs, "applyDatabaseHost")?,
            wamn_platform_install::org::field(inputs, "applyTransactionDatabaseHost")?,
        ],
        database.port,
        api_server,
        org_values["networkPolicy"]["registry"]
            .as_array()
            .context("org has no registry destinations")?,
        &quota,
    )?;
    let worker_login = "wamn_apply_env_0000000000000000000000000000000000000000".parse()?;
    let worker_configuration = env_install::worker_configuration(
        inputs,
        &database,
        &triple.org,
        &env_namespace,
        &worker_login,
        api_server,
    )?;
    let postgres = serde_json::from_str(
        worker_configuration[1]["data"]["postgres.json"]
            .as_str()
            .context("worker PostgreSQL configuration")?,
    )?;
    env_objects.extend(worker_configuration);
    env_objects.extend(env_install::sql_prerequisites(&env_namespace));
    env_objects.push(org_install::env_provision_job(
        &triple.org,
        &triple.env,
        image,
        &database,
    ));
    rendered.push(Rendered {
        label: "env create".into(),
        namespace: env_namespace.clone(),
        objects: env_objects,
    });

    let instance: wamn_registry::instance_suffix::InstanceSuffix = STAND_IN_INSTANCE.parse()?;
    let connection = connection_for(&triple, &instance, &env_namespace);
    let helm_namespace = wamn_kubernetes::helm::org_release_namespace(&env_namespace);
    let connections = workload::connections(&connection)?;
    let registry = control["registry"]
        .as_str()
        .context("control configuration has no registry")?;
    let registry_token_metadata = control["registry_token_metadata"] == true;
    let host_image = deployment::host_image(chart.directory()).await?;

    let (org_id, env, project) = (&triple.org, &triple.env, &triple.project);
    let inbox = name::nats_user_inbox(STAND_IN_NATS_KEY);
    let nats_url = wamn_platform_install::org::field(control, "nats_url")?;
    let identity_issuer = wamn_platform_install::org::field(control, "identity_issuer")?;
    let insecure_registry = wamn_registry::registry_is_loopback(registry)
        || registry.split('/').next() == Some(POD_REGISTRY);
    let host_values = host::host_values(
        org_id,
        env,
        &env_namespace,
        &host_image,
        &serde_json::Map::new(),
        document.drain_bound_seconds,
        &serde_json::Map::from_iter([(
            project.to_string(),
            host::host_project_values(&triple, &connection)?,
        )]),
        nats_url,
        identity_issuer,
        registry_token_metadata,
        insecure_registry,
        &[],
        &control["oauth_encryption"],
        &inbox,
        &inbox,
    );
    let host_file = write_json(directory, "env-values.json", &host_values)?;
    rendered.push(
        template(
            directory,
            "env",
            &name::env_host_name(org_id, env),
            &charts.join("env"),
            &helm_namespace,
            &[&host_file],
            false,
            tally,
        )
        .await?,
    );

    // lifecycle.rs::configure: the project release values.
    let cluster = name::project_cluster(&connection.database);
    let publication = name::cdc_object_name(org_id, project, env, &instance);
    let mut project_values = platform_env::database_values(&connection, 1, sizing, &postgres)?;
    wamn_env::env::chart_values::set_operation_values(
        &mut project_values,
        &triple,
        &cluster,
        &connection.database,
        document.apps.keys(),
    )?;
    project_values["publication"] = wamn_env::env::chart_values::publication_values(
        &triple,
        &instance,
        &cluster,
        &publication,
        &[],
    )["publication"]
        .take();
    project_values["databaseConfig"] =
        workload::configuration_values(&triple, &instance, &connections)?;
    project_values["cdcReader"] = lifecycle::reader_values(
        &triple,
        &capture_configuration(connection)?,
        &host_image,
        nats_url,
        registry_token_metadata,
    );
    project_values["cdcReader"]["pause"] = false.into();
    let project_release = name::project_env_resource_name(org_id, project, env);
    let project_file = write_json(directory, "project-values.json", &project_values)?;
    rendered.push(
        template(
            directory,
            "project",
            &project_release,
            &charts.join("project"),
            &helm_namespace,
            &[&project_file],
            false,
            tally,
        )
        .await?,
    );

    // wamn deploy: the application release values, as operation.rs renders them.
    let manifest = app_manifest(root).await?;
    let package = manifest.package.id.clone();
    let app = document
        .apps
        .keys()
        .next()
        .context("the project has one application")?
        .clone();
    let selection = document
        .apps
        .get_mut(&app)
        .context("the application is declared")?;
    let digest = crate::helm_size::package_digest(&package);
    selection.current = wamn_env::env::document::DeclaredRelease::Digest(digest);
    selection.packages = BTreeSet::from([package.clone()]);
    let schemas: Vec<wamn_package::canonical_name::CanonicalName> = manifest
        .models
        .values()
        .map(|model| model.schema.clone().into())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let manifests = BTreeMap::from([(package, manifest)]);
    let platform_registry = wamn_platform_install::org::field(control, "platform_registry")?;
    let integration_base = format!("{platform_registry}/wamn/integrations");
    let (_, component_base) = wamn_registry::org_artifact_bases(registry);
    let platform_domain = inputs["base"]
        .as_str()
        .context("org inputs have no base")?
        .parse()?;
    let mut app_values = wamn_env::env::write::release_values(
        &document,
        &env_namespace,
        &platform_domain,
        &app,
        &manifests,
        &digest,
        &[],
        &integration_base,
        &component_base,
        wamn_http::wasm_authority::wasm_authority_port,
    )?;
    operation::use_pod_registry(&chart, &mut app_values)?;
    let app_file = directory.join("app-values.yaml");
    std::fs::write(&app_file, serde_yaml::to_string(&app_values)?)?;
    rendered.push(
        template(
            directory,
            "app",
            &wamn_env::release_chart::release_name(org_id, project, env, &app)?,
            &charts.join("app"),
            &helm_namespace,
            &[&app_file],
            false,
            tally,
        )
        .await?,
    );

    // After deploy, the project release gains the publication schemas (operation.rs, --reuse-values).
    project_values["publication"] = wamn_env::env::chart_values::publication_values(
        &triple,
        &instance,
        &cluster,
        &publication,
        &schemas,
    )["publication"]
        .take();
    let deployed_file = write_json(directory, "project-deployed-values.json", &project_values)?;
    rendered.push(
        template(
            directory,
            "project after deploy",
            &project_release,
            &charts.join("project"),
            &helm_namespace,
            &[&deployed_file],
            false,
            tally,
        )
        .await?,
    );
    // The org inputs borrow the platform render until here.
    rendered.insert(1, platform);
    Ok((rendered, vec![integration_base, component_base]))
}

/// The objects that wamn org create applies outside the chart. A copy of the
/// shapes in the private org.rs::create, org_job and registry_pull_secret.
fn org_create_objects(
    directory: &Path,
    org: &OrgId,
    helm_namespace: &str,
    image: &str,
    database: &wamn_postgres::database::ConnectionConfig,
    inputs: &Value,
    org_values: &Value,
) -> anyhow::Result<Vec<Value>> {
    use wamn_platform_install::org::{field, registry_configuration};

    let mut objects = vec![org_resources::helm_storage_namespace(helm_namespace)];
    if org_values["control"]["configuration"]["registry_token_metadata"] == false {
        // The registry auth file that docs/operations/deployment.md writes on kind.
        let auth = write_json(
            directory,
            "registry-auth.json",
            &json!({"auths": {LOCAL_REGISTRY: {}}}),
        )?;
        let config = registry_configuration(
            &auth,
            field(inputs, "registry")?,
            field(org_values, "registry")?,
        )?;
        objects.push(org_resources::registry_credentials(org, &config));
    }
    let mut sql = org_install::provision_job(org, image, database);
    let mut invitation = org_install::invitation_job(org, image, OWNER);
    // The installer supplies these namespaces on its API requests.
    sql["metadata"]["namespace"] = PLATFORM.into();
    invitation["metadata"]["namespace"] = org.as_str().into();
    objects.extend([sql, invitation]);
    std::fs::write(
        directory.join("render/org-create.json"),
        serde_json::to_vec_pretty(&objects)?,
    )?;
    Ok(objects)
}

/// The published manifest of the deployed application, with placeholder digests.
async fn app_manifest(root: &Path) -> anyhow::Result<wamn_component_policy::PackageManifest> {
    let source = wamn_build::prepare_source(&root.join(APP)).await?;
    ensure!(
        source.bases().is_empty(),
        "{APP} has base packages, which preflight does not build"
    );
    let output_root = wamn_build::default_output_root(source.root());
    let request = wamn_build::BuildRequest::new(
        source.root(),
        &output_root,
        wamn_build::DescribeMode::Cached,
    );
    wamn_build::build_prepared(&request, &source, wamn_build::BuildMode::Published)
        .await
        .with_context(|| format!("build {APP}"))?;
    crate::helm_size::published_manifest(&wamn_build::package_output(
        &output_root,
        &source.package().id,
    ))
}

/// Run helm template, keep its output under `render/`, and parse its objects.
#[expect(clippy::too_many_arguments, reason = "one Helm invocation")]
async fn template(
    directory: &Path,
    label: &str,
    release: &str,
    chart: &Path,
    namespace: &str,
    values: &[&Path],
    crds: bool,
    tally: &mut Tally,
) -> anyhow::Result<Rendered> {
    let mut arguments: Vec<&OsStr> = vec![
        "template".as_ref(),
        release.as_ref(),
        chart.as_os_str(),
        "--namespace".as_ref(),
        namespace.as_ref(),
        if crds {
            "--include-crds"
        } else {
            "--skip-crds"
        }
        .as_ref(),
    ];
    for file in values {
        arguments.extend(["--values".as_ref(), file.as_os_str()]);
    }
    let output = match wamn_kubernetes::helm::run(&arguments).await {
        Ok(output) => output,
        Err(error) => {
            tally.check("render", false, format!("{label}: {error:#}"));
            bail!("render {label}");
        }
    };
    std::fs::write(
        directory.join(format!("render/{}.yaml", label.replace(' ', "-"))),
        &output,
    )?;
    let mut objects = serde_yaml::Deserializer::from_slice(&output)
        .map(|document| -> anyhow::Result<Value> { Ok(serde::Deserialize::deserialize(document)?) })
        .filter(|document| !matches!(document, Ok(Value::Null)))
        .collect::<anyhow::Result<Vec<_>>>()
        .with_context(|| format!("parse the {label} render"))?;
    for object in &mut objects {
        normalize_file_modes(object, false);
    }
    tally.check(
        "render",
        true,
        format!("{label} ({} objects)", objects.len()),
    );
    Ok(Rendered {
        label: label.into(),
        namespace: namespace.into(),
        objects,
    })
}

// serde_yaml hides scalar style, so apply YAML 1.1 octal parsing only to volume file modes.
fn normalize_file_modes(value: &mut Value, in_file_volume: bool) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if in_file_volume
                    && matches!(key.as_str(), "defaultMode" | "mode")
                    && let Some(mode) = child.as_str()
                    && mode.len() > 1
                    && mode.starts_with('0')
                    && let Ok(mode) = u64::from_str_radix(mode, 8)
                {
                    *child = mode.into();
                } else if key == "volumes"
                    && let Some(volumes) = child.as_array_mut()
                {
                    for volume in volumes {
                        for name in ["configMap", "downwardAPI", "projected", "secret"] {
                            if let Some(source) = volume.get_mut(name) {
                                normalize_file_modes(source, true);
                            }
                        }
                    }
                } else {
                    normalize_file_modes(child, in_file_volume);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                normalize_file_modes(entry, in_file_volume);
            }
        }
        _ => {}
    }
}

/// Check every image reference by the exit code of imagetools inspect.
async fn images(
    rendered: &[Rendered],
    deploy_bases: &[String],
    tally: &mut Tally,
) -> anyhow::Result<()> {
    let argument = regex::Regex::new(r"^--[A-Za-z0-9-]*image=(\S+)$")?;
    let mut references = BTreeMap::<String, BTreeSet<String>>::new();
    for chart in rendered {
        for object in &chart.objects {
            let mut found = Vec::new();
            image_references(object, &argument, &mut found);
            for reference in found {
                references.entry(reference).or_default().insert(format!(
                    "{} {}",
                    chart.label,
                    describe(object)
                ));
            }
        }
    }
    for (reference, users) in references {
        let users = users.into_iter().collect::<Vec<_>>().join(", ");
        if deploy_bases
            .iter()
            .any(|base| reference.starts_with(base.as_str()))
        {
            tally.note(
                "images",
                "pushed at deploy",
                format!("{reference} ({users})"),
            );
            continue;
        }
        let local = local_reference(&reference);
        let status = tokio::process::Command::new("docker")
            .args([
                "buildx",
                "--builder",
                crate::platform::BUILDER,
                "imagetools",
                "inspect",
                &local,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        let shown = if local == reference {
            reference
        } else {
            format!("{reference} as {local}")
        };
        tally.check("images", status.success(), format!("{shown} ({users})"));
    }
    Ok(())
}

fn image_references(value: &Value, argument: &regex::Regex, found: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "image" | "imageName")
                    && let Some(image) = child.as_str().filter(|image| !image.is_empty())
                {
                    found.push(image.into());
                }
                image_references(child, argument, found);
            }
            if let (Some(name), Some(image)) = (fields.get("name"), fields.get("value"))
                && name
                    .as_str()
                    .is_some_and(|name| name.to_ascii_uppercase().contains("IMAGE"))
                && let Some(image) = image.as_str().filter(|image| !image.is_empty())
            {
                found.push(image.into());
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                image_references(entry, argument, found);
            }
        }
        Value::String(text) => {
            if let Some(captures) = argument.captures(text) {
                found.push(captures[1].into());
            }
        }
        _ => {}
    }
}

/// Run each wamn container with its rendered command and arguments plus --help.
async fn arguments(rendered: &[Rendered], tally: &mut Tally) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for chart in rendered {
        for object in &chart.objects {
            let mut found = Vec::new();
            containers(object, &mut found);
            for (pod, container) in found {
                let image = container["image"].as_str().unwrap_or_default();
                if !wamn_image(image) {
                    continue;
                }
                let environment: BTreeMap<&str, &str> = container["env"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| Some((entry["name"].as_str()?, entry["value"].as_str()?)))
                    .collect();
                let strings = |field: &str| -> Vec<String> {
                    container[field]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|text| expand(text, &environment))
                        .collect()
                };
                let (command, mut args) = (strings("command"), strings("args"));
                args.push("--help".into());
                let user = docker_user(pod, container);
                let key = (local_reference(image), user, command, args);
                if seen.contains(&key) {
                    continue;
                }
                let (local, user, command, args) = &key;
                let line = command
                    .iter()
                    .chain(args)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                let subject = format!(
                    "{} {} {}: {line}",
                    chart.label,
                    describe(object),
                    container["name"].as_str().unwrap_or_default()
                );
                let container_name =
                    format!("wamn-preflight-{}-{}", std::process::id(), seen.len());
                let mut run = tokio::process::Command::new("docker");
                run.args([
                    "run",
                    "--rm",
                    "--network",
                    "none",
                    "--name",
                    &container_name,
                ]);
                if let Some(user) = user {
                    run.args(["--user", user]);
                }
                if let Some(entrypoint) = command.first() {
                    run.args(["--entrypoint", entrypoint]);
                }
                run.arg(local)
                    .args(command.iter().skip(1))
                    .args(args)
                    .kill_on_drop(true);
                if let Ok(output) = tokio::time::timeout(RUN_DEADLINE, run.output()).await {
                    let output = output?;
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let last = stderr.lines().rfind(|line| !line.trim().is_empty());
                    if output.status.success() {
                        tally.check("args", true, subject);
                    } else {
                        tally.check(
                            "args",
                            false,
                            format!("{subject}: {}", last.unwrap_or("no stderr")),
                        );
                    }
                } else {
                    tokio::process::Command::new("docker")
                        .args(["rm", "--force", &container_name])
                        .status()
                        .await?;
                    tally.check(
                        "args",
                        false,
                        format!("{subject}: no exit within {RUN_DEADLINE:?}"),
                    );
                }
                seen.insert(key);
            }
        }
    }
    Ok(())
}

fn containers<'a>(value: &'a Value, found: &mut Vec<(&'a Value, &'a Value)>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "containers" | "initContainers")
                    && let Some(entries) = child.as_array()
                {
                    found.extend(
                        entries
                            .iter()
                            .filter(|entry| entry["image"].is_string())
                            .map(|entry| (value, entry)),
                    );
                } else {
                    containers(child, found);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                containers(entry, found);
            }
        }
        _ => {}
    }
}

fn docker_user(pod: &Value, container: &Value) -> Option<String> {
    let effective_id = |field: &str| {
        container["securityContext"][field]
            .as_i64()
            .or_else(|| pod["securityContext"][field].as_i64())
    };
    let uid = effective_id("runAsUser");
    let gid = effective_id("runAsGroup");
    match (uid, gid) {
        (Some(uid), Some(gid)) => Some(format!("{uid}:{gid}")),
        (Some(uid), None) => Some(uid.to_string()),
        // The WAMN images default to UID 0.
        (None, Some(gid)) => Some(format!("0:{gid}")),
        (None, None) => None,
    }
}

fn wamn_image(reference: &str) -> bool {
    let name = reference.split('@').next().unwrap_or(reference);
    let last = name.rsplit('/').next().unwrap_or(name);
    let last = last.split(':').next().unwrap_or(last);
    crate::platform::IMAGES
        .iter()
        .any(|image| last.strip_prefix("wamn-") == Some(image))
}

/// Expand `$(NAME)` from the container environment as Kubernetes does; `$$` is `$`.
fn expand(text: &str, env: &BTreeMap<&str, &str>) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('$') {
        expanded.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(after) = rest.strip_prefix("$$") {
            expanded.push('$');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("$(")
            && let Some(end) = after.find(')')
            && let Some(value) = env.get(&after[..end])
        {
            expanded.push_str(value);
            rest = &after[end + 1..];
        } else {
            expanded.push('$');
            rest = &rest[1..];
        }
    }
    expanded.push_str(rest);
    expanded
}

/// The host reaches the pod registry at localhost:5001.
fn local_reference(reference: &str) -> String {
    reference.strip_prefix(POD_REGISTRY).map_or_else(
        || reference.to_owned(),
        |rest| format!("{LOCAL_REGISTRY}{rest}"),
    )
}

/// One object to dry-run, with its API resource and target namespace.
struct Planned<'a> {
    label: &'a str,
    object: Value,
    resource: ApiResource,
    namespace: Option<String>,
}

fn restricted_namespaces(rendered: &[Rendered]) -> BTreeMap<&str, &Value> {
    rendered
        .iter()
        .flat_map(|chart| &chart.objects)
        .filter(|object| {
            object["kind"] == "Namespace"
                && object["metadata"]["labels"][ENFORCE_LABEL] == "restricted"
        })
        .filter_map(|object| {
            Some((
                object["metadata"]["name"].as_str()?,
                &object["metadata"]["labels"],
            ))
        })
        .collect()
}

fn admission_pod(object: &Value) -> Option<Value> {
    let template = match object["kind"].as_str()? {
        "Pod" => object,
        "CronJob" => &object["spec"]["jobTemplate"]["spec"]["template"],
        "Job" | "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" => {
            &object["spec"]["template"]
        }
        // The kind run enforces restricted for Pods that operators generate,
        // including CloudNativePG Pooler Pods.
        _ => return None,
    };
    if !template["spec"]["containers"].is_array() {
        return None;
    }
    let mut pod = json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"generateName": "wamn-preflight-"},
        "spec": template["spec"],
    });
    for field in ["labels", "annotations"] {
        if let Some(value) = template["metadata"].get(field) {
            pod["metadata"][field] = value.clone();
        }
    }
    // The real service account is itself only dry-run. Pod admission requires
    // a stored account even for a dry run.
    pod["spec"]["serviceAccountName"] = PREFLIGHT_ACCOUNT.into();
    pod["spec"].as_object_mut()?.remove("serviceAccount");
    // The StatefulSet controller supplies a PVC volume for each claim template.
    if object["kind"] == "StatefulSet"
        && let Some(claims) = object["spec"]["volumeClaimTemplates"].as_array()
    {
        let mut volumes = pod["spec"]["volumes"].take();
        if volumes.is_null() {
            volumes = json!([]);
        }
        let volumes = volumes.as_array_mut()?;
        for claim in claims {
            let name = claim["metadata"]["name"].as_str()?;
            volumes.retain(|volume| volume["name"] != name);
            volumes.push(json!({
                "name": name,
                "persistentVolumeClaim": {"claimName": name},
            }));
        }
        pod["spec"]["volumes"] = json!(volumes);
    }
    Some(pod)
}

/// Install the CRDs if Helm does not own them, dry-run every object, and clean up.
async fn cluster(
    client: &kube::Client,
    rendered: &[Rendered],
    tally: &mut Tally,
) -> anyhow::Result<()> {
    let releases = Api::<Secret>::namespaced(client.clone(), PLATFORM)
        .list(&ListParams::default().labels(&format!("owner=helm,name={INFRASTRUCTURE_RELEASE}")))
        .await?;
    let helm_owns_crds = !releases.items.is_empty();
    let definitions = Api::<CustomResourceDefinition>::all(client.clone());
    let namespaces = Api::<Namespace>::all(client.clone());
    let restricted = restricted_namespaces(rendered);
    let mut installed = Vec::new();
    let mut temporary = BTreeMap::<String, String>::new();
    let result = async {
        if helm_owns_crds {
            println!("-- {INFRASTRUCTURE_RELEASE} is installed: the dry run uses the CRDs that Helm installed");
        } else {
            install_crds(&definitions, rendered, &mut installed, tally).await?;
        }
        let existing: BTreeSet<String> = namespaces
            .list(&ListParams::default())
            .await?
            .items
            .into_iter()
            .filter_map(|namespace| namespace.metadata.name)
            .collect();
        let mut resources = BTreeMap::<(String, String), (ApiResource, ApiCapabilities)>::new();
        let mut planned = Vec::new();
        for chart in rendered {
            for object in &chart.objects {
                let version = object["apiVersion"].as_str().unwrap_or_default();
                let kind = object["kind"].as_str().unwrap_or_default();
                let key = (version.to_owned(), kind.to_owned());
                if !resources.contains_key(&key) {
                    let (group, version) = version.rsplit_once('/').unwrap_or(("", version));
                    let gvk = GroupVersionKind::gvk(group, version, kind);
                    match kube::discovery::pinned_kind(client, &gvk).await {
                        Ok(resolved) => {
                            resources.insert(key.clone(), resolved);
                        }
                        Err(error) => {
                            tally.check("dry-run", false, format!("{} {}: {error}", chart.label, describe(object)));
                            continue;
                        }
                    }
                }
                let (resource, capabilities) = &resources[&key];
                let namespace = (capabilities.scope == Scope::Namespaced).then(|| {
                    object["metadata"]["namespace"]
                        .as_str()
                        .unwrap_or(&chart.namespace)
                        .to_owned()
                });
                if let Some(namespace) = &namespace
                    && !temporary.contains_key(namespace)
                    && (!existing.contains(namespace) || restricted.contains_key(namespace.as_str()))
                {
                    let mut stand_in = json!({"metadata": {"generateName": "wamn-preflight-"}});
                    if let Some(labels) = restricted.get(namespace.as_str()) {
                        stand_in["metadata"]["labels"] = json!({
                            (ENFORCE_LABEL): labels[ENFORCE_LABEL],
                            (WARN_LABEL): labels[WARN_LABEL],
                        });
                    }
                    let stand_in = namespaces
                        .create(&PostParams::default(), &serde_json::from_value(stand_in)?)
                        .await?
                        .metadata
                        .name
                        .context("the API server returned no namespace name")?;
                    println!("-- temporary namespace {stand_in} stands in for {namespace}");
                    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), &stand_in);
                    temporary.insert(namespace.clone(), stand_in);
                    if restricted.contains_key(namespace.as_str()) {
                        let account = json!({"metadata": {"name": PREFLIGHT_ACCOUNT}});
                        accounts
                            .create(&PostParams::default(), &serde_json::from_value(account)?)
                            .await
                            .with_context(|| format!("create the pod dry-run account for {namespace}"))?;
                    }
                }
                planned.push(Planned {
                    label: &chart.label,
                    object: object.clone(),
                    resource: resource.clone(),
                    namespace,
                });
            }
        }
        for mut planned in planned {
            let api: Api<DynamicObject> = match &planned.namespace {
                Some(namespace) => {
                    let namespace = if existing.contains(namespace) {
                        namespace
                    } else {
                        &temporary[namespace]
                    };
                    planned.object["metadata"]["namespace"] = namespace.as_str().into();
                    Api::namespaced_with(client.clone(), namespace, &planned.resource)
                }
                None => Api::all_with(client.clone(), &planned.resource),
            };
            let subject = format!(
                "{} {}{}",
                planned.label,
                describe(&planned.object),
                planned.namespace.as_deref().map_or_default(|namespace| format!(" in {namespace}"))
            );
            // Always check the policy from the deployment, even when the
            // existing namespace still has different labels.
            if let Some(namespace) = &planned.namespace
                && restricted.contains_key(namespace.as_str())
                && let Some(pod) = admission_pod(&planned.object)
            {
                let api = Api::<Pod>::namespaced(client.clone(), &temporary[namespace]);
                let params = PostParams { dry_run: true, ..Default::default() };
                let result = api.create(&params, &serde_json::from_value(pod)?).await;
                match result {
                    Ok(_) => tally.check("restricted pod", true, &subject),
                    Err(error) => tally.check("restricted pod", false, format!("{subject}: {error}")),
                }
            }
            let result = if let Some(name) = planned.object["metadata"]["name"].as_str() {
                let params = PatchParams::apply(FIELD_MANAGER).force().dry_run();
                api.patch(name, &params, &Patch::Apply(&planned.object)).await.map(drop)
            } else {
                let params = PostParams {
                    dry_run: true,
                    field_manager: Some(FIELD_MANAGER.into()),
                };
                api.create(&params, &serde_json::from_value(planned.object)?).await.map(drop)
            };
            match result {
                Ok(()) => tally.check("dry-run", true, subject),
                Err(error) => tally.check("dry-run", false, format!("{subject}: {error}")),
            }
        }
        anyhow::Ok(())
    }
    .await;
    for stand_in in temporary.values() {
        if let Err(error) = namespaces.delete(stand_in, &DeleteParams::default()).await {
            tally.check("cleanup", false, format!("Namespace/{stand_in}: {error}"));
        }
    }
    for name in &installed {
        let deleted = async {
            definitions.delete(name, &DeleteParams::default()).await?;
            wamn_kubernetes::client::wait_named(&definitions, name, DEADLINE, |object| {
                Ok(object.is_none().then_some(()))
            })
            .await
        }
        .await;
        tally.check(
            "cleanup",
            deleted.is_ok(),
            format!("CustomResourceDefinition/{name}"),
        );
    }
    result
}

/// Apply each rendered CRD that is absent and wait until it is Established.
async fn install_crds(
    definitions: &Api<CustomResourceDefinition>,
    rendered: &[Rendered],
    installed: &mut Vec<String>,
    tally: &mut Tally,
) -> anyhow::Result<()> {
    let params = PatchParams::apply(CRD_MANAGER).force();
    for chart in rendered {
        for object in chart
            .objects
            .iter()
            .filter(|object| object["kind"] == "CustomResourceDefinition")
        {
            let name = object["metadata"]["name"]
                .as_str()
                .context("CRD has no name")?;
            // Never delete a CRD that existed before this run.
            if definitions.get_opt(name).await?.is_some() {
                println!("-- CustomResourceDefinition/{name} exists and is kept");
                continue;
            }
            let applied = definitions
                .patch(name, &params, &Patch::Apply(object))
                .await;
            if applied.is_ok() {
                installed.push(name.to_owned());
            }
            tally.check(
                "crds",
                applied.is_ok(),
                format!("{} CustomResourceDefinition/{name}", chart.label),
            );
        }
    }
    for name in installed.iter() {
        let established =
            wamn_kubernetes::client::wait_named(definitions, name, DEADLINE, |object| {
                Ok(object
                    .and_then(|definition| definition.status)
                    .and_then(|status| status.conditions)
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition.type_ == "Established" && condition.status == "True"
                        })
                    })
                    .then_some(()))
            })
            .await;
        tally.check(
            "crds",
            established.is_ok(),
            format!("CustomResourceDefinition/{name} Established"),
        );
    }
    Ok(())
}

/// Print the cluster requests without sending them. This assumes a cluster
/// without the platform and its namespaces.
fn print_cluster(rendered: &[Rendered], tally: &mut Tally) {
    println!("-- would list the Helm release Secrets of {INFRASTRUCTURE_RELEASE} in {PLATFORM}");
    for chart in rendered {
        for object in chart
            .objects
            .iter()
            .filter(|object| object["kind"] == "CustomResourceDefinition")
        {
            tally.note(
                "crds",
                "would apply",
                format!(
                    "{} {} (server side, manager {CRD_MANAGER}) and wait for Established",
                    chart.label,
                    describe(object)
                ),
            );
        }
    }
    let mut namespaces = BTreeSet::new();
    let restricted = restricted_namespaces(rendered);
    for chart in rendered {
        for object in &chart.objects {
            let namespace = object["metadata"]["namespace"]
                .as_str()
                .unwrap_or(&chart.namespace);
            namespaces.insert(namespace);
            let action = if object["metadata"]["name"].is_string() {
                "would dry-run apply"
            } else {
                "would dry-run create"
            };
            tally.note(
                "dry-run",
                action,
                format!(
                    "{} {} in {namespace} if namespaced (manager {FIELD_MANAGER}, force conflicts)",
                    chart.label,
                    describe(object)
                ),
            );
            if restricted.contains_key(namespace) && admission_pod(object).is_some() {
                tally.note(
                    "restricted pod",
                    "would dry-run create",
                    format!(
                        "{} {} in the stand-in for {namespace}",
                        chart.label,
                        describe(object)
                    ),
                );
            }
        }
    }
    for namespace in namespaces {
        if restricted.contains_key(namespace) {
            println!(
                "-- would create a restricted stand-in and pod dry-run account for {namespace}, then delete the namespace"
            );
        } else {
            println!(
                "-- would create a temporary namespace for {namespace} if it is absent, then delete it"
            );
        }
    }
    println!("-- would delete the CRDs that this run installed and wait until they are gone");
}

fn describe(object: &Value) -> String {
    let metadata = &object["metadata"];
    let name = metadata["name"].as_str().map_or_else(
        || format!("{}*", metadata["generateName"].as_str().unwrap_or_default()),
        str::to_owned,
    );
    format!("{}/{name}", object["kind"].as_str().unwrap_or_default())
}

fn find<'a>(objects: &'a [Value], kind: &str, name: &str) -> anyhow::Result<&'a Value> {
    objects
        .iter()
        .find(|object| object["kind"] == kind && object["metadata"]["name"] == name)
        .with_context(|| format!("the render has no {kind}/{name}"))
}

fn write_json(directory: &Path, file: &str, value: &Value) -> anyhow::Result<PathBuf> {
    let path = directory.join(file);
    std::fs::write(&path, serde_json::to_vec_pretty(value)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PREFLIGHT_ACCOUNT, admission_pod, docker_user};

    #[test]
    fn docker_run_uses_container_identity_before_pod_defaults() {
        let pod = json!({"securityContext": {"runAsUser": 65532, "runAsGroup": 65532}});
        let container = json!({"securityContext": {"runAsGroup": 2000}});
        assert_eq!(docker_user(&pod, &container).as_deref(), Some("65532:2000"));
        assert_eq!(
            docker_user(&json!({}), &container).as_deref(),
            Some("0:2000")
        );
    }

    #[test]
    fn restricted_pod_dry_run_skips_poolers_and_checks_deployments() {
        let pooler = json!({
            "apiVersion": "postgresql.cnpg.io/v1",
            "kind": "Pooler",
            "spec": {
                "template": {
                    "spec": {
                        "containers": [{
                            "name": "pgbouncer",
                            "resources": {"requests": {"cpu": "100m"}},
                        }],
                    },
                },
            },
        });
        let deployment = json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "spec": {
                "template": {
                    "spec": {
                        "containers": [{"name": "server", "image": "example.invalid/server"}],
                    },
                },
            },
        });
        assert!(admission_pod(&pooler).is_none());
        assert!(admission_pod(&deployment).is_some());
    }

    #[test]
    fn a_stateful_set_dry_run_pod_preserves_security_fields_and_mounts_its_claims() {
        let context = json!({"runAsNonRoot": true, "seccompProfile": {"type": "RuntimeDefault"}});
        let containers = json!([{
            "name": "server",
            "image": "example.invalid/server",
            "volumeMounts": [{"name": "data", "mountPath": "/data"}],
            "securityContext": {"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}},
        }]);
        let annotations =
            json!({"container.apparmor.security.beta.kubernetes.io/server": "runtime/default"});
        let object = json!({
            "kind": "StatefulSet",
            "spec": {
                "template": {
                    "metadata": {"annotations": annotations},
                    "spec": {
                        "securityContext": context,
                        "containers": containers,
                        "serviceAccountName": "server",
                    },
                },
                "volumeClaimTemplates": [{"metadata": {"name": "data"}}],
            },
        });
        let pod = admission_pod(&object).expect("StatefulSet Pod");
        assert_eq!(pod["kind"], "Pod");
        assert_eq!(pod["metadata"]["annotations"], annotations);
        assert_eq!(pod["spec"]["securityContext"], context);
        assert_eq!(pod["spec"]["containers"], containers);
        assert_eq!(pod["spec"]["serviceAccountName"], PREFLIGHT_ACCOUNT);
        let mount = &pod["spec"]["containers"][0]["volumeMounts"][0]["name"];
        let volume = pod["spec"]["volumes"]
            .as_array()
            .expect("Pod volumes")
            .iter()
            .find(|volume| volume["name"] == *mount)
            .expect("volume for the StatefulSet claim");
        assert_eq!(volume["persistentVolumeClaim"]["claimName"], *mount);
    }
}
