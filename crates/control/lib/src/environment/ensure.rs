//! Step 4 of `apply`: ensure the substrate (docs/plan/platform-deploy.md
//! §10.1 step 4, R6 (3) and (4)).
//!
//! Each step is an ensure, judged by what it observes: the environment row
//! with a fresh instance suffix only if there is none, the CNPG `Database` CR
//! until it reports `status.applied`, the project database floor, the run
//! plane, the credential Secrets (each written only when absent, the PAT
//! reused when present), and the admin grants. CDC is ensured by
//! [`ensure_cdc`] once the release's packages are installed, because the
//! capture schema is a package fact. On an environment that has all of this,
//! every ensure is a read.
//!
//! The verb writes the credential Secrets and owns them. A Secret reaches
//! `kubectl apply -f -` on its standard input; a library function that writes
//! a Secret only to a file writes it into a private mode 0700 directory, and
//! the file is shredded after the apply.

use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wamn_control_registry::Triple;

use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;
use super::{EventBroker, Platform};
use crate::provision_project_env::{
    ProvisionProjectEnvRequest, WorkloadActionRequest, WorkloadActionVerb,
    WorkloadGenerationAction, provision_project_env, provisioning_transaction,
    read_project_env_instance, resolve_cluster, run_workload_action,
};

/// The run-plane schema of every environment.
pub(super) const RUN_SCHEMA: &str = "wamn_run";

/// How long one CR may take to report `status.applied`.
const APPLIED_TIMEOUT: &str = "--timeout=120s";

/// The credential families the host of the release chart reads, and whether
/// each credential reaches the project database. Every Secret is named from
/// the coordinate (`workload_secret_name`, §9.1).
pub(super) const HOST_FAMILIES: [(WorkloadRoleFamily, bool); 6] = [
    (WorkloadRoleFamily::App, true),
    (WorkloadRoleFamily::ExecutorPlatform, true),
    (WorkloadRoleFamily::HttpAdmitter, true),
    (WorkloadRoleFamily::EventMaterializer, true),
    (WorkloadRoleFamily::IdentityReader, false),
    (WorkloadRoleFamily::Administration, true),
];

/// Ensure the substrate of the document's environment.
///
/// # Errors
///
/// When an ensure fails. A rerun repairs a partial run.
pub async fn ensure(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
) -> anyhow::Result<()> {
    let triple = document.triple();
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    // The row's policy until step 5 writes the declared one; a first apply
    // creates the row with the declared policy.
    let policy = analysis
        .authorities
        .row
        .as_ref()
        .map_or(document.policy.as_str(), |row| row.policy_name.as_str());
    let cluster = resolve_cluster(&platform.system_database_url, &document.org, policy).await?;
    let private = PrivateDirectory::create()?;
    let namespace = platform.target.namespace.as_str();

    // The row, the role posture, the Database CR, the privilege floor, the
    // database Secret, and the management-author PAT when absent.
    let pat_name = management_author_pat_name(&triple);
    let issue_pat = platform.pat_issuer.endpoint.is_some()
        && !secret_exists(platform, namespace, &pat_name).await?;
    let database_secret = private.path("database.json");
    let pat_secret = private.path("management-author-pat.json");
    let outcome = provision_project_env(&ProvisionProjectEnvRequest {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        policy: Some(policy.to_owned()),
        disposable: false,
        system_database_url: Some(platform.system_database_url.clone()),
        cluster: Some(cluster.clone()),
        connection_limit: None,
        cluster_namespace: platform.database_namespace.clone(),
        namespace: namespace.to_owned(),
        secret_namespace: Some(namespace.to_owned()),
        // `-` returns these three in the outcome and writes no file.
        emit_database: Some(PathBuf::from("-")),
        emit_role_sql: Some(PathBuf::from("-")),
        emit_privilege_sql: Some(PathBuf::from("-")),
        emit_secret: Some(database_secret.clone()),
        pat_issuer: platform.pat_issuer.clone(),
        emit_management_author_pat_secret: issue_pat.then(|| pat_secret.clone()),
        emit_operator_pat_secret: None,
    })
    .await
    .context("record the environment and render its database")?;
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute(&outcome.role_sql)
        .await
        .context("ensure the database roles")?;
    kubectl(
        platform,
        &platform.database_namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&outcome.database_cr)?),
    )
    .await
    .context("apply the Database CR")?;
    wait_applied(platform, "database", &outcome.database).await?;
    system
        .batch_execute(&outcome.privilege_sql)
        .await
        .context("ensure the database privilege floor")?;
    private.apply_and_shred(platform, &database_secret).await?;
    if issue_pat {
        private.apply_and_shred(platform, &pat_secret).await?;
    }

    // The run plane: the run schema the credentials are granted on.
    let project_url = project_url(platform, &triple).await?;
    crate::reconcile_run_plane::reconcile_run_plane(
        crate::reconcile_run_plane::ReconcileRunPlaneRequest {
            system_database_url: platform.system_database_url.clone(),
            admin_database_url: project_url.clone(),
            org: document.org.clone(),
            project: document.project.clone(),
            tenant: tenant.clone(),
            env: document.env.clone(),
            schema: RUN_SCHEMA.to_owned(),
            dry_run: false,
        },
    )
    .await
    .context("reconcile the run plane")?;

    // Generation a of each host credential, only where its Secret is absent.
    for (family, project_database) in HOST_FAMILIES {
        let name = wamn_control_provision::workload_secret_name(
            family,
            &document.org,
            &document.project,
            &document.env,
        );
        if secret_exists(platform, namespace, &name).await? {
            continue;
        }
        let secret = private.path(&format!("{}.json", family.label()));
        let patch = (family == WorkloadRoleFamily::Administration)
            .then(|| private.path("control-administration-patch.json"));
        run_workload_action(&WorkloadActionRequest {
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
            tenant: Some(tenant.clone()),
            system_database_url: Some(platform.system_database_url.clone()),
            target_admin_database_url: project_database.then(|| project_url.clone()),
            cluster: Some(cluster.clone()),
            db_host: platform.database_host.clone(),
            db_port: platform.database_port,
            namespace: namespace.to_owned(),
            action: WorkloadGenerationAction {
                family,
                verb: WorkloadActionVerb::Prepare,
                generation: CredentialGeneration::A,
            },
            secret: Some(secret.clone()),
            emit_role_sql: None,
            control_administration_patch: patch.clone(),
        })
        .await
        .with_context(|| format!("prepare generation a of {}", family.label()))?;
        private.apply_and_shred(platform, &secret).await?;
        // The org's control host holds every environment's administration
        // login. Where the platform runs no control host, there is nothing
        // to patch.
        if let Some(patch) = patch {
            let control = wamn_control_provision::control_administration_secret_name(&document.org);
            if secret_exists(platform, namespace, &control).await? {
                private
                    .patch_and_shred(platform, &patch, namespace, &control)
                    .await?;
            }
        }
    }

    // The rows of the current org and project administrators.
    let mut client = connect(&platform.system_database_url).await?;
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let transaction = provisioning_transaction(&mut client).await?;
    wamn_platform_identity::org::materialize_admin_grants(
        &transaction,
        &document.org,
        &document.project,
        &document.env,
    )
    .await
    .context("give the environment the rows of the current administrators")?;
    transaction
        .commit()
        .await
        .context("commit the administrator rows")
}

/// Ensure CDC capture of the packages' model schema: the source stream, the
/// replication role, the slot, the `Publication` CR and the replication
/// Secret. A recorded CDC reader is the observation that it exists; a package
/// set with no model schema captures nothing.
///
/// # Errors
///
/// When the packages declare several schemas, the event broker is not
/// configured, or a write fails.
pub(super) async fn ensure_cdc(
    platform: &Platform,
    document: &EnvironmentDocument,
    manifests: &[wamn_schema_generator::PackageManifest],
) -> anyhow::Result<()> {
    let schemas: std::collections::BTreeSet<&str> = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values())
        .map(|model| model.schema.as_str())
        .collect();
    let schema = match schemas.into_iter().collect::<Vec<_>>().as_slice() {
        [] => return Ok(()),
        [schema] => (*schema).to_owned(),
        several => bail!(
            "the release's packages declare the schemas {several:?}; one environment captures \
             one schema"
        ),
    };
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let recorded = system
        .query_opt(
            "SELECT 1 FROM registry.event_readers WHERE org = $1 AND project = $2 AND env = $3",
            &[&document.org, &document.project, &document.env],
        )
        .await
        .context("read the CDC reader registration")?
        .is_some();
    if recorded {
        return Ok(());
    }
    let EventBroker {
        nats_url,
        nats_username,
        nats_password_file,
        stream_replicas,
        dup_window_secs,
    } = platform.events.clone().context(
        "the release's packages declare a model schema and the environment has no CDC reader; \
         set WAMN_EVENT_NATS_URL, WAMN_EVENT_NATS_USERNAME and WAMN_EVENT_NATS_PASSWORD_FILE",
    )?;
    let triple = document.triple();
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let consumers =
        wamn_control_provision::events::registration_consumers(&triple, &tenant, manifests)
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
    let namespace = platform.target.namespace.as_str();
    let outcome = crate::enable_cdc_project_env::enable_cdc_project_env(
        &crate::enable_cdc_project_env::EnableCdcProjectEnvRequest {
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
            schema,
            system_database_url: Some(platform.system_database_url.clone()),
            cluster: None,
            replication_password: random_password()?,
            db_host: platform.database_host.clone(),
            db_port: platform.database_port,
            namespace: namespace.to_owned(),
            cluster_namespace: platform.database_namespace.clone(),
            secret_namespace: Some(namespace.to_owned()),
            stream: None,
            nats_url,
            nats_username,
            nats_password_file,
            stream_replicas,
            dup_window_secs,
            consumer_config: consumers,
            emit_role_sql: None,
            emit_cdc_sql: None,
            emit_publication: None,
            emit_secret: None,
        },
    )
    .await
    .context("enable CDC")?;
    system
        .batch_execute(&outcome.role_sql)
        .await
        .context("run the replication role SQL")?;
    connect(&project_url(platform, &triple).await?)
        .await?
        .batch_execute(&outcome.cdc_sql)
        .await
        .context("run the CDC SQL")?;
    kubectl(
        platform,
        &platform.database_namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&outcome.publication)?),
    )
    .await
    .context("apply the Publication CR")?;
    kubectl(
        platform,
        namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&outcome.secret)?),
    )
    .await
    .context("apply the replication Secret")?;
    let database = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        &read_project_env_instance(&platform.system_database_url, &triple).await?,
    );
    wait_applied(platform, "publication", &database).await
}

/// The URL of the environment's project database: the system URL's login on
/// the database the registry row names.
pub(super) async fn project_url(platform: &Platform, triple: &Triple) -> anyhow::Result<String> {
    let instance = read_project_env_instance(&platform.system_database_url, triple).await?;
    let mut url = url::Url::parse(&platform.system_database_url).context("parse the system URL")?;
    url.set_path(&format!(
        "/{}",
        wamn_control_provision::project_env_database_name(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            &instance
        )
    ));
    Ok(url.to_string())
}

/// `wamn-pat-management-author-<org>--<project>--<env>`.
fn management_author_pat_name(triple: &Triple) -> String {
    format!(
        "wamn-pat-management-author-{}--{}--{}",
        triple.org, triple.project, triple.env
    )
}

/// Whether the Secret `name` exists in `namespace`.
pub(super) async fn secret_exists(
    platform: &Platform,
    namespace: &str,
    name: &str,
) -> anyhow::Result<bool> {
    let listed = kubectl(
        platform,
        namespace,
        &["get", "secret", name, "--ignore-not-found", "-o", "name"],
        None,
    )
    .await?;
    Ok(!listed.is_empty())
}

/// Wait for a CloudNativePG CR to report `status.applied`.
async fn wait_applied(platform: &Platform, kind: &str, name: &str) -> anyhow::Result<()> {
    kubectl(
        platform,
        &platform.database_namespace,
        &[
            "wait",
            &format!("{kind}/{name}"),
            "--for=jsonpath={.status.applied}=true",
            APPLIED_TIMEOUT,
        ],
        None,
    )
    .await
    .map(drop)
    .with_context(|| format!("wait for {kind} {name} to report status.applied"))
}

/// Run `kubectl` against the platform's cluster in `namespace`. A call that
/// sends input never shows `kubectl`'s standard error, because the input can
/// be a Secret.
pub(super) async fn kubectl(
    platform: &Platform,
    namespace: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> anyhow::Result<String> {
    let mut child = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(&platform.target.kubeconfig)
        .args(["--context", &platform.target.context, "-n", namespace])
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
        .context("start kubectl")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("open the kubectl input")?;
        stdin
            .write_all(input)
            .await
            .context("write the kubectl input")?;
        drop(stdin);
    }
    let output = child.wait_with_output().await.context("wait for kubectl")?;
    if !output.status.success() {
        if input.is_some() {
            bail!(
                "kubectl {} exited {}",
                arguments.first().copied().unwrap_or_default(),
                output.status
            );
        }
        bail!(
            "kubectl {} exited {}: {}",
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

/// A replication password: 32 random bytes in hexadecimal.
fn random_password() -> anyhow::Result<String> {
    use ring::rand::SecureRandom as _;
    let mut random = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate the replication password"))?;
    Ok(hex::encode(random))
}

/// A private mode 0700 directory for the Secrets a library function writes
/// only to a file. Each file is shredded after its apply, and the directory
/// goes on drop.
pub(super) struct PrivateDirectory {
    root: PathBuf,
}

static PRIVATE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl PrivateDirectory {
    pub(super) fn create() -> anyhow::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "wamn-env-apply-{}-{}",
            std::process::id(),
            PRIVATE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .with_context(|| format!("create {}", root.display()))?;
        Ok(Self { root })
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    async fn apply_and_shred(&self, platform: &Platform, file: &Path) -> anyhow::Result<()> {
        let bytes = std::fs::read(file).context("read a private Secret file")?;
        let applied = kubectl(
            platform,
            &platform.target.namespace,
            &["apply", "-f", "-"],
            Some(&bytes),
        )
        .await;
        shred(file)?;
        applied.map(drop)
    }

    async fn patch_and_shred(
        &self,
        platform: &Platform,
        file: &Path,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        let bytes = std::fs::read(file).context("read a private patch file")?;
        let patched = kubectl(
            platform,
            namespace,
            &[
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
        patched.map(drop)
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
