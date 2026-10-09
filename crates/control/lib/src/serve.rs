//! `wamn-ctl serve`: the provisioning worker (`docs/plan/platform-ui.md`
//! §5.5, `wamn-zua8.3`).
//!
//! The worker polls the system database every 5 seconds with one query, and
//! runs the oldest open create or copy saga of an org with no other running
//! saga. It runs the steps of [`wamn_control_provision::saga::STEPS`] in
//! order, with the same library functions as the CLI verbs. A copy runs
//! [`wamn_control_provision::saga::COPY_STEPS`]: `read-source` first, then the
//! same steps with what it read (`wamn-zua8.4`). It runs the role
//! and privilege SQL as its own login `wamn_provisioner`, and applies
//! Kubernetes objects with `kubectl`, which reads the ServiceAccount of the
//! pod. A Secret reaches `kubectl apply -f -` on its standard input. A
//! library function that writes a Secret only to a file writes it into a
//! private mode 0700 directory, and the worker shreds the file after the
//! apply. A failed step keeps its error, without PostgreSQL error text, and
//! the saga waits for `saga-resume` or `saga-abandon`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, Command};
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::saga::{COPY_STEPS, EnvironmentRequest, STEPS};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wamn_control_registry::Triple;

use crate::environment_saga::{self, OpenSaga, SagaRequest};
use crate::package_artifact::{
    COMPONENT_LIST, ListedComponent, OpenedPackage, PackageRegistry, PackageSource,
    open_package_source,
};
use crate::pat_client::PatIssuerConfig;
use crate::provision_project_env::{
    ProvisionProjectEnvRequest, WorkloadActionRequest, WorkloadActionVerb,
    WorkloadGenerationAction, provision_project_env, read_project_env_instance,
    run_workload_action,
};
use crate::release_composition::{PackageInput, SelectedComponentArtifact, WiringInput};

/// The interval between two polls of the system database.
const POLL: Duration = Duration::from_secs(5);
/// The namespace of the CloudNativePG cluster, its `Database` and
/// `Publication` CRs, and the CDC and registry reader Secrets.
const PLATFORM_NAMESPACE: &str = "platform";
/// The namespace of the host and gate Secrets.
const HOSTS_NAMESPACE: &str = "hosts";
/// The namespace of the session target Secret.
const IDENTITY_NAMESPACE: &str = "identity";
/// The run-plane schema of every environment.
const RUN_SCHEMA: &str = "wamn_run";
/// The bead that a request with more than one package schema waits for.
const MULTI_SCHEMA_BEAD: &str = "wamn-64iw";
/// How long the worker waits for a CR to report `status.applied`.
const APPLIED_TIMEOUT: &str = "--timeout=120s";
/// The platform packages that a package's own component is admitted with, as
/// `upgrade-environment` admits it.
const OWN_PLATFORM_PACKAGES: [&str; 2] = ["wamn:node", "wamn:postgres"];

/// The inputs of the worker.
pub struct ServeConfig {
    /// `wamn_provisioner` URL of the system database.
    pub system_database_url: String,
    /// Host that every emitted credential URL names.
    pub db_host: String,
    /// Port that every emitted credential URL names.
    pub db_port: u16,
    /// The event broker and the worker's own provisioning credential.
    pub nats_url: String,
    pub nats_username: String,
    pub nats_password_file: PathBuf,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
    /// The bucket of the web clients, `s3://` or `gs://`.
    pub ui_bucket: String,
    /// `<registry>/<repository>` bases of package, component and release
    /// manifest artifacts.
    pub package_artifact_base: String,
    pub component_artifact_base: String,
    pub release_artifact_base: String,
    pub registry_auth_file: PathBuf,
    pub insecure_registry: bool,
    pub oci_ca_paths: Vec<PathBuf>,
    /// The identity service that issues the management-author PAT.
    pub pat_issuer: PatIssuerConfig,
    /// The `wamn-scenario-worker` program that gates each wiring.
    pub scenario_worker: PathBuf,
}

impl fmt::Debug for ServeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServeConfig")
            .field("system_database_url", &"[REDACTED]")
            .field("db_host", &self.db_host)
            .field("ui_bucket", &self.ui_bucket)
            .field("pat_issuer", &self.pat_issuer)
            .finish_non_exhaustive()
    }
}

/// Run sagas until the process ends. Only a failure to read or record a saga
/// ends the loop.
pub async fn serve(config: &ServeConfig) -> anyhow::Result<()> {
    loop {
        let client = connect(&config.system_database_url).await?;
        while let Some(open) = environment_saga::next_open_saga(&client).await? {
            run_saga(config, &client, open).await?;
        }
        drop(client);
        tokio::time::sleep(POLL).await;
    }
}

/// Run one saga from its first step that is not completed. A failed step
/// ends the run and leaves the saga failed.
pub async fn run_saga(config: &ServeConfig, client: &Client, open: OpenSaga) -> anyhow::Result<()> {
    let saga_id = open.saga_id.as_str();
    let mut next_step = open.next_step;
    let (steps, source_env, request): (&[&str], _, _) = match open.request {
        SagaRequest::Create(request) => (&STEPS, None, request),
        SagaRequest::Copy(copy) => {
            // A copy runs the create steps with what `read-source` read, and
            // a resumed copy reads that again from the step's detail.
            let read = if next_step == 1 {
                environment_saga::start_step(client, saga_id, 1).await?;
                tracing::info!(saga = %saga_id, step = 1, name = "read-source", "step started");
                let source_url = project_env_url(
                    &config.system_database_url,
                    &open.org,
                    &copy.project,
                    &copy.source_env,
                )
                .await?;
                match crate::environment_copy::read_source(
                    &source_url,
                    &copy.source_env,
                    &copy.connections,
                )
                .await
                {
                    Ok(read) => {
                        let detail = serde_json::to_value(&read).context("encode the source")?;
                        environment_saga::complete_step(client, saga_id, 1, Some(&detail)).await?;
                        next_step = 2;
                        read
                    }
                    Err(error) => {
                        let error = step_error(&error);
                        tracing::warn!(saga = %saga_id, step = 1, name = "read-source", %error, "step failed");
                        environment_saga::fail_step(client, saga_id, 1, &error).await?;
                        return Ok(());
                    }
                }
            } else {
                environment_saga::read_source_detail(client, saga_id).await?
            };
            let source_env = copy.source_env.clone();
            (&COPY_STEPS, Some(source_env), copy.environment(read))
        }
    };
    let mut run = SagaRun {
        config,
        org: open.org,
        request,
        source_env,
        packages: Vec::new(),
    };
    for step in next_step..=i32::try_from(steps.len()).context("count the steps")? {
        let name = steps[usize::try_from(step - 1).context("index the step")?];
        environment_saga::start_step(client, saga_id, step).await?;
        tracing::info!(saga = %saga_id, step, name, "step started");
        if name == "awaiting-operator" {
            let detail = run.operator_commands();
            environment_saga::await_operator(client, saga_id, step, &detail).await?;
            return Ok(());
        }
        match run.step(name).await {
            Ok(detail) => {
                environment_saga::complete_step(client, saga_id, step, detail.as_ref()).await?;
            }
            Err(error) => {
                let error = step_error(&error);
                tracing::warn!(saga = %saga_id, step, name, %error, "step failed");
                environment_saga::fail_step(client, saga_id, step, &error).await?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The `wamn_provisioner` URL of the database of one project environment.
async fn project_env_url(
    system_url: &str,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<String> {
    let instance = read_project_env_instance(system_url, &Triple::new(org, project, env)).await?;
    let mut url = url::Url::parse(system_url).context("parse the system URL")?;
    url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(org, project, env, &instance)
    ));
    Ok(url.to_string())
}

/// The error text of a failed step. A PostgreSQL error is named by its
/// SQLSTATE and constraint only, because its message and DETAIL can repeat a
/// row, and a row can hold personal data.
pub fn step_error(error: &anyhow::Error) -> String {
    let mut parts = Vec::new();
    for cause in error.chain() {
        if let Some(database) = cause.downcast_ref::<tokio_postgres::error::DbError>() {
            parts.push(match database.constraint() {
                Some(constraint) => format!(
                    "database error {} on constraint {constraint}",
                    database.code().code()
                ),
                None => format!("database error {}", database.code().code()),
            });
            break;
        }
        if let Some(postgres) = cause.downcast_ref::<tokio_postgres::Error>()
            && let Some(database) = postgres.as_db_error()
        {
            parts.push(match database.constraint() {
                Some(constraint) => format!(
                    "database error {} on constraint {constraint}",
                    database.code().code()
                ),
                None => format!("database error {}", database.code().code()),
            });
            break;
        }
        parts.push(cause.to_string());
    }
    parts.join(": ")
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// One open package: its root and its manifest.
struct Package {
    opened: OpenedPackage,
    manifest: wamn_schema_generator::PackageManifest,
}

impl Package {
    fn root(&self) -> &Path {
        self.opened.root()
    }

    fn input(&self) -> PackageInput {
        PackageInput {
            root: self.root().to_path_buf(),
            manifest: self.manifest.clone(),
        }
    }

    fn components(&self) -> anyhow::Result<Vec<ListedComponent>> {
        let path = self.root().join(COMPONENT_LIST);
        serde_json::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {COMPONENT_LIST}"))?,
        )
        .with_context(|| format!("parse {COMPONENT_LIST} of {}", self.manifest.package.id))
    }
}

/// The state of one saga run in this process.
struct SagaRun<'a> {
    config: &'a ServeConfig,
    org: String,
    request: EnvironmentRequest,
    /// The source environment of a copy.
    source_env: Option<String>,
    /// The package artifacts, opened by the first step that reads them.
    packages: Vec<Package>,
}

impl SagaRun<'_> {
    /// Run one step, and return the detail it records, if any.
    async fn step(&mut self, name: &str) -> anyhow::Result<Option<Value>> {
        if name == "prepare-credentials" {
            return self.prepare_credentials().await.map(Some);
        }
        match name {
            "provision-project-env" => self.provision_project_env().await,
            "reconcile-run-plane" => self.reconcile_run_plane().await,
            "apply-packages" => self.apply_packages().await,
            "reconcile-package-data-access" => self.reconcile_package_data_access().await,
            "enable-cdc" => self.enable_cdc().await,
            "wait-publication" => self.wait_publication().await,
            "admit-components" => self.admit_components().await,
            "publish-release" => self.publish_release().await,
            "bind-connection" => self.bind_connections().await,
            "push-release-manifest" => self.push_release_manifest().await,
            "select-release" => self.select_release().await,
            "upload-ui" => self.upload_ui().await,
            "materialize-admin-grants" => self.materialize_admin_grants().await,
            "copy-roles" => self.copy_roles().await,
            other => bail!("the worker has no step {other}"),
        }
        .map(|()| None)
    }

    fn triple(&self) -> Triple {
        Triple::new(
            self.org.as_str(),
            self.request.project.as_str(),
            self.request.env.as_str(),
        )
    }

    /// The tenant of the environment, derived from its coordinate (owner
    /// ruling of 2026-10-09 on `wamn-snz0`). The request names none.
    fn tenant(&self) -> String {
        wamn_control_registry::project_env_tenant(
            &self.org,
            &self.request.project,
            &self.request.env,
        )
    }

    fn system_url(&self) -> &str {
        &self.config.system_database_url
    }

    async fn instance(&self) -> anyhow::Result<String> {
        read_project_env_instance(self.system_url(), &self.triple()).await
    }

    async fn database(&self) -> anyhow::Result<String> {
        Ok(wamn_control_provision::project_env_database_name(
            &self.org,
            &self.request.project,
            &self.request.env,
            &self.instance().await?,
        ))
    }

    /// The `wamn_provisioner` URL of the project-environment database.
    async fn project_url(&self) -> anyhow::Result<String> {
        project_env_url(
            self.system_url(),
            &self.org,
            &self.request.project,
            &self.request.env,
        )
        .await
    }

    /// The manifest digest of the release that `publish-release` recorded. The
    /// environment is new, so the saga's release is the one release its
    /// database holds.
    async fn published_release(&self, project_url: &str) -> anyhow::Result<String> {
        let rows = connect(project_url)
            .await?
            .query(
                "SELECT manifest_digest FROM catalog.releases WHERE tenant_id = $1",
                &[&self.tenant()],
            )
            .await
            .context("read the published release")?;
        match rows.as_slice() {
            [row] => Ok(row.get(0)),
            [] => bail!("the environment has no published release"),
            _ => bail!(
                "the environment holds {} releases; the saga published one",
                rows.len()
            ),
        }
    }

    fn registry(&self) -> PackageRegistry {
        PackageRegistry {
            artifact_base: self.config.package_artifact_base.clone(),
            registry_auth_file: self.config.registry_auth_file.clone(),
            insecure_registry: self.config.insecure_registry,
            oci_ca_paths: self.config.oci_ca_paths.clone(),
            control_database_url: self.config.system_database_url.clone(),
        }
    }

    async fn packages(&mut self) -> anyhow::Result<&[Package]> {
        if self.packages.is_empty() {
            for package in &self.request.packages {
                let opened = open_package_source(PackageSource::Artifact {
                    tag: format!("{}-{}", package.package_id, package.version),
                    registry: self.registry(),
                })
                .await?;
                let path = wamn_schema_generator::package_manifest_path(opened.root());
                let manifest = wamn_schema_generator::PackageManifest::from_slice(
                    &std::fs::read(&path).context("read the package manifest")?,
                )
                .context("parse the package manifest")?;
                self.packages.push(Package { opened, manifest });
            }
        }
        Ok(&self.packages)
    }

    async fn roots(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        Ok(self
            .packages()
            .await?
            .iter()
            .map(|package| package.root().to_path_buf())
            .collect())
    }

    fn workload(
        &self,
        family: WorkloadRoleFamily,
        namespace: &str,
        target: Option<String>,
        secret: PathBuf,
        control_patch: Option<PathBuf>,
    ) -> WorkloadActionRequest {
        WorkloadActionRequest {
            org: self.org.clone(),
            project: self.request.project.clone(),
            env: self.request.env.clone(),
            tenant: Some(self.tenant()),
            system_database_url: Some(self.config.system_database_url.clone()),
            target_admin_database_url: target,
            cluster: None,
            db_host: Some(self.config.db_host.clone()),
            db_port: self.config.db_port,
            namespace: namespace.to_owned(),
            action: WorkloadGenerationAction {
                family,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            },
            secret: Some(secret),
            emit_role_sql: None,
            control_administration_patch: control_patch,
        }
    }

    /// Step 1: the registry rows, the roles, the `Database` CR, the database
    /// Secret and the management-author PAT.
    async fn provision_project_env(&mut self) -> anyhow::Result<()> {
        let private = PrivateDirectory::create()?;
        let database_secret = private.path("database.json");
        let pat_secret = private.path("management-author-pat.json");
        let outcome = provision_project_env(&ProvisionProjectEnvRequest {
            org: self.org.clone(),
            project: self.request.project.clone(),
            env: self.request.env.clone(),
            tenant: Some(self.tenant()),
            disposable: false,
            system_database_url: Some(self.config.system_database_url.clone()),
            cluster: None,
            connection_limit: None,
            cluster_namespace: PLATFORM_NAMESPACE.to_owned(),
            namespace: HOSTS_NAMESPACE.to_owned(),
            secret_namespace: Some(HOSTS_NAMESPACE.to_owned()),
            // `-` returns these three in the outcome and writes no file.
            emit_database: Some(PathBuf::from("-")),
            emit_role_sql: Some(PathBuf::from("-")),
            emit_privilege_sql: Some(PathBuf::from("-")),
            emit_secret: Some(database_secret.clone()),
            pat_issuer: self.config.pat_issuer.clone(),
            emit_management_author_pat_secret: Some(pat_secret.clone()),
            emit_operator_pat_secret: None,
        })
        .await?;
        let system = connect(self.system_url()).await?;
        system
            .batch_execute(&outcome.role_sql)
            .await
            .context("run the role SQL")?;
        kubectl_apply(&serde_json::to_vec(&outcome.database_cr)?).await?;
        wait_applied("database", &outcome.database).await?;
        system
            .batch_execute(&outcome.privilege_sql)
            .await
            .context("run the privilege SQL")?;
        private.apply_and_shred(&database_secret).await?;
        private.apply_and_shred(&pat_secret).await?;
        Ok(())
    }

    /// Step 2: the run plane. It reads the registry row and the database of
    /// step 1, and creates the run schema that the credentials of step 3 are
    /// granted on.
    async fn reconcile_run_plane(&mut self) -> anyhow::Result<()> {
        crate::reconcile_run_plane::reconcile_run_plane(
            crate::reconcile_run_plane::ReconcileRunPlaneRequest {
                system_database_url: self.config.system_database_url.clone(),
                admin_database_url: self.project_url().await?,
                org: self.org.clone(),
                project: self.request.project.clone(),
                tenant: self.tenant(),
                env: self.request.env.clone(),
                schema: RUN_SCHEMA.to_owned(),
                dry_run: false,
            },
        )
        .await?;
        Ok(())
    }

    /// Step 3: generation `a` of every credential of the environment. Its
    /// detail names the databases that the grant checks skipped, because
    /// `wamn_provisioner` cannot connect to them (owner ruling of 2026-10-03).
    async fn prepare_credentials(&mut self) -> anyhow::Result<Value> {
        let private = PrivateDirectory::create()?;
        let system = connect(self.system_url()).await?;
        let project_url = self.project_url().await?;
        let families = [
            (WorkloadRoleFamily::App, HOSTS_NAMESPACE, true),
            (WorkloadRoleFamily::ExecutorPlatform, HOSTS_NAMESPACE, true),
            (WorkloadRoleFamily::HttpAdmitter, HOSTS_NAMESPACE, true),
            (WorkloadRoleFamily::EventMaterializer, HOSTS_NAMESPACE, true),
            (WorkloadRoleFamily::IdentityReader, HOSTS_NAMESPACE, false),
            (WorkloadRoleFamily::Administration, HOSTS_NAMESPACE, true),
            (WorkloadRoleFamily::ControlAuthor, HOSTS_NAMESPACE, false),
            (
                WorkloadRoleFamily::ManagementAdmitter,
                HOSTS_NAMESPACE,
                true,
            ),
            (
                WorkloadRoleFamily::SessionRoleReader,
                IDENTITY_NAMESPACE,
                true,
            ),
            (
                WorkloadRoleFamily::RegistryReader,
                PLATFORM_NAMESPACE,
                false,
            ),
        ];
        for (family, namespace, project_database) in families {
            let secret = private.path(&format!("{}.json", family.label()));
            let patch = (family == WorkloadRoleFamily::Administration)
                .then(|| private.path("control-administration-patch.json"));
            run_workload_action(&self.workload(
                family,
                namespace,
                project_database.then(|| project_url.clone()),
                secret.clone(),
                patch.clone(),
            ))
            .await
            .with_context(|| format!("prepare generation a of {}", family.label()))?;
            private.apply_and_shred(&secret).await?;
            if let Some(patch) = patch {
                let name = wamn_control_provision::control_administration_secret_name(&self.org);
                private
                    .patch_and_shred(&patch, HOSTS_NAMESPACE, &name)
                    .await?;
            }
        }
        let skipped: Vec<String> = system
            .query(
                wamn_control_provision::sql::non_template_databases_sql(),
                &[],
            )
            .await
            .context("list the databases that the grant checks skipped")?
            .iter()
            .filter(|row| !row.get::<_, bool>(1))
            .map(|row| row.get(0))
            .collect();
        Ok(json!({ "skipped_databases": skipped }))
    }

    /// Step 4: every package artifact of the request.
    async fn apply_packages(&mut self) -> anyhow::Result<()> {
        let project_url = self.project_url().await?;
        for root in self.roots().await? {
            crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
                package: root,
                database_url: project_url.clone(),
                tenant: self.tenant(),
            })
            .await?;
        }
        Ok(())
    }

    /// Step 5: the generated data access of the installed packages.
    async fn reconcile_package_data_access(&mut self) -> anyhow::Result<()> {
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: self.roots().await?,
                database_url: self.project_url().await?,
                tenant: self.tenant(),
            },
        )
        .await?;
        Ok(())
    }

    /// Step 6: the source stream, the replication role, the slot, the
    /// `Publication` CR and the replication Secret.
    async fn enable_cdc(&mut self) -> anyhow::Result<()> {
        let triple = self.triple();
        let tenant = self.tenant();
        let packages = self.packages().await?;
        let schemas: BTreeSet<&str> = packages
            .iter()
            .flat_map(|package| package.manifest.models.values())
            .map(|model| model.schema.as_str())
            .collect();
        let schema = match schemas.into_iter().collect::<Vec<_>>().as_slice() {
            [schema] => (*schema).to_owned(),
            [] => bail!("the packages of the request declare no model schema"),
            several => bail!(
                "the packages of the request declare the schemas {several:?}; one environment \
                 captures one schema until {MULTI_SCHEMA_BEAD} lands"
            ),
        };
        let manifests: Vec<_> = packages
            .iter()
            .map(|package| package.manifest.clone())
            .collect();
        let consumers =
            wamn_control_provision::events::registration_consumers(&triple, &tenant, &manifests)
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()?;
        let outcome = crate::enable_cdc_project_env::enable_cdc_project_env(
            &crate::enable_cdc_project_env::EnableCdcProjectEnvRequest {
                org: self.org.clone(),
                project: self.request.project.clone(),
                env: self.request.env.clone(),
                schema,
                system_database_url: Some(self.config.system_database_url.clone()),
                cluster: None,
                replication_password: random_password()?,
                db_host: Some(self.config.db_host.clone()),
                db_port: self.config.db_port,
                namespace: PLATFORM_NAMESPACE.to_owned(),
                cluster_namespace: PLATFORM_NAMESPACE.to_owned(),
                secret_namespace: Some(PLATFORM_NAMESPACE.to_owned()),
                stream: None,
                nats_url: self.config.nats_url.clone(),
                nats_username: self.config.nats_username.clone(),
                nats_password_file: self.config.nats_password_file.clone(),
                stream_replicas: self.config.stream_replicas,
                dup_window_secs: self.config.dup_window_secs,
                consumer_config: consumers,
                emit_role_sql: None,
                emit_cdc_sql: None,
                emit_publication: None,
                emit_secret: None,
            },
        )
        .await?;
        connect(self.system_url())
            .await?
            .batch_execute(&outcome.role_sql)
            .await
            .context("run the replication role SQL")?;
        connect(&self.project_url().await?)
            .await?
            .batch_execute(&outcome.cdc_sql)
            .await
            .context("run the CDC SQL")?;
        kubectl_apply(&serde_json::to_vec(&outcome.publication)?).await?;
        kubectl_apply(&serde_json::to_vec(&outcome.secret)?).await?;
        Ok(())
    }

    /// Step 7: the `Database` and `Publication` CRs report `status.applied`.
    async fn wait_publication(&mut self) -> anyhow::Result<()> {
        let database = self.database().await?;
        wait_applied("database", &database).await?;
        wait_applied("publication", &database).await
    }

    /// Step 8: every component that a package lists, fetched by its digest
    /// and admitted into the environment.
    async fn admit_components(&mut self) -> anyhow::Result<()> {
        use crate::component_declaration::{
            authored_base_digests, declared_platform_packages, render_declaration_document,
            render_palette_declaration,
        };
        let project_url = self.project_url().await?;
        let tenant = self.tenant();
        let config = self.config;
        let packages = self.packages().await?;
        let inputs: Vec<PackageInput> = packages.iter().map(Package::input).collect();
        let wirings = crate::release_composition::load_wirings(&inputs)?;
        let private = PrivateDirectory::create()?;
        for package in packages {
            let root = package.root();
            for listed in package.components()? {
                let digest = format!("sha256:{}", listed.sha256);
                let bytes = crate::push_component::pull_component_bytes(
                    &config.component_artifact_base,
                    &digest,
                    &config.registry_auth_file,
                    config.insecure_registry,
                    &config.oci_ca_paths,
                )
                .await?;
                let component_file = private.path(&format!("{}.wasm", listed.name));
                std::fs::write(&component_file, &bytes)?;
                let template = root
                    .join("publication/components")
                    .join(format!("{}.json.in", listed.name));
                let (document, admitted) = if package.manifest.components.contains_key(&listed.name)
                {
                    let document = render_declaration_document(
                        &template,
                        &tenant,
                        &authored_base_digests(root)?,
                    )?;
                    let admitted = OWN_PLATFORM_PACKAGES.map(str::to_owned).to_vec();
                    (document, admitted)
                } else {
                    let artifact = SelectedComponentArtifact {
                        package_id: package.manifest.package.id.as_str().into(),
                        package_version: package.manifest.package.version.as_str().into(),
                        component: listed.name.as_str().into(),
                        path: component_file.clone(),
                        digest: digest.as_str().into(),
                    };
                    let alias =
                        crate::release_composition::wiring_store_alias(&wirings, &artifact)?;
                    let scope = wamn_catalog::ComponentPackageScope {
                        tenant_id: tenant.clone(),
                        package_id: package.manifest.package.id.clone(),
                        package_version: package.manifest.package.version.clone(),
                    };
                    let document = render_palette_declaration(&template, &scope, alias.as_deref())?;
                    let admitted = declared_platform_packages(&template, &document)?;
                    (document, admitted)
                };
                let declaration = private.path(&format!("{}.declaration.json", listed.name));
                std::fs::write(&declaration, serde_json::to_vec(&document)?)?;
                crate::push_component::push_component(
                    crate::push_component::AdmitComponentRequest {
                        package: root.to_path_buf(),
                        component_bytes: component_file,
                        declaration,
                        admitted_platform_packages: admitted,
                    },
                    crate::push_component::PublishAdmittedComponentRequest {
                        artifact_base: config.component_artifact_base.clone(),
                        registry_auth_file: config.registry_auth_file.clone(),
                        insecure_registry: config.insecure_registry,
                        oci_ca_paths: config.oci_ca_paths.clone(),
                        project_database_url: project_url.clone(),
                        control_database_url: config.system_database_url.clone(),
                    },
                )
                .await
                .with_context(|| format!("admit the component {} {digest}", listed.name))?;
            }
        }
        Ok(())
    }

    /// Step 9: each wiring gated and authored, then release 1 published.
    async fn publish_release(&mut self) -> anyhow::Result<()> {
        let project_url = self.project_url().await?;
        let pat = read_secret_key(HOSTS_NAMESPACE, &self.pat_secret_name(), "token").await?;
        let publisher = kubectl(
            &[
                "-n",
                HOSTS_NAMESPACE,
                "get",
                "secret",
                &self.pat_secret_name(),
                "-o",
                "jsonpath={.metadata.annotations.wamn\\.io/principal-subject}",
            ],
            None,
        )
        .await?;
        ensure!(
            !publisher.is_empty(),
            "the PAT Secret names no principal subject"
        );
        let gate_urls = (
            self.workload_url(WorkloadRoleFamily::IdentityReader)
                .await?,
            self.workload_url(WorkloadRoleFamily::ControlAuthor).await?,
            self.workload_url(WorkloadRoleFamily::ManagementAdmitter)
                .await?,
        );
        let (org, project, env, tenant) = (
            self.org.clone(),
            self.request.project.clone(),
            self.request.env.clone(),
            self.tenant(),
        );
        let config = self.config;
        let packages = self.packages().await?;
        let inputs: Vec<PackageInput> = packages.iter().map(Package::input).collect();
        let wirings = crate::release_composition::load_wirings(&inputs)?;
        let mut targets = Vec::new();
        if !wirings.is_empty() {
            let private = PrivateDirectory::create()?;
            let gate = Gate::start(
                config,
                &private,
                &gate_urls,
                [&org, &project, &env, &tenant],
            )
            .await?;
            for input in &wirings {
                gate.gate(&pat, input, &project, &env).await?;
                crate::author_wiring::author_wiring_document(
                    crate::author_wiring::AuthorWiringDocumentRequest {
                        database_url: project_url.clone(),
                        control_database_url: config.system_database_url.clone(),
                        tenant: tenant.clone(),
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        wiring_document: input.path.clone(),
                    },
                )
                .await?;
                targets.push(crate::publish_release::ReleaseWiringTarget {
                    package_id: input.package_id.to_string(),
                    package_version: input.package_version.to_string(),
                    wiring_id: input.wiring.wiring_id.clone(),
                    wiring_version: input.wiring.version,
                });
            }
        }
        crate::publish_release::publish_release(crate::publish_release::PublishReleaseRequest {
            database_url: project_url,
            control_database_url: config.system_database_url.clone(),
            org,
            project,
            tenant,
            environment: env,
            verified_publisher_principal: publisher,
            run_schema: RUN_SCHEMA.to_owned(),
            packages: packages
                .iter()
                .map(|package| {
                    wamn_catalog::PackageCoordinate::new(
                        &package.manifest.package.id,
                        &package.manifest.package.version,
                    )
                })
                .collect::<Result<_, _>>()?,
            wirings: targets,
            attachments: packages
                .iter()
                .map(|package| package.root().join("publication/attachments.json"))
                .filter(|path| path.is_file())
                .collect(),
            package_manifests: packages
                .iter()
                .map(|package| wamn_schema_generator::package_manifest_path(package.root()))
                .collect(),
        })
        .await?;
        Ok(())
    }

    fn pat_secret_name(&self) -> String {
        format!(
            "wamn-pat-management-author-{}--{}--{}",
            self.org, self.request.project, self.request.env
        )
    }

    /// The URL of the credential Secret of `family` in `hosts`.
    async fn workload_url(&self, family: WorkloadRoleFamily) -> anyhow::Result<String> {
        let name = wamn_control_provision::workload_secret_name(
            family,
            &self.org,
            &self.request.project,
            &self.request.env,
        );
        read_secret_key(HOSTS_NAMESPACE, &name, "url").await
    }

    /// Step 10: each connection of the request bound to the published release,
    /// at the component that the wiring node with its store alias names.
    async fn bind_connections(&mut self) -> anyhow::Result<()> {
        let project_url = self.project_url().await?;
        let release = self.published_release(&project_url).await?;
        let (tenant, env) = (self.tenant(), self.request.env.clone());
        let connections = self.request.connections.clone();
        let packages = self.packages().await?;
        let inputs: Vec<PackageInput> = packages.iter().map(Package::input).collect();
        let wirings = crate::release_composition::load_wirings(&inputs)?;
        let mut digests = BTreeMap::new();
        for package in packages {
            for listed in package.components()? {
                digests.insert(
                    (package.manifest.package.id.clone(), listed.name),
                    format!("sha256:{}", listed.sha256),
                );
            }
        }
        let private = PrivateDirectory::create()?;
        for connection in connections {
            let digest = alias_component(&wirings, &connection.alias)?
                .and_then(|key| digests.get(&key))
                .with_context(|| {
                    format!(
                        "no listed component has a wiring node with the store alias {}",
                        connection.alias
                    )
                })?;
            let definition = private.path(&format!("{}.definition.json", connection.instance_id));
            std::fs::write(&definition, serde_json::to_vec(&connection.definition)?)?;
            crate::bind_connection::bind(&crate::bind_connection::BindConnectionRequest {
                database_url: project_url.clone(),
                tenant: tenant.clone(),
                environment: env.clone(),
                instance_id: connection.instance_id.clone(),
                requirement_type: connection.requirement_type,
                definition,
                credential_handle: None,
                manifest_digest: release.clone(),
                component_digest: digest.clone(),
                store_alias: connection.alias.clone(),
            })
            .await
            .with_context(|| format!("bind the connection {}", connection.instance_id))?;
        }
        Ok(())
    }

    async fn release_request(
        &self,
        project_url: String,
    ) -> anyhow::Result<crate::push_release_manifest::PushReleaseManifestRequest> {
        let manifest_digest = self.published_release(&project_url).await?;
        Ok(crate::push_release_manifest::PushReleaseManifestRequest {
            database_url: project_url,
            org: self.org.clone(),
            project: self.request.project.clone(),
            environment: self.request.env.clone(),
            tenant: self.tenant(),
            manifest_digest,
            artifact_base: self.config.release_artifact_base.clone(),
            registry_auth_file: self.config.registry_auth_file.clone(),
            insecure_registry: self.config.insecure_registry,
            oci_ca_paths: self.config.oci_ca_paths.clone(),
            control_database_url: self.config.system_database_url.clone(),
        })
    }

    /// Step 11: the release manifest pushed and attested.
    async fn push_release_manifest(&mut self) -> anyhow::Result<()> {
        let request = self.release_request(self.project_url().await?).await?;
        crate::push_release_manifest::push_release_manifest(&request, None).await?;
        Ok(())
    }

    /// Step 12: the published release selected with no qualification,
    /// because the new environment has no head. A head that the environment
    /// already has satisfies the step when its package set and component
    /// digests equal those of the published release, and the later steps use
    /// that head. A head with another set fails the step (owner rulings of
    /// 2026-10-03).
    async fn select_release(&mut self) -> anyhow::Result<()> {
        use crate::delivery::selection::package_set;
        use crate::print_release_env::lookup_release_snapshot;
        let project_url = self.project_url().await?;
        let head = head_release(&project_url, &self.tenant(), &self.request.env).await?;
        let request = self.release_request(project_url).await?;
        if let Some(head) = head {
            let mut sets = Vec::new();
            for release in [&request.manifest_digest, &head] {
                let snapshot = lookup_release_snapshot(
                    &request.database_url,
                    &request.tenant,
                    release,
                    &request.artifact_base,
                )
                .await?;
                sets.push(package_set(&snapshot.manifest)?);
            }
            let (published, current) = (&sets[0], &sets[1]);
            ensure!(
                published == current,
                "the head release {head} of {} holds {current:?}, but release {} that \
                 the saga published holds {published:?}",
                self.request.env,
                request.manifest_digest
            );
            return Ok(());
        }
        crate::delivery::deployment::select_new_environment(&request).await?;
        Ok(())
    }

    /// `copy-roles` of a copy: the authored roles of the source and their
    /// directly selected permissions, with each closure from the published
    /// release.
    async fn copy_roles(&mut self) -> anyhow::Result<()> {
        let source_env = self
            .source_env
            .clone()
            .context("copy-roles runs only in a copy")?;
        let source_url = project_env_url(
            self.system_url(),
            &self.org,
            &self.request.project,
            &source_env,
        )
        .await?;
        let request = self.release_request(self.project_url().await?).await?;
        let snapshot = crate::print_release_env::lookup_release_snapshot(
            &request.database_url,
            &request.tenant,
            &request.manifest_digest,
            &request.artifact_base,
        )
        .await?;
        crate::environment_copy::copy_roles(
            &source_url,
            &source_env,
            &request.database_url,
            &request.tenant,
            &wamn_catalog::ReleaseClosures::from_manifest(&snapshot.manifest),
        )
        .await
    }

    /// Step 13: the built web client of each package with a client package,
    /// under the digest of the head release.
    async fn upload_ui(&mut self) -> anyhow::Result<()> {
        use crate::web_upload::{
            ExistingObject, Sink, client_project, client_root, collect, release_hex, require_head,
            write_files,
        };
        let project_url = self.project_url().await?;
        let release: String = connect(&project_url)
            .await?
            .query_one(
                "SELECT manifest_digest FROM catalog.effective_release_heads \
                  WHERE tenant_id = $1 AND environment = $2",
                &[&self.tenant(), &self.request.env],
            )
            .await
            .context("read the manifest digest of the head release")?
            .get(0);
        require_head(&project_url, &release).await?;
        let hex = release_hex(&release)?.to_owned();
        let org = self.org.clone();
        let sink = Sink::parse(&self.config.ui_bucket)?;
        let store = sink.store()?;
        for package in self.packages().await? {
            let Some(client) = &package.manifest.client_package else {
                continue;
            };
            let project = client_project(&client.name)?;
            let dist = package.root().join("web/dist");
            let mut files = Vec::new();
            collect(&dist, &mut files)?;
            ensure!(
                files.iter().any(|file| file == &dist.join("index.html")),
                "the package artifact {} holds no web/dist/index.html",
                package.manifest.package.id
            );
            write_files(
                store.as_ref(),
                &client_root(sink.prefix, &package.manifest.package.id, &hex),
                &dist,
                &mut files,
                &crate::web_scope::scope_file_bytes(&org, project),
                ExistingObject::AcceptIdentical,
            )
            .await?;
        }
        Ok(())
    }

    /// Step 14: the rows of the current org and project administrators,
    /// again, for a grant made while the saga ran.
    async fn materialize_admin_grants(&mut self) -> anyhow::Result<()> {
        let mut client = connect(self.system_url()).await?;
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        let transaction =
            crate::provision_project_env::provisioning_transaction(&mut client).await?;
        wamn_platform_identity::org::materialize_admin_grants(
            &transaction,
            &self.org,
            &self.request.project,
            &self.request.env,
        )
        .await
        .context("give the environment the rows of the current administrators")?;
        transaction
            .commit()
            .await
            .context("commit the administrator rows")
    }

    /// Step 15: the commands that the operator runs. The saga never restarts
    /// the broker, identity or a host.
    fn operator_commands(&self) -> Value {
        let tenant = self.tenant();
        let (org, project, env) = (
            self.org.as_str(),
            self.request.project.as_str(),
            self.request.env.as_str(),
        );
        let secret =
            |family| wamn_control_provision::workload_secret_name(family, org, project, env);
        let packages: Vec<String> = self
            .request
            .packages
            .iter()
            .map(|package| format!("{}-{}", package.package_id, package.version))
            .collect();
        json!({
            "commands": [
                {
                    "purpose": "event NATS users of the environment, and a broker restart",
                    "runbook": "docs/operations/gcp.md section 5.4",
                    "commands": [
                        format!(
                            "cargo run -p wamn-test-infrastructure --example event_broker_files -- \
                             $P/evt nats://evt-nats.platform.svc.cluster.local:4222 {org} {project} \
                             {env} {tenant} {} <generated/wamn.json of {}>",
                            self.config.stream_replicas,
                            packages.join(", ")
                        ),
                        "merge the users of $P/evt/event-nats/authorization.conf into the Secret \
                         evt-nats-authorization, then restart the broker and run the tap-stream \
                         Job as in section 3.5".to_owned(),
                        format!(
                            "kubectl -n hosts create secret generic wamn-event-nats-{project} \
                             --from-file=username=$E/runtime-username \
                             --from-file=password=$E/runtime-password \
                             --from-literal=org={org} --from-literal=project={project} \
                             --from-literal=environment={env} \
                             --from-literal=stream_replicas={} \
                             --from-literal=dup_window_secs={}",
                            self.config.stream_replicas, self.config.dup_window_secs
                        ),
                        format!(
                            "kubectl -n hosts create secret generic wamn-materializer-nats-{project} \
                             --from-file=binding.json=$E/binding.json"
                        ),
                    ],
                },
                {
                    "purpose": "the CDC reader of the environment",
                    "runbook": "docs/operations/gcp.md sections 3.19 and 5.4",
                    "commands": [
                        format!(
                            "copy deploy/gcp/cdc-reader-wms.yaml for {org}/{project}/{env}, with \
                             the registry reader Secret {} and the CDC Secret {}",
                            secret(WorkloadRoleFamily::RegistryReader),
                            wamn_control_provision::project_env_cdc_secret_name(org, project, env)
                        ),
                        format!("kubectl apply -f <the copy for {project}>"),
                        format!(
                            "kubectl -n platform rollout status deploy/cdc-reader-{project} \
                             --timeout=180s"
                        ),
                    ],
                },
                {
                    "purpose": "the identity audience of the environment",
                    "runbook": "docs/operations/gcp.md section 3.10",
                    "commands": [
                        format!(
                            "add {} to sessionTargetSecrets in deploy/gcp/values-identity.yaml",
                            secret(WorkloadRoleFamily::SessionRoleReader)
                        ),
                        "helm upgrade identity deploy/platform/identity -n identity \
                         -f deploy/gcp/values-identity.yaml".to_owned(),
                        "kubectl -n identity rollout status deploy/identity --timeout=300s"
                            .to_owned(),
                    ],
                },
                {
                    "purpose": "the host workload of the environment",
                    "runbook": "docs/operations/gcp.md sections 3.11, 3.14, 3.20 and 5.5",
                    "commands": [
                        format!(
                            "add a host group for {project} to deploy/gcp/values-host.yaml that \
                             names the Secrets {}, {}, {}, {}, {} and {}, the selected \
                             release of {org}/{project}/{env}, and the variables \
                             WAMN_ORG={org}, WAMN_PROJECT={project}, \
                             WASMCLOUD_HOST_ENVIRONMENT={env} and WAMN_ROUTE_HOST={}",
                            secret(WorkloadRoleFamily::App),
                            secret(WorkloadRoleFamily::ExecutorPlatform),
                            secret(WorkloadRoleFamily::HttpAdmitter),
                            secret(WorkloadRoleFamily::EventMaterializer),
                            secret(WorkloadRoleFamily::IdentityReader),
                            secret(WorkloadRoleFamily::Administration),
                            self.request.route_host,
                        ),
                        "helm upgrade wamn-host oci://ghcr.io/wasmcloud/charts/runtime-operator \
                         --version 2.10.3 -n hosts -f deploy/gcp/values-host.yaml".to_owned(),
                        format!(
                            "apply the HTTP and materializer workloads of {project} as in \
                             section 5.5, then kubectl -n hosts rollout status \
                             deploy/hostgroup-{project} --timeout=300s"
                        ),
                    ],
                },
            ],
        })
    }
}

/// The `(package_id, component)` of the wiring node that gives `alias` as
/// its store alias.
fn alias_component(
    wirings: &[WiringInput],
    alias: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let mut found: Option<(String, String)> = None;
    for input in wirings {
        for (node, value) in &input.wiring.nodes {
            if value.params.get("store_alias").and_then(Value::as_str) != Some(alias) {
                continue;
            }
            let key = (input.package_id.to_string(), value.component.clone());
            match &found {
                Some(other) if other != &key => bail!(
                    "node {node:?} of wiring {:?} gives the store alias {alias} to {}::{}, and \
                     another node gives it to {}::{}",
                    input.wiring.wiring_id,
                    key.0,
                    key.1,
                    other.0,
                    other.1
                ),
                _ => found = Some(key),
            }
        }
    }
    Ok(found)
}

/// The `wamn-scenario-worker` child that gates each wiring, as
/// `upgrade-environment` runs it.
struct Gate {
    port: u16,
    http: reqwest::Client,
    _child: Child,
}

impl Gate {
    async fn start(
        config: &ServeConfig,
        private: &PrivateDirectory,
        (reader_url, control_author_url, management_admitter_url): &(String, String, String),
        [org, project, env, tenant]: [&str; 4],
    ) -> anyhow::Result<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let log = private.path("gate.log");
        let output = std::fs::File::create(&log)?;
        let child = Command::new(&config.scenario_worker)
            .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
            .env("WAMN_SYSTEM_URL", reader_url)
            .env("WAMN_CONTROL_AUTHORING_PG_URL", control_author_url)
            .env("WAMN_MANAGEMENT_ADMISSION_PG_URL", management_admitter_url)
            .env("WAMN_MANAGEMENT_ORG", org)
            .env("WAMN_MANAGEMENT_PROJECT", project)
            .env("WAMN_MANAGEMENT_ENVIRONMENT", env)
            .env("WAMN_MANAGEMENT_TENANT", tenant)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true)
            .spawn()
            .context("start the gate service")?;
        for _ in 0..60 {
            if std::fs::read_to_string(&log)?.contains("listening") {
                return Ok(Self {
                    port,
                    http: reqwest::Client::new(),
                    _child: child,
                });
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        bail!("the gate service did not listen within 60 s")
    }

    async fn gate(
        &self,
        pat: &str,
        input: &WiringInput,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<()> {
        use wamn_authoring_model::{
            AuthoringCommand, AuthoringDocument, AuthoringRequest, AuthoringRequestEnvelope,
            AuthoringScope, Gate, SCHEMA_VERSION,
        };
        let request = AuthoringDocument::Request(Box::new(AuthoringRequestEnvelope::Command(
            AuthoringRequest {
                schema_version: SCHEMA_VERSION.to_owned(),
                command_id: format!(
                    "gate-{}-{}-{}",
                    input.package_id, input.package_version, input.wiring.wiring_id
                ),
                command: AuthoringCommand::Gate(Gate {
                    scope: AuthoringScope {
                        project_id: project.to_owned(),
                        environment: environment.to_owned(),
                    },
                    package_id: input.package_id.to_string(),
                    package_version: input.package_version.to_string(),
                    document: input.document.clone(),
                }),
            },
        )));
        let reply: Value = self
            .http
            .post(format!("http://127.0.0.1:{}/authoring", self.port))
            .bearer_auth(pat)
            .json(&request)
            .send()
            .await
            .context("post the gate request")?
            .json()
            .await
            .context("read the gate reply")?;
        let outcome = &reply["body"]["outcome"];
        ensure!(
            outcome["status"] == "completed",
            "the gate of {} answered {}",
            input.wiring.wiring_id,
            outcome["status"]
        );
        Ok(())
    }
}

/// A private mode 0700 directory for the Secrets that a library function
/// writes only to a file. Each file is shredded after its apply, and the
/// directory goes on drop.
struct PrivateDirectory {
    root: PathBuf,
}

static PRIVATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl PrivateDirectory {
    fn create() -> anyhow::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "wamn-serve-{}-{}",
            std::process::id(),
            PRIVATE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .with_context(|| format!("create {}", root.display()))?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    async fn apply_and_shred(&self, file: &Path) -> anyhow::Result<()> {
        let bytes = std::fs::read(file).context("read a private Secret file")?;
        let applied = kubectl_apply(&bytes).await;
        shred(file)?;
        applied
    }

    async fn patch_and_shred(
        &self,
        file: &Path,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        let bytes = std::fs::read(file).context("read a private patch file")?;
        let patched = kubectl(
            &[
                "-n",
                namespace,
                "patch",
                "secret",
                name,
                "--type",
                "merge",
                "--patch-file",
                "/dev/stdin",
            ],
            Some(&bytes),
        )
        .await;
        shred(file)?;
        patched.map(|_| ())
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let _ = shred(&entry.path());
            }
        }
        let _ = std::fs::remove_dir(&self.root);
    }
}

/// Overwrite a file with zeros, then remove it.
fn shred(file: &Path) -> anyhow::Result<()> {
    let length = std::fs::metadata(file)
        .with_context(|| format!("read {}", file.display()))?
        .len();
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .open(file)
        .with_context(|| format!("open {}", file.display()))?;
    handle.write_all(&vec![0; usize::try_from(length)?])?;
    handle.sync_all()?;
    drop(handle);
    std::fs::remove_file(file).with_context(|| format!("remove {}", file.display()))
}

/// Run `kubectl`. A call that sends input never shows the standard error of
/// `kubectl`, because the input can be a Secret.
async fn kubectl(arguments: &[&str], input: Option<&[u8]>) -> anyhow::Result<String> {
    let mut child = Command::new("kubectl")
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("run kubectl")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("open the kubectl input")?;
        stdin
            .write_all(input)
            .await
            .context("write the kubectl input")?;
        drop(stdin);
    }
    let output = child.wait_with_output().await.context("wait for kubectl")?;
    let verb = arguments
        .iter()
        .find(|argument| !argument.starts_with('-') && **argument != "-n")
        .copied()
        .unwrap_or("kubectl");
    if !output.status.success() {
        if input.is_some() {
            bail!("kubectl {verb} failed: {}", output.status);
        }
        bail!(
            "kubectl {} failed: {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("kubectl wrote text")?
        .trim()
        .to_owned())
}

async fn kubectl_apply(document: &[u8]) -> anyhow::Result<()> {
    kubectl(&["apply", "-f", "-"], Some(document))
        .await
        .map(|_| ())
}

/// Wait for a CloudNativePG CR in `platform` to report `status.applied`.
async fn wait_applied(kind: &str, name: &str) -> anyhow::Result<()> {
    kubectl(
        &[
            "-n",
            PLATFORM_NAMESPACE,
            "wait",
            &format!("{kind}/{name}"),
            "--for=jsonpath={.status.applied}=true",
            APPLIED_TIMEOUT,
        ],
        None,
    )
    .await
    .map(|_| ())
}

/// The manifest digest of the head release of `environment`, if it has one.
async fn head_release(
    project_url: &str,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<Option<String>> {
    Ok(connect(project_url)
        .await?
        .query_opt(
            "SELECT manifest_digest FROM catalog.effective_release_heads \
              WHERE tenant_id = $1 AND environment = $2",
            &[&tenant, &environment],
        )
        .await
        .context("read the head release")?
        .map(|row| row.get(0)))
}

/// One key of a Secret, decoded.
async fn read_secret_key(namespace: &str, name: &str, key: &str) -> anyhow::Result<String> {
    use base64::Engine as _;
    let encoded = kubectl(
        &[
            "-n",
            namespace,
            "get",
            "secret",
            name,
            "-o",
            &format!("jsonpath={{.data.{key}}}"),
        ],
        None,
    )
    .await?;
    ensure!(!encoded.is_empty(), "the Secret {name} holds no {key}");
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .with_context(|| format!("decode {key} of the Secret {name}"))?,
    )
    .with_context(|| format!("{key} of the Secret {name} is text"))
}

/// A replication password: 32 random bytes in hexadecimal, as the runbook
/// makes it with `openssl rand -hex 32`.
fn random_password() -> anyhow::Result<String> {
    use ring::rand::SecureRandom as _;
    let mut random = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate the replication password"))?;
    Ok(hex::encode(random))
}

#[cfg(test)]
mod tests {
    use super::step_error;

    #[test]
    fn a_step_error_keeps_its_context_chain() {
        let error = anyhow::anyhow!("the run plane refused").context("reconcile the run plane");
        assert_eq!(
            step_error(&error),
            "reconcile the run plane: the run plane refused"
        );
    }
}
