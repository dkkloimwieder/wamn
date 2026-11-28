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
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMap, Pod, PodSecurityContext, PodTemplateSpec, SeccompProfile,
    SecurityContext, Service,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{
    ApiResource, AttachParams, DeleteParams, DynamicObject, GroupVersionKind, ListParams,
    LogParams, Patch, PatchParams, PostParams,
};
use serde_json::{Value, json};
use tokio::time::Instant;
use wamn_http_client::HttpClient;
use wamn_identity_protocol::PrincipalId;
use wamn_package::local_operation::LocalOperation;
use wamn_registry::{OrgId, Triple, name};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[path = "../../support/cluster.rs"]
pub(crate) mod cluster;
pub(crate) use cluster::{Cluster, Observed, PLATFORM, PortForward, exec, run, secret_key};
mod failure;
pub(super) use failure::capture;
pub mod host;
pub mod request;
mod routes;
pub use routes::Routes;

macro_rules! journey {
    ($(#[$attribute:meta])* async fn $name:ident() -> anyhow::Result<()> $body:block) => {
        $(#[$attribute])*
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "requires: cluster, bin:helm"]
        async fn $name() -> anyhow::Result<()> {
            $crate::common::capture(stringify!($name), || async move $body).await
        }
    };
}
pub(super) use journey;

/// The number of tests that hold an org and environment at one time (Epic 15 ruling C9).
const CAPACITY: usize = 4;
pub(crate) const ENV: &str = "dev";
const PROJECT: &str = "fixture";
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

async fn load_cluster() -> anyhow::Result<Cluster> {
    Cluster::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).await
}

/// One org with its owner session, made for one test.
pub struct Org {
    pub cluster: Cluster,
    pub name: OrgId,
    pub owner: PrincipalId,
    pub token: String,
    _capacity: Option<Capacity>,
}

impl Org {
    /// Run `wamn org create` with a unique org and owner email, and open the owner session.
    pub async fn create() -> anyhow::Result<Self> {
        let capacity = failure::hold_capacity(Capacity::acquire());
        let cluster = load_cluster().await?;
        let name: OrgId =
            format!("j{}", &uuid::Uuid::new_v4().simple().to_string()[..10]).parse()?;
        failure::org(&name);
        let (command, email) = cluster.org_create_command(&name);
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

    /// Provision an env through the operator command.
    pub async fn create_env(&self, env: &wamn_registry::EnvName) -> anyhow::Result<()> {
        run(self.cluster.env_create_command(&self.name, env))
            .await
            .map(drop)
    }

    /// Provision the primary environment at the org apex.
    pub async fn create_prod_env(&self) -> anyhow::Result<()> {
        let mut command = self.cluster.env_create_command(&self.name, &ENV.parse()?);
        command.arg("--prod");
        run(command).await.map(drop)
    }

    /// Write an environment file with one project and one application, and run `wamn env apply`.
    pub async fn apply(self, app: &str) -> anyhow::Result<Env> {
        self.create_prod_env().await?;
        self.apply_created(app).await
    }

    /// Apply in an existing env, so a journey can start its watches before apply.
    pub async fn apply_created(self, app: &str) -> anyhow::Result<Env> {
        let scratch = ScratchRoot::create()?;
        let file = scratch.path().join("env.yaml");
        // A JSON document is a YAML document.
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({
                "org": self.name,
                "env": ENV,
                "target": "cloud",
                "revocation_seconds": 60,
                "database": {"instances": 1, "storage": "1Gi", "cpu": "100m", "memory": "256Mi"},
                "projects": [{"name": PROJECT, "apps": {app: {}}}],
            }))?,
        )?;
        let triple = Triple::new(self.name.clone(), ENV.parse()?, PROJECT.parse()?);
        failure::env(&triple);
        // The guard exists first, so a failed apply is also deleted.
        let env = Env {
            org: self,
            file,
            triple,
            scratch,
        };
        let mut command = env.org.command();
        command.args(["env", "apply"]).arg(&env.file);
        run(command).await?;
        Ok(env)
    }
}

/// One applied environment. Runs a plain `wamn env deactivate-project` after failure capture, if enabled.
pub struct Env {
    pub org: Org,
    pub file: PathBuf,
    pub triple: Triple,
    scratch: ScratchRoot,
}

impl Env {
    pub fn namespace(&self) -> String {
        name::env_key(&self.triple.org, &self.triple.env)
    }

    /// The primary environment is provisioned as prod and serves the org apex.
    pub fn public_host(&self) -> anyhow::Result<String> {
        Ok(name::org_host(
            &self.triple.org,
            &self.org.cluster.outputs.base_domain.parse()?,
        ))
    }

    /// The public application API base through the org apex.
    pub fn app_api_url(&self, app: &str) -> anyhow::Result<String> {
        Ok(format!(
            "http://{}{}api",
            self.public_host()?,
            name::app_mount(&self.triple.project, &app.parse()?)
        ))
    }

    /// Admit the observer login to the project database, and connect with it.
    ///
    /// Each `wamn env apply` and `wamn deploy` upgrades the project chart, which puts its
    /// pg_hba list back, so the admission runs again on each call.
    pub async fn observe(&self) -> anyhow::Result<Observed> {
        admit_observer(&self.org.cluster, &self.triple).await
    }

    /// Run `wamn deploy` for the application's source package.
    pub async fn deploy(&self, package: &Path, app: &str) -> anyhow::Result<()> {
        let mut command = self.org.command();
        command
            .arg("deploy")
            .arg(package)
            .args(["--app", app, "--env"])
            .arg(&self.file);
        run(command).await.map(drop)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        failure::delete_env(&self.org, &self.triple);
    }
}

/// A deployed fixture application and routes that an admin API service calls.
pub struct App {
    pub routes: Routes,
    caller: PrincipalId,
    pub env: Env,
}

impl App {
    /// Create an org and environment, and deploy `source` as application `app`.
    pub async fn prepare(source: &Path, app: &str) -> anyhow::Result<Self> {
        let package = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("fixture directory name")?;
        let env = Org::create().await?.apply(app).await?;
        env.deploy(source, app).await?;
        let org = &env.org;
        let outputs = &org.cluster.outputs;
        let host = env.public_host()?;
        let mount = name::app_mount(&env.triple.project, &app.parse()?);
        // The fixture routes accept a PAT only, and an API service cannot hold `admin`.
        caller_role(&env, source, package, &env.app_api_url(app)?).await?;
        let secret = env.scratch.path().join("caller.json");
        let triple = &env.triple;
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
            endpoint: format!("http://{}{mount}api", outputs.web_address),
            host,
            bearer,
            client: HttpClient::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        };
        Ok(Self {
            routes,
            caller,
            env,
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
    env: &Env,
    source: &Path,
    package: &str,
    host_url: &str,
) -> anyhow::Result<()> {
    // `wamn deploy` builds the package into `<workspace>/target/wamn/<package>`.
    let manifest = std::fs::read(
        source
            .parent()
            .context("fixture workspace")?
            .join("target/wamn")
            .join(package)
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
    let mut command = env.org.command();
    command
        .args(["create-role", "--host-url", host_url])
        .args(["--role", CALLER_ROLE]);
    run(command).await?;
    for operation in &operations {
        let mut command = env.org.command();
        command
            .args(["grant-permission", "--host-url", host_url])
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
    let instance = wamn_env::registry::deployment::instance_suffix(&mut system.connection, triple)
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
    observe_project_cluster(cluster, &name::env_key(&triple.org, &triple.env), &name).await
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

/// Run one restricted PostgreSQL-image probe Job, collect its exit code and logs, and delete it.
pub async fn run_job(
    client: &kube::Client,
    namespace: &str,
    mut template: PodTemplateSpec,
    deadline: Duration,
) -> anyhow::Result<(i32, String)> {
    // The CloudNativePG PostgreSQL image uses UID and GID 26.
    const POSTGRES_ID: i64 = 26;

    ensure!(
        template
            .metadata
            .as_ref()
            .and_then(|meta| meta.labels.as_ref())
            .is_some_and(|labels| !labels.is_empty()),
        "the probe pod needs labels"
    );
    let spec = template.spec.as_mut().context("probe pod spec is absent")?;
    let [container] = spec.containers.as_mut_slice() else {
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
    container.security_context = Some(SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".into()]),
            ..Capabilities::default()
        }),
        ..SecurityContext::default()
    });
    spec.security_context = Some(PodSecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(POSTGRES_ID),
        run_as_group: Some(POSTGRES_ID),
        fs_group: Some(POSTGRES_ID),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".into(),
            ..SeccompProfile::default()
        }),
        ..PodSecurityContext::default()
    });
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
/// Ask the API server to authorize a request with the groups of a service-account token.
pub(crate) async fn service_account_can(
    reviews: &Api<SubjectAccessReview>,
    namespace: &str,
    account: &str,
    attributes: ResourceAttributes,
) -> anyhow::Result<bool> {
    let review = SubjectAccessReview {
        spec: SubjectAccessReviewSpec {
            user: Some(format!("system:serviceaccount:{namespace}:{account}")),
            groups: Some(vec![
                "system:authenticated".into(),
                "system:serviceaccounts".into(),
                format!("system:serviceaccounts:{namespace}"),
            ]),
            resource_attributes: Some(attributes),
            ..SubjectAccessReviewSpec::default()
        },
        ..SubjectAccessReview::default()
    };
    let status = reviews
        .create(&PostParams::default(), &review)
        .await?
        .status
        .context("subject access review has no status")?;
    if let Some(error) = status.evaluation_error.filter(|error| !error.is_empty()) {
        bail!("subject access review evaluation failed: {error}");
    }
    Ok(status.allowed)
}

/// Run a command and return its standard output.
/// The identity pods that the identity Service selects.
pub(crate) async fn identity_pods(client: &kube::Client) -> anyhow::Result<(Service, Vec<Pod>)> {
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

/// Use the pod's named identity port so chart overrides reach direct callers.
pub(crate) fn identity_port(pod: &Pod, name: &str) -> anyhow::Result<u16> {
    let port = pod
        .spec
        .as_ref()
        .context("identity pod spec")?
        .containers
        .iter()
        .flat_map(|container| container.ports.iter().flatten())
        .find(|port| port.name.as_deref() == Some(name))
        .with_context(|| format!("identity pod has no {name} port"))?;
    Ok(u16::try_from(port.container_port)?)
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
        .iter()
        .find(|pod| host::ready(pod))
        .context("no identity pod is ready")?;
    let port = identity_port(pod, "password")?;
    let pod_name = pod.metadata.name.as_deref().context("identity pod name")?;
    let ca = Api::<ConfigMap>::namespaced(client.clone(), "hosts")
        .get("identity-ca")
        .await?
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .context("the identity CA has no ca.crt")?;
    let forward =
        PortForward::start(Api::namespaced(client.clone(), PLATFORM), pod_name, port).await?;
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
