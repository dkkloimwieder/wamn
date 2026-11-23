//! Journey harness: each test gets its own org and environment through product commands.
//!
//! `cargo xtask journey-cluster` writes the outputs file that [`Outputs`] reads.
//! At most [`CAPACITY`] tests hold an org at one time. Observers use the read-only
//! login that journey-cluster creates on the system Cluster.
#![allow(
    dead_code,
    reason = "each journey target uses a part of the shared harness"
)]

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{ConfigMap, Pod, PodTemplateSpec, Secret, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{
    ApiResource, AttachParams, DeleteParams, DynamicObject, GroupVersionKind, ListParams,
    LogParams, Patch, PatchParams, PostParams,
};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::ConnectOptions as _;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use wamn_http_client::HttpClient;
use wamn_identity_protocol::PrincipalId;
use wamn_package::local_operation::LocalOperation;
use wamn_registry::deployment::DeploymentStore as _;
use wamn_registry::{OrgId, Triple, name};
use wamn_test_infrastructure::scratch::ScratchRoot;

pub mod host;
pub mod request;
mod routes;
pub use routes::Routes;

/// The number of tests that hold an org and environment at one time (Epic 15 ruling C9).
const CAPACITY: usize = 4;
const PLATFORM: &str = "platform";
const SYSTEM_CLUSTER: &str = "wamn-pg";
const SYSTEM_DATABASE: &str = "wamn_system";
const ENV: &str = "dev";
const PROJECT: &str = "fixture";
const LABEL: &str = "fixture";
/// The role of the API service that calls the fixture routes.
const CALLER_ROLE: &str = "route-caller";
/// How long CloudNativePG takes to apply the observer's pg_hba line.
const OBSERVER_DEADLINE: Duration = Duration::from_secs(120);
/// How long foreground deletion can wait for the probe pod to terminate.
const PROBE_CLEANUP_DEADLINE: Duration = Duration::from_secs(60);
/// The field manager of the observer's pg_hba patch.
const FIELD_MANAGER: &str = "wamn-journey-observer";

static RUNNING: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

/// The journey-cluster outputs. Field names match `xtask/src/journey_cluster.rs::Outputs`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outputs {
    pub kubeconfig: PathBuf,
    pub context: String,
    pub bundle: PathBuf,
    pub wamn: PathBuf,
    pub registry_auth_file: PathBuf,
    pub package_artifact_base: String,
    pub component_artifact_base: String,
    pub web_address: SocketAddr,
    pub base_domain: String,
    pub observer_role: String,
    pub observer_password: String,
}

/// The installed kind platform and a Kubernetes client on its recorded context.
pub struct Cluster {
    pub outputs: Outputs,
    pub client: kube::Client,
}

impl Cluster {
    /// Read the outputs file. A missing file or entry fails.
    pub async fn load() -> anyhow::Result<Self> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/journey/cluster.json");
        let outputs: Outputs =
            serde_json::from_slice(&std::fs::read(&path).with_context(|| {
                format!("read {}; run cargo xtask journey-cluster", path.display())
            })?)
            .with_context(|| format!("decode {}", path.display()))?;
        for file in [
            &outputs.kubeconfig,
            &outputs.bundle,
            &outputs.wamn,
            &outputs.registry_auth_file,
        ] {
            ensure!(
                file.exists(),
                "journey-cluster output {} is absent",
                file.display()
            );
        }
        let config = kube::Config::from_custom_kubeconfig(
            Kubeconfig::read_from(&outputs.kubeconfig)?,
            &KubeConfigOptions {
                context: Some(outputs.context.clone()),
                ..KubeConfigOptions::default()
            },
        )
        .await?;
        let client = kube::Client::try_from(config)?;
        Ok(Self { outputs, client })
    }

    /// A `wamn` child with the cluster inputs and no control session.
    fn command(&self) -> std::process::Command {
        let outputs = &self.outputs;
        let mut command = std::process::Command::new(&outputs.wamn);
        command
            .env("KUBECONFIG", &outputs.kubeconfig)
            .env("WAMN_REGISTRY_AUTH_FILE", &outputs.registry_auth_file)
            .env("WAMN_PACKAGE_ARTIFACT_BASE", &outputs.package_artifact_base)
            .env(
                "WAMN_COMPONENT_ARTIFACT_BASE",
                &outputs.component_artifact_base,
            )
            .env_remove("WAMN_CONTROL_URL")
            .env_remove("WAMN_TOKEN")
            .env_remove("WAMN_RELEASE_CHART");
        command
    }

    /// Connect to the system database with the read-only observer login.
    pub async fn observe_system(&self) -> anyhow::Result<Observed> {
        let pods = Api::<Pod>::namespaced(self.client.clone(), PLATFORM);
        let primary = pods
            .list(&ListParams::default().labels(&format!(
                "cnpg.io/cluster={SYSTEM_CLUSTER},cnpg.io/instanceRole=primary"
            )))
            .await?
            .items
            .into_iter()
            .find_map(|pod| pod.metadata.name)
            .context("the system Cluster has no primary pod")?;
        let ca = secret_key(
            &self.client,
            PLATFORM,
            &format!("{SYSTEM_CLUSTER}-ca"),
            "ca.crt",
        )
        .await?;
        let outputs = &self.outputs;
        self.observe(
            PLATFORM,
            &primary,
            SYSTEM_DATABASE,
            &ca,
            &outputs.observer_role,
            &outputs.observer_password,
        )
        .await
    }

    /// Connect to `database` on PostgreSQL port 5432 of `pod`, through a port-forward, as `role`.
    pub async fn observe(
        &self,
        namespace: &str,
        pod: &str,
        database: &str,
        server_ca: &[u8],
        role: &str,
        password: &str,
    ) -> anyhow::Result<Observed> {
        let forward =
            PortForward::start(Api::namespaced(self.client.clone(), namespace), pod, 5432).await?;
        let connection = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(forward.address.port())
            .ssl_mode(PgSslMode::VerifyCa)
            .ssl_root_cert_from_pem(server_ca.to_vec())
            .username(role)
            .password(password)
            .database(database)
            .connect()
            .await
            .with_context(|| format!("connect to {database} on {namespace}/{pod} as {role}"))?;
        Ok(Observed {
            connection,
            _forward: forward,
        })
    }
}

/// A read-only database session. The port-forward closes with it.
pub struct Observed {
    pub connection: sqlx::PgConnection,
    _forward: PortForward,
}

/// One org with its owner session, made for one test.
pub struct Org {
    pub cluster: Cluster,
    pub name: OrgId,
    pub owner: PrincipalId,
    pub token: String,
    _capacity: Capacity,
}

impl Org {
    /// Run `wamn org create` with a unique org and owner email, and open the owner session.
    pub async fn create() -> anyhow::Result<Self> {
        let capacity = Capacity::acquire();
        let cluster = Cluster::load().await?;
        let name: OrgId =
            format!("j{}", &uuid::Uuid::new_v4().simple().to_string()[..10]).parse()?;
        let email = format!("owner-{name}@example.com");
        let mut command = cluster.command();
        command.args(["org", "create", name.as_str(), "--owner", &email]);
        run(command).await?;
        let (owner, invitation) = invitation(&cluster.client, &email).await?;
        let token = session(&cluster.client, &name, &email, &owner, &invitation).await?;
        Ok(Self {
            cluster,
            name,
            owner,
            token,
            _capacity: capacity,
        })
    }

    /// The org's control address through web.
    pub fn control_url(&self) -> String {
        format!("http://{}.{}", self.name, self.cluster.outputs.base_domain)
    }

    /// A `wamn` child with the owner session.
    pub fn command(&self) -> std::process::Command {
        let mut command = self.cluster.command();
        command
            .env("WAMN_CONTROL_URL", self.control_url())
            .env("WAMN_TOKEN", &self.token);
        command
    }

    /// Write an environment file with one project and one application, and run `wamn env apply`.
    pub async fn apply(self, application: &str) -> anyhow::Result<Environment> {
        let scratch = ScratchRoot::create()?;
        let file = scratch.path().join("environment.yaml");
        // A JSON document is a YAML document.
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "org": self.name,
                "env": ENV,
                "target": "cloud",
                "revocation_seconds": 60,
                "database": {"instances": 1, "storage": "1Gi", "cpu": "100m", "memory": "256Mi"},
                "projects": [{"name": PROJECT, "applications": {application: {"label": LABEL}}}],
            }))?,
        )?;
        let triple = Triple::new(self.name.clone(), ENV.parse()?, PROJECT.parse()?);
        // The guard exists first, so a failed apply is also deleted.
        let environment = Environment {
            org: self,
            file,
            triple,
            scratch,
        };
        let mut command = environment.org.command();
        command.args(["env", "apply"]).arg(&environment.file);
        run(command).await?;
        Ok(environment)
    }
}

/// One applied environment. Drop runs a plain `wamn env delete`, also after a failure.
pub struct Environment {
    pub org: Org,
    pub file: PathBuf,
    pub triple: Triple,
    scratch: ScratchRoot,
}

impl Environment {
    /// Admit the observer login to the project database, and connect with it.
    ///
    /// Each `wamn env apply` and `wamn deploy` upgrades the project chart, which puts its
    /// pg_hba list back, so the admission runs again on each call.
    pub async fn observe(&self) -> anyhow::Result<Observed> {
        admit_observer(&self.org.cluster, &self.triple).await
    }

    /// Run `wamn deploy` for the application's source package.
    pub async fn deploy(&self, package: &Path, application: &str) -> anyhow::Result<()> {
        let mut command = self.org.command();
        command
            .arg("deploy")
            .arg(package)
            .args(["--application", application, "--env"])
            .arg(&self.file);
        run(command).await.map(drop)
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        let mut command = self.org.command();
        command.args(["env", "delete", &self.triple.to_string()]);
        match command.output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "wamn env delete {} failed: {}",
                self.triple,
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => eprintln!("wamn env delete {}: {error}", self.triple),
        }
    }
}

/// A deployed fixture application and routes that an admin API service calls.
pub struct Application {
    pub routes: Routes,
    caller: PrincipalId,
    pub environment: Environment,
}

impl Application {
    /// Create an org and environment, and deploy the source package at `source`.
    pub async fn prepare(source: &Path) -> anyhow::Result<Self> {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("fixture directory name")?;
        let environment = Org::create().await?.apply(name).await?;
        environment.deploy(source, name).await?;
        let org = &environment.org;
        let outputs = &org.cluster.outputs;
        let host =
            wamn_registry::name::application_host(LABEL, &org.name, &outputs.base_domain.parse()?);
        // The fixture routes accept a PAT only, and an API service cannot hold `admin`.
        caller_role(&environment, source, name, &host).await?;
        let secret = environment.scratch.path().join("caller.json");
        let triple = &environment.triple;
        let mut command = org.command();
        command
            .args(["create-api-service", "--project", triple.project.as_str()])
            .args(["--env", triple.env.as_str(), "--name", triple.env.as_str()])
            .args(["--display-name", "Route fixture", "--role", CALLER_ROLE])
            .args(["--lifetime-seconds", "900", "--emit-secret"])
            .arg(&secret);
        run(command).await?;
        let mut service: Value = serde_json::from_slice(&std::fs::read(&secret)?)?;
        std::fs::remove_file(&secret)?;
        let bearer = match service["token"].take() {
            Value::String(token) => token,
            token => bail!("the API service secret has no token: {token}"),
        };
        let caller = service["principal_id"]
            .as_str()
            .context("the API service secret has no principal")?
            .parse()?;
        let routes = Routes {
            endpoint: format!("http://{}/api", outputs.web_address),
            host,
            bearer,
            client: HttpClient::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        };
        Ok(Self {
            routes,
            caller,
            environment,
        })
    }

    /// The API service principal that the routes' bearer token names.
    pub fn caller(&self) -> &PrincipalId {
        &self.caller
    }
}

/// Create the API caller's role on the application host, and grant it each operation that
/// the fixture's build output serves, its bases' operations included.
async fn caller_role(
    environment: &Environment,
    source: &Path,
    application: &str,
    host: &str,
) -> anyhow::Result<()> {
    // `wamn deploy` builds the package into `<workspace>/target/wamn/<package>`.
    let manifest = std::fs::read(
        source
            .parent()
            .context("fixture workspace")?
            .join("target/wamn")
            .join(application)
            .join("wamn.json"),
    )?;
    let owners = wamn_package::manifest::OperationOwners::from_slice(&manifest)?;
    let document: Value = serde_json::from_slice(&manifest)?;
    let reference = |package, version, local: &LocalOperation| {
        let token = wamn_package::operation_token(package, version, local)?;
        anyhow::Ok(wamn_package::sealed_operation_reference(token.as_str()).to_owned())
    };
    let mut operations = BTreeSet::new();
    for local in document["compiled_operations"]
        .as_object()
        .context("the fixture build output has no compiled_operations")?
        .keys()
    {
        operations.insert(reference(
            &owners.package.id,
            &owners.package.version,
            &local.parse()?,
        )?);
    }
    for base in owners.base_dependencies.values() {
        for local in &base.operations {
            operations.insert(reference(&base.package, &base.version, local)?);
        }
    }
    let host_url = format!("http://{host}");
    let mut command = environment.org.command();
    command
        .args(["create-role", "--host-url", &host_url])
        .args(["--role", CALLER_ROLE]);
    run(command).await?;
    for operation in &operations {
        let mut command = environment.org.command();
        command
            .args(["grant-permission", "--host-url", &host_url])
            .args(["--role", CALLER_ROLE, "--operation", operation]);
        run(command).await?;
    }
    Ok(())
}

fn postgres_clusters(client: &kube::Client, namespace: &str) -> Api<DynamicObject> {
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("postgresql.cnpg.io", "v1", "Cluster"),
        "clusters",
    );
    Api::namespaced_with(client.clone(), namespace, &resource)
}

/// Resolve the active project Cluster from the registry generation (C2, C3, C6).
pub async fn active_project_cluster(cluster: &Cluster, triple: &Triple) -> anyhow::Result<String> {
    let mut system = cluster.observe_system().await?;
    let instance = wamn_postgres::Registry::new(&mut system.connection)
        .instance_suffix(triple)
        .await
        .with_context(|| format!("read the active deployment for {triple}"))?
        .with_context(|| format!("{triple} has no active deployment"))?;
    let database =
        name::project_env_database_name(&triple.org, &triple.project, &triple.env, &instance);
    Ok(name::project_cluster(&database))
}

/// Admit the read-only observer to the active project Cluster (C2, C3).
pub async fn admit_observer(cluster: &Cluster, triple: &Triple) -> anyhow::Result<Observed> {
    let name = active_project_cluster(cluster, triple).await?;
    observe_project_cluster(cluster, triple.org.as_str(), &name).await
}

/// Admit the read-only observer to a named project Cluster kept after deletion (C2).
pub async fn observe_project_cluster(
    cluster: &Cluster,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Observed> {
    let client = &cluster.client;
    let clusters = postgres_clusters(client, namespace);
    let mut project = clusters
        .get(name)
        .await
        .with_context(|| format!("read Cluster {namespace}/{name}"))?;
    let spec = &mut project.data["spec"];
    let database = spec["bootstrap"]["initdb"]["database"]
        .as_str()
        .context("the project Cluster has no initdb database")?
        .to_owned();
    let mut rules: Vec<String> = serde_json::from_value(spec["postgresql"]["pg_hba"].take())?;
    let outputs = &cluster.outputs;
    let role = &outputs.observer_role;
    let observer_rule = format!("hostssl {database} {role} 127.0.0.1/32 scram-sha-256");
    if !rules.contains(&observer_rule) {
        let at = rules
            .iter()
            .position(|rule| rule == "host all all all reject")
            .unwrap_or(rules.len());
        rules.insert(at, observer_rule);
        let params = PatchParams {
            field_manager: Some(FIELD_MANAGER.to_owned()),
            ..PatchParams::default()
        };
        let patched = clusters
            .patch(
                name,
                &params,
                &Patch::Merge(json!({"spec": {"postgresql": {"pg_hba": rules}}})),
            )
            .await?;
        // Helm applies the project chart server-side without forcing conflicts. Without
        // this manager's entry, pg_hba has no owner, and the next apply takes it back.
        let kept: Vec<_> = patched
            .metadata
            .managed_fields
            .into_iter()
            .flatten()
            .filter(|entry| entry.manager.as_deref() != Some(FIELD_MANAGER))
            .collect();
        clusters
            .patch(
                name,
                &params,
                &Patch::Merge(json!({"metadata": {
                    "managedFields": kept,
                    "resourceVersion": patched.metadata.resource_version,
                }})),
            )
            .await?;
    }
    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let primary = pods
        .list(&ListParams::default().labels(&format!(
            "cnpg.io/cluster={name},cnpg.io/instanceRole=primary"
        )))
        .await?
        .items
        .into_iter()
        .find_map(|pod| pod.metadata.name)
        .with_context(|| format!("the project Cluster {name} has no primary pod"))?;
    // The names come from the chart and journey-cluster; the password is URL-safe base64.
    let password = &outputs.observer_password;
    let sql = format!(
        r#"DO $$ BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN
               CREATE ROLE "{role}";
             END IF;
           END $$;
           ALTER ROLE "{role}" LOGIN BYPASSRLS PASSWORD '{password}';
           ALTER ROLE "{role}" SET default_transaction_read_only = on;
           GRANT pg_read_all_data, pg_read_all_stats TO "{role}";
           GRANT CONNECT ON DATABASE "{database}" TO "{role}";"#
    );
    exec(
        &pods,
        &primary,
        &AttachParams::default().container("postgres"),
        [
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-d",
            "postgres",
            "-c",
            &sql,
        ],
    )
    .await
    .context("create the observer role in the project Cluster")?;
    let server_ca = secret_key(client, namespace, &format!("{name}-ca"), "ca.crt").await?;
    // CloudNativePG reloads pg_hba after the patch; the first accepted login is the event.
    let deadline = Instant::now() + OBSERVER_DEADLINE;
    loop {
        let observed = cluster
            .observe(namespace, &primary, &database, &server_ca, role, password)
            .await;
        match observed {
            Ok(observed) => return Ok(observed),
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("the project Cluster never admitted the observer"));
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

/// Read the digest-pinned PostgreSQL image from the project Cluster used by the observer (C6).
pub async fn probe_image(
    client: &kube::Client,
    namespace: &str,
    cluster: &str,
) -> anyhow::Result<String> {
    let project = postgres_clusters(client, namespace)
        .get(cluster)
        .await
        .with_context(|| format!("read Cluster {namespace}/{cluster}"))?;
    project.data["spec"]["imageName"]
        .as_str()
        .filter(|image| {
            image
                .split_once("@sha256:")
                .is_some_and(|(reference, digest)| {
                    !reference.is_empty()
                        && digest.len() == 64
                        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        })
        .map(str::to_owned)
        .with_context(|| format!("Cluster {namespace}/{cluster} has no digest-pinned imageName"))
}

/// Read one Secret key as bytes for Pooler credentials or a certificate (C7, C8, C11).
pub async fn secret_key(
    client: &kube::Client,
    namespace: &str,
    secret: &str,
    key: &str,
) -> anyhow::Result<Vec<u8>> {
    Api::<Secret>::namespaced(client.clone(), namespace)
        .get(secret)
        .await
        .with_context(|| format!("read key {key} from Secret {namespace}/{secret}"))?
        .data
        .and_then(|mut data| data.remove(key))
        .map(|bytes| bytes.0)
        .with_context(|| format!("Secret {namespace}/{secret} has no key {key}"))
}

/// Scale a Deployment such as `wamn_registry::name::control_resource` and wait for readiness (C7).
pub async fn scale_deployment(
    client: &kube::Client,
    namespace: &str,
    name: &str,
    replicas: i32,
    deadline: Duration,
) -> anyhow::Result<()> {
    let deployments = Api::<Deployment>::namespaced(client.clone(), namespace);
    let waiting = format!("Deployment {namespace}/{name} reaches {replicas} ready replicas");
    tokio::time::timeout(deadline, async {
        deployments
            .patch_scale(
                name,
                &PatchParams::default(),
                &Patch::Merge(json!({"spec": {"replicas": replicas}})),
            )
            .await
            .with_context(|| format!("scale Deployment {namespace}/{name} to {replicas}"))?;
        request::until(deadline, &waiting, || async {
            let deployment = deployments
                .get(name)
                .await
                .with_context(|| format!("read Deployment {namespace}/{name}"))?;
            Ok(deployment.status.and_then(|status| {
                (status.observed_generation == deployment.metadata.generation
                    && deployment.spec.and_then(|spec| spec.replicas) == Some(replicas)
                    && status.ready_replicas.unwrap_or(0) == replicas
                    && (replicas != 0
                        || (status.replicas.unwrap_or(0) == 0
                            && status.terminating_replicas.unwrap_or(0) == 0)))
                    .then_some(())
            }))
        })
        .await
    })
    .await
    .with_context(|| format!("{waiting} within {deadline:?}"))?
}

/// Run a one-container Job once, collect its exit code and logs, and wait for deletion (C6).
pub async fn run_job(
    client: &kube::Client,
    namespace: &str,
    mut template: PodTemplateSpec,
    deadline: Duration,
) -> anyhow::Result<(i32, String)> {
    ensure!(
        template
            .metadata
            .as_ref()
            .and_then(|meta| meta.labels.as_ref())
            .is_some_and(|labels| !labels.is_empty()),
        "the probe pod needs labels"
    );
    let spec = template.spec.as_mut().context("probe pod spec is absent")?;
    let [container] = spec.containers.as_slice() else {
        bail!("the probe pod must have one container");
    };
    ensure!(
        container
            .image
            .as_ref()
            .is_some_and(|image| !image.is_empty())
            && container
                .command
                .as_ref()
                .is_some_and(|command| !command.is_empty()),
        "the probe container needs an image and command"
    );
    spec.restart_policy = Some("Never".to_owned());
    let jobs = Api::<Job>::namespaced(client.clone(), namespace);
    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let job = Job {
        metadata: ObjectMeta {
            name: Some(format!("journey-probe-{}", uuid::Uuid::new_v4().simple())),
            ..ObjectMeta::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            template,
            ..JobSpec::default()
        }),
        ..Job::default()
    };
    let name = job.metadata.name.as_deref().context("the probe Job name")?;
    let waiting = format!("Job {namespace}/{name} finishes");
    let result = tokio::time::timeout(deadline, async {
        let created = jobs.create(&PostParams::default(), &job).await?;
        let uid = created.metadata.uid.context("the probe Job has no UID")?;
        request::until(deadline, &waiting, || async {
            let job = jobs.get(name).await?;
            Ok(job
                .status
                .is_some_and(|status| {
                    status.conditions.into_iter().flatten().any(|condition| {
                        matches!(condition.type_.as_str(), "Complete" | "Failed")
                            && condition.status == "True"
                    })
                })
                .then_some(()))
        })
        .await?;
        // The Job controller sets the pod's controller reference when it creates the pod.
        let selector =
            ListParams::default().labels(&format!("batch.kubernetes.io/controller-uid={uid}"));
        let [pod] = <[_; 1]>::try_from(pods.list(&selector).await?.items).map_err(|found| {
            anyhow::anyhow!("Job {name} has {} pods, expected one", found.len())
        })?;
        let pod_name = pod.metadata.name.context("the probe pod has no name")?;
        let statuses = pod
            .status
            .and_then(|status| status.container_statuses)
            .with_context(|| format!("probe pod {pod_name} has no container status"))?;
        let [status] = <[_; 1]>::try_from(statuses).map_err(|found| {
            anyhow::anyhow!(
                "probe pod {pod_name} has {} container statuses, expected one",
                found.len()
            )
        })?;
        let exit = status
            .state
            .and_then(|state| state.terminated)
            .with_context(|| format!("probe pod {pod_name} has no exit status"))?
            .exit_code;
        let logs = pods
            .logs(
                &pod_name,
                &LogParams {
                    container: Some(status.name),
                    ..LogParams::default()
                },
            )
            .await?;
        anyhow::Ok((exit, logs))
    })
    .await
    .with_context(|| format!("{waiting} within {deadline:?}"))
    .flatten()
    .with_context(|| format!("run probe Job {namespace}/{name}"));

    let removing = format!("Job {namespace}/{name} is deleted");
    let cleanup = tokio::time::timeout(PROBE_CLEANUP_DEADLINE, async {
        match jobs.delete(name, &DeleteParams::foreground()).await {
            Ok(_) => {}
            Err(kube::Error::Api(error)) if error.code == 404 => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        request::until(PROBE_CLEANUP_DEADLINE, &removing, || async {
            Ok(jobs.get_opt(name).await?.is_none().then_some(()))
        })
        .await
    })
    .await
    .with_context(|| format!("{removing} within {PROBE_CLEANUP_DEADLINE:?}"))
    .flatten()
    .with_context(|| format!("delete probe Job {namespace}/{name}"));

    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok((exit, logs)), Err(error)) => {
            Err(error.context(format!("probe exited {exit}: {logs}")))
        }
        (Err(run_error), Err(error)) => Err(run_error.context(format!("{error:#}"))),
    }
}

/// Run a pod command such as `SHOW CONFIG` (C9), returning stdout on success.
pub(crate) async fn exec<const N: usize>(
    pods: &Api<Pod>,
    pod: &str,
    params: &AttachParams,
    command: [&str; N],
) -> anyhow::Result<String> {
    let mut process = pods.exec(pod, command, params).await?;
    let mut stdout = process.stdout().context("command output")?;
    let mut stderr = process.stderr().context("command diagnostics")?;
    let status = process.take_status().context("command status")?;
    let (mut output, mut diagnostics) = (String::new(), String::new());
    tokio::try_join!(
        stdout.read_to_string(&mut output),
        stderr.read_to_string(&mut diagnostics),
    )?;
    process.join().await?;
    let status = status.await.context("the command has no exit status")?;
    ensure!(
        status.status.as_deref() == Some("Success"),
        "{command:?} in {pod} failed: {} {diagnostics}",
        status.message.unwrap_or_default()
    );
    Ok(output)
}

/// Run one `wamn` child and return its output.
async fn run(command: std::process::Command) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::from(command);
    let output = command.kill_on_drop(true).output().await?;
    ensure!(
        output.status.success(),
        "{:?} failed: {}{}",
        command.as_std().get_args().collect::<Vec<_>>(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// The identity pods that the identity Service selects.
async fn identity_pods(client: &kube::Client) -> anyhow::Result<(Service, Vec<Pod>)> {
    let service = Api::<Service>::namespaced(client.clone(), PLATFORM)
        .get("identity")
        .await?;
    let selector = service
        .spec
        .as_ref()
        .and_then(|spec| spec.selector.as_ref())
        .context("the identity Service has no selector")?
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    let pods = Api::<Pod>::namespaced(client.clone(), PLATFORM)
        .list(&ListParams::default().labels(&selector))
        .await?
        .items;
    Ok((service, pods))
}

/// Read the owner's invitation from the identity pods' local outbox.
async fn invitation(client: &kube::Client, email: &str) -> anyhow::Result<(PrincipalId, String)> {
    let api = Api::<Pod>::namespaced(client.clone(), PLATFORM);
    let (_, pods) = identity_pods(client).await?;
    for pod in pods {
        let name = pod.metadata.name.context("identity pod name")?;
        let mount = pod
            .spec
            .iter()
            .flat_map(|spec| &spec.containers)
            .flat_map(|container| container.volume_mounts.iter().flatten())
            .find(|mount| mount.name == "local-mail")
            .map(|mount| mount.mount_path.clone())
            .context("the identity pod has no local mail volume")?;
        let outbox = exec(
            &api,
            &name,
            &AttachParams::default(),
            ["sh", "-c", r#"cat "$1"/*"#, "sh", &mount],
        )
        .await?;
        for line in outbox.lines() {
            let mail: Value = serde_json::from_str(line)?;
            if mail["to"] != email {
                continue;
            }
            let (principal, secret) = mail["text"]
                .as_str()
                .and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("Invitation code: "))
                })
                .and_then(|code| code.split_once(':'))
                .context("the invitation mail has no invitation code")?;
            return Ok((principal.parse()?, secret.to_owned()));
        }
    }
    bail!("no identity pod holds an invitation for {email}")
}

/// Enroll the owner's password and open a session for the org audience.
async fn session(
    client: &kube::Client,
    org: &OrgId,
    email: &str,
    owner: &PrincipalId,
    invitation: &str,
) -> anyhow::Result<String> {
    let (service, pods) = identity_pods(client).await?;
    let pod = pods
        .into_iter()
        .find(|pod| {
            pod.status
                .as_ref()
                .and_then(|status| status.conditions.as_ref())
                .is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                })
        })
        .and_then(|pod| pod.metadata.name)
        .context("no identity pod is ready")?;
    let ca = Api::<ConfigMap>::namespaced(client.clone(), "hosts")
        .get("identity-ca")
        .await?
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .context("the identity CA has no ca.crt")?;
    let forward = PortForward::start(Api::namespaced(client.clone(), PLATFORM), &pod, 8443).await?;
    // The serving certificate names the Service address.
    let host = format!(
        "{}.{PLATFORM}.svc.cluster.local",
        service.metadata.name.context("identity Service name")?
    );
    let identity = HttpClient::builder()
        .roots(ca.as_bytes())?
        .resolve(&host, forward.address)
        .timeout(Duration::from_secs(60))
        .build()?;
    let base = format!("https://{host}:{}", forward.address.port());
    let password = uuid::Uuid::new_v4().simple().to_string();
    let enrolled = identity
        .post(format!("{base}/password/enroll"))
        .json(&json!({"principal_id": owner, "invitation": invitation, "password": password}))
        .send()
        .await?;
    let status = enrolled.status();
    ensure!(
        status == hyper::StatusCode::NO_CONTENT,
        "enroll answered {status}: {}",
        enrolled.text().await?
    );
    let session = identity
        .post(format!("{base}/password/session"))
        .json(&json!({"email": email, "password": password, "aud": format!("urn:wamn:org:{org}")}))
        .send()
        .await?;
    let status = session.status();
    let body: Value = session.json().await?;
    ensure!(
        status == hyper::StatusCode::OK,
        "session answered {status}: {body}"
    );
    body["access_token"]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("the session has no access token: {body}"))
}

/// A local listener that forwards each connection to one pod port. Drop closes it.
pub(crate) struct PortForward {
    pub(crate) address: SocketAddr,
    task: JoinHandle<()>,
}

impl PortForward {
    /// Forward connections to a named pod and port for metrics or a TLS handshake (C7, C11).
    pub(crate) async fn start(pods: Api<Pod>, pod: &str, port: u16) -> anyhow::Result<Self> {
        let namespace = pods
            .namespace()
            .context("a port-forward needs a namespace")?;
        ensure!(port != 0, "a port-forward needs a port");
        pods.get_metadata(pod)
            .await
            .with_context(|| format!("read pod {namespace}/{pod} for port-forward"))?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let pod: Arc<str> = pod.into();
        let task = tokio::spawn(async move {
            // Aborting this task drops the set, which aborts every open connection.
            let mut connections = JoinSet::new();
            while let Ok((mut tcp, _)) = listener.accept().await {
                let pods = pods.clone();
                let pod = Arc::clone(&pod);
                connections.spawn(async move {
                    let forwarded = async {
                        let mut forwarder = pods.portforward(&pod, &[port]).await?;
                        let mut upstream = forwarder
                            .take_stream(port)
                            .context("the port-forward has no stream")?;
                        tokio::io::copy_bidirectional(&mut tcp, &mut upstream).await?;
                        drop(upstream);
                        forwarder.join().await?;
                        anyhow::Ok(())
                    };
                    if let Err(error) = forwarded.await {
                        eprintln!("port-forward to {pod}:{port}: {error:#}");
                    }
                });
            }
        });
        Ok(Self { address, task })
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One of [`CAPACITY`] places. Each test thread waits here before it creates an org.
struct Capacity;

impl Capacity {
    fn acquire() -> Self {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        while *running >= CAPACITY {
            running = FREED.wait(running).unwrap_or_else(PoisonError::into_inner);
        }
        *running += 1;
        Self
    }
}

impl Drop for Capacity {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        FREED.notify_one();
    }
}
