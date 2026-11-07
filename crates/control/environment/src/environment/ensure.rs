//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

pub(super) mod plan;
pub use plan::credential_access;
use plan::credential_request;
pub(super) use plan::management_author_pat_name;

use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use base64::Engine as _;
use wamn_control_provision::host_credentials::{
    CredentialDatabase, HOST_CREDENTIALS, HostCredential,
};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wamn_control_registry::Triple;

use super::Platform;
use super::document::EnvironmentDocument;
use crate::provision_project_env::{
    ProvisionProjectEnvRequest, WorkloadActionRequest, WorkloadActionVerb,
    WorkloadGenerationAction, provision_project_env, provisioning_transaction,
    read_project_env_instance, resolve_cluster,
};
use wamn_package_control::database::connect;

/// The run-plane schema of every environment.
pub(super) const RUN_SCHEMA: &str = "wamn_run";

/// How long one CR may take to report `status.applied`.
const APPLIED_TIMEOUT: &str = "--timeout=120s";

/// Provision shared resources for the document's environment.
///
/// # Errors
///
/// When an ensure fails. A rerun repairs a partial run.
pub async fn provision_resources(
    platform: &Platform,
    document: &EnvironmentDocument,
    packages: &[wamn_catalog::manifest::PackageManifest],
    admin_url: &str,
) -> anyhow::Result<()> {
    platform
        .events
        .as_ref()
        .context("environment provisioning requires the event broker")?;
    let admin = connect(admin_url).await?;
    let triple = document.triple();
    let policy = document.policy.as_str();
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
    let outcome = provision(
        platform,
        document,
        policy,
        admin_url,
        &database_secret,
        issue_pat.then_some(pat_secret.as_path()),
    )
    .await?;
    ensure_credential_access(platform, document).await?;
    kubectl(
        platform,
        &platform.database_namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&outcome.database_cr)?),
    )
    .await
    .context("apply the Database CR")?;
    wait_applied(platform, "database", &outcome.database).await?;
    admin
        .batch_execute(&outcome.privilege_sql)
        .await
        .context("ensure the database privilege floor")?;
    private.apply_and_shred(platform, &database_secret).await?;
    if issue_pat {
        private.apply_and_shred(platform, &pat_secret).await?;
    }

    // The run plane: the run schema the credentials are granted on.
    let project_admin =
        ensure_project_database(platform, document, packages, &admin, admin_url).await?;

    // Generation a of each credential, only where its Secret is absent.
    for credential in HOST_CREDENTIALS {
        let name = wamn_control_provision::workload_secret_name(
            credential.family,
            &document.org,
            &document.project,
            &document.env,
        );
        if secret_exists(platform, namespace, &name).await? {
            continue;
        }
        let request = credential_request(
            credential,
            platform,
            document,
            &private,
            &cluster,
            &project_admin,
        );
        crate::provision_project_env::workload::run_workload_action_with_admin(&request, admin_url)
            .await
            .with_context(|| format!("prepare generation a of {}", credential.family.label()))?;
        private
            .apply_and_shred(platform, &private.path(credential.file_name))
            .await?;
        // The org's control host holds every environment's administration
        // login. Where the platform runs no control host, there is nothing
        // to patch.
        if let Some(patch) = request.control_administration_patch {
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
    let transaction = provisioning_transaction(&mut client).await?;
    wamn_platform_identity::org::materialize_admin_grants(
        &transaction,
        &document.org,
        &document.project,
        &document.env,
    )
    .await
    .context("give the environment the rows of the current administrators")?;
    let mut drops = wamn_platform_identity::auth_changes::AuthDrops::default();
    for row in transaction
        .query(
            "SELECT principal_id::text FROM identity.project_roles WHERE org=$1 AND project=$2",
            &[&document.org, &document.project],
        )
        .await?
    {
        drops.identity(row.get(0));
    }
    transaction
        .commit()
        .await
        .context("commit the administrator rows")?;
    platform.auth_changes().await?.publish(drops).await;
    let authorization = crate::kubectl::get::<serde_json::Value>(
        &platform.target.kubeconfig,
        &platform.target.context,
        "platform",
        "secret",
        crate::platform::broker::AUTHORIZATION,
    )
    .await?
    .context("missing event broker authorization Secret")?;
    for mut object in crate::platform::broker::environment_objects(&authorization, document)? {
        let object_namespace = if object["kind"] == "Certificate" {
            namespace
        } else {
            "platform"
        };
        object["metadata"]["namespace"] = object_namespace.into();
        crate::kubectl::apply(
            &platform.target.kubeconfig,
            &platform.target.context,
            object_namespace,
            &object,
        )
        .await?;
    }
    prepare_cdc(platform, document, &admin, &project_admin).await?;
    ensure_streams(platform, document, &[]).await?;
    provision_materializer(platform, document).await?;
    Ok(())
}

/// Observe the resources at the end of provisioning, not merely its first row.
/// The materializer is installed only after credentials, the database, and CDC
/// preparation succeed. An interrupted prefix therefore remains repairable.
pub(super) async fn resources_in_place(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<bool> {
    let system = connect(&platform.system_database_url).await?;
    if super::analyse::read_row(&system, document).await?.is_none() {
        return Ok(false);
    }
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    if !super::analyse::release_present(platform, &name).await? {
        return Ok(false);
    }
    let workload: Option<serde_json::Value> = crate::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        "workloaddeployment",
        &format!("{name}-materializer"),
    )
    .await?;
    // A transient readiness failure does not authorize another SQL operation.
    // The only remaining provisioning action after this object is a readiness wait.
    Ok(workload.is_some())
}

pub(super) fn environment_host_name(org: &str, project: &str, env: &str) -> anyhow::Result<String> {
    Ok(wamn_control_provision::environment_state::environment_resource_name(org, project, env)?)
}

/// Render the shared host without application release arguments or labels.
pub fn materializer_host_values(
    document: &EnvironmentDocument,
    drain_bound_seconds: u32,
    stream_replicas: usize,
    dup_window_secs: u64,
    host_group: Option<&Path>,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    use wamn_release_control::release_chart;
    anyhow::ensure!(
        drain_bound_seconds > release_chart::MATERIALIZER_DRAIN_BOUND_SECONDS,
        "the environment host drain bound must exceed the materializer drain bound"
    );
    let mut group = serde_json::to_value(super::write::host_group(
        host_group,
        &document.org,
        &document.project,
        &document.env,
    )?)?;
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    for field in [
        "name",
        "namespace",
        "service",
        "extraArgs",
        "image",
        "plugins",
    ] {
        anyhow::ensure!(
            group.get(field).is_none(),
            "the environment host group sets derived field {field}"
        );
    }
    group["name"] = name.into();
    group["extraArgs"] = json!([format!("--environment={}", document.env)]);
    group["plugins"] = serde_json::to_value(release_chart::events_plugin(
        &document.org,
        &document.project,
        &document.env,
    )?)?;
    let variables = group["env"]
        .as_array_mut()
        .context("host env is not a list")?;
    for (key, value) in [
        ("WAMN_EVT_ORG", document.org.clone()),
        ("WAMN_EVT_PROJECT", document.project.clone()),
        ("WAMN_EVT_ENV", document.env.clone()),
        ("WAMN_EVT_STREAM_REPLICAS", stream_replicas.to_string()),
        ("WAMN_EVT_DUP_WINDOW_SECS", dup_window_secs.to_string()),
    ] {
        if let Some(variable) = variables.iter().find(|variable| variable["name"] == key) {
            anyhow::ensure!(
                variable["value"] == value,
                "the environment host group overrides {key}"
            );
        } else {
            variables.push(json!({"name":key,"value":value}));
        }
    }
    for (key, value) in [
        ("WAMN_ORG", document.org.clone()),
        ("WAMN_PROJECT", document.project.clone()),
        ("WAMN_DRAIN_BOUND_SECONDS", drain_bound_seconds.to_string()),
        (
            "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS",
            release_chart::MATERIALIZER_DRAIN_BOUND_SECONDS.to_string(),
        ),
    ]
    .into_iter()
    .chain(release_chart::TELEMETRY_VARIABLES.map(|(key, value)| (key, value.to_owned())))
    {
        anyhow::ensure!(
            !variables.iter().any(|variable| variable["name"] == key),
            "the environment host group sets derived variable {key}"
        );
        variables.push(json!({"name":key,"value":value}));
    }
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    Ok(json!({
        "roles": [],
        "runtime-operator": {"runtime": {
            "terminationGracePeriodSeconds": drain_bound_seconds,
            "serviceAccount": {"create":false, "name": wamn_credential_store::kubernetes::service_account_name(&document.org, &document.project, &document.env)},
            "automountServiceAccountToken": true,
            "hostGroups": [group]
        }},
        "global": {"certificates": {"dataCertSecretName": format!("wamn-nats-{env_key}")}}
    }))
}

/// Render the environment materializer on its stable host group.
pub fn materializer_workload(
    document: &EnvironmentDocument,
    namespace: &str,
    image: &str,
) -> anyhow::Result<serde_json::Value> {
    use wamn_control_provision::materializer_workload::{self, EventIdentity, MaterializerInput};
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    let rendered = materializer_workload::render_materializer(
        include_str!("../../../../../deploy/platform/materializer.example.yaml"),
        &MaterializerInput {
            workload: format!("{name}-materializer"),
            namespace: namespace.to_owned(),
            image: image.to_owned(),
            env_key: wamn_control_provision::project_env_key(
                &document.org,
                &document.project,
                &document.env,
            ),
            event: EventIdentity {
                org: document.org.clone(),
                project: document.project.clone(),
                environment: document.env.clone(),
            },
            event_stream: wamn_control_provision::event_stream_name(
                &document.org,
                &document.project,
                &document.env,
            ),
            fetch_ms: materializer_workload::FETCH_MS,
            sweep_ms: materializer_workload::SWEEP_MS,
        },
    )?;
    let mut workload: serde_json::Value = serde_yaml::from_str(&rendered)?;
    workload["spec"]["template"]["spec"]["hostSelector"] = serde_json::json!({"hostgroup":name});
    Ok(workload)
}

async fn provision_materializer(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<()> {
    use wamn_release_control::release_chart;
    let system = connect(&platform.system_database_url).await?;
    let policy = super::analyse::read_policy(&system, &document.org, &document.policy).await?;
    let events = platform
        .events
        .as_ref()
        .context("environment provisioning requires the event broker")?;
    let desired = materializer_host_values(
        document,
        u32::try_from(policy.drain_bound_seconds)?,
        events.stream_replicas,
        events.dup_window_secs,
        platform.host_group.as_deref(),
    )?;
    let private = PrivateDirectory::create()?;
    let values = private.path("environment-host.yaml");
    std::fs::write(&values, serde_yaml::to_string(&desired)?)?;
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    release_chart::recover_pending(&platform.target, &name).await?;
    release_chart::upgrade(
        &platform.target,
        &platform.chart,
        &name,
        &values,
        std::time::Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?),
        "environment provisioning",
    )
    .await?;
    let image = release_chart::image_set(&platform.chart)
        .await?
        .materializer;
    let workload = materializer_workload(document, &platform.target.namespace, &image)?;
    crate::kubectl::apply(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        &workload,
    )
    .await
    .context("apply the environment materializer")?;
    kubectl(
        platform,
        &platform.target.namespace,
        &[
            "wait",
            "--for=condition=Ready",
            &format!("workloaddeployment/{name}-materializer"),
            &format!("--timeout={}s", policy.readiness_budget_seconds),
        ],
        None,
    )
    .await
    .context("wait for the environment materializer")?;
    Ok(())
}

/// Record the environment and create roles before CNPG creates its database.
/// The caller applies the returned Database resource and the protected Secret output.
pub async fn provision(
    platform: &Platform,
    document: &EnvironmentDocument,
    policy: &str,
    admin_url: &str,
    secret: &Path,
    pat_secret: Option<&Path>,
) -> anyhow::Result<crate::provision_project_env::ProvisionProjectEnvOutcome> {
    let outcome = provision_project_env(&ProvisionProjectEnvRequest {
        changes: platform.auth_changes().await?,
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        policy: Some(policy.to_owned()),
        target: document.target.clone(),
        disposable: None,
        system_database_url: Some(platform.system_database_url.clone()),
        cluster: None,
        connection_limit: None,
        cluster_namespace: platform.database_namespace.clone(),
        namespace: platform.target.namespace.clone(),
        secret_namespace: Some(platform.target.namespace.clone()),
        // `-` returns these three in the outcome and writes no file.
        emit_database: Some(PathBuf::from("-")),
        emit_role_sql: Some(PathBuf::from("-")),
        emit_privilege_sql: Some(PathBuf::from("-")),
        emit_secret: Some(secret.to_path_buf()),
        pat_issuer: platform.pat_issuer.clone(),
        emit_management_author_pat_secret: pat_secret.map(Path::to_path_buf),
        emit_operator_pat_secret: None,
    })
    .await
    .context("record the environment and render its database")?;
    connect(admin_url)
        .await?
        .batch_execute(&outcome.role_sql)
        .await
        .context("ensure the database roles")?;
    Ok(outcome)
}

/// Ensure the project database grants and run plane after its database exists.
///
/// The system URL uses `wamn_apply`. The administrator installs extensions,
/// grants owner memberships, and reconciles with statements as the database owner.
/// Returns the project administrator URL for credential preparation.
///
/// # Errors
///
/// Returns an error if a database operation fails.
pub async fn ensure_project_database(
    platform: &Platform,
    document: &EnvironmentDocument,
    packages: &[wamn_catalog::manifest::PackageManifest],
    admin: &tokio_postgres::Client,
    admin_url: &str,
) -> anyhow::Result<String> {
    let project_url = project_url(platform, &document.triple()).await?;
    let mut project_admin = url::Url::parse(admin_url)?;
    project_admin.set_path(url::Url::parse(&project_url)?.path());
    let project_admin = project_admin.to_string();
    connect(&project_admin)
        .await?
        .batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
        .await?;
    admin
        .batch_execute(&wamn_control_provision::sql::ensure_platform_group_role_sql())
        .await?;
    admin.batch_execute(&format!(
        "GRANT CONNECT ON DATABASE {} TO wamn_apply; GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE;",
        wamn_pg_core::quote_ident(url::Url::parse(&project_url)?.path().trim_start_matches('/')),
    )).await?;
    for package in packages {
        admin
            .batch_execute(&wamn_control_provision::sql::ensure_package_owner_role_sql(
                &package.package.id,
            ))
            .await?;
        admin
            .batch_execute(&format!(
                "GRANT {} TO wamn_db_owner WITH ADMIN FALSE, INHERIT TRUE, SET TRUE",
                wamn_pg_core::quote_ident(&wamn_control_provision::package_owner_role(
                    &package.package.id
                ))
            ))
            .await?;
    }
    crate::reconcile_run_plane::reconcile_run_plane(
        crate::reconcile_run_plane::ReconcileRunPlaneRequest {
            system_database_url: platform.system_database_url.clone(),
            admin_database_url: project_admin.clone(),
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
            schema: RUN_SCHEMA.to_owned(),
            dry_run: false,
            changes: platform.auth_changes().await?,
        },
    )
    .await
    .context("reconcile the run plane")?;
    Ok(project_admin)
}

/// Prepare replication authority before the admin connection leaves ensure.
async fn prepare_cdc(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin: &tokio_postgres::Client,
    project_admin_url: &str,
) -> anyhow::Result<()> {
    let project_admin = connect(project_admin_url).await?;
    let instance =
        read_project_env_instance(&platform.system_database_url, &document.triple()).await?;
    let role = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let namespace = platform.target.namespace.as_str();
    let secret_name = wamn_control_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    let replication_password = match crate::kubectl::get::<serde_json::Value>(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        "secret",
        &secret_name,
    )
    .await?
    {
        Some(secret) => {
            let encoded = secret["data"]["url"]
                .as_str()
                .context("the CDC Secret has no URL")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .context("decode the CDC credential")?;
            let url = std::str::from_utf8(&bytes).context("decode the CDC URL")?;
            let config: tokio_postgres::Config = url.parse().context("parse the CDC URL")?;
            String::from_utf8(
                config
                    .get_password()
                    .context("the CDC URL has no password")?
                    .to_vec(),
            )
            .context("decode the replication password")?
        }
        None => random_password()?,
    };
    admin
        .batch_execute(&wamn_control_provision::sql::ensure_replication_role_sql(
            &role,
            &replication_password,
        ))
        .await?;
    project_admin
        .batch_execute(&wamn_control_provision::sql::create_failover_slot_sql(
            &role,
        ))
        .await?;
    let cluster = resolve_cluster(
        &platform.system_database_url,
        &document.org,
        &document.policy,
    )
    .await?;
    let database = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let mut url = url::Url::parse(&platform.system_database_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("encode replication user"))?;
    url.set_password(Some(&replication_password))
        .map_err(|()| anyhow::anyhow!("encode replication password"))?;
    url.set_host(Some(
        platform
            .database_host
            .as_deref()
            .unwrap_or(&format!("{cluster}-rw")),
    ))?;
    url.set_port(Some(platform.database_port))
        .map_err(|()| anyhow::anyhow!("encode replication port"))?;
    url.set_path(&database);
    let secret = wamn_control_provision::render_project_env_cdc_secret_manifest(
        &document.triple(),
        &instance,
        namespace,
        url.as_str(),
    );
    kubectl(
        platform,
        namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&secret)?),
    )
    .await?;
    Ok(())
}

/// Read every installed baseline, independent of the selected application release.
pub async fn installed_manifests(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_catalog::manifest::PackageManifest>> {
    let mut manifests = Vec::new();
    for (id, baseline) in super::stage::installed(platform, document).await? {
        let package = super::stage::open(platform, &id, &baseline.current).await?;
        let path = wamn_schema_generator::package_manifest_path(package.root());
        manifests.push(wamn_catalog::manifest::PackageManifest::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )?);
    }
    Ok(manifests)
}

/// Ensure capture of every model in the environment's installed applications.
/// The slot and publication belong to the environment, as do its entity consumers.
///
/// # Errors
///
/// When the event broker is absent or a substrate write fails.
pub async fn ensure_cdc(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<()> {
    let manifests = installed_manifests(platform, document).await?;
    let schemas = manifests
        .iter()
        .flat_map(|manifest| manifest.models.values().map(|model| model.schema.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if schemas.is_empty() {
        return ensure_streams(platform, document, &manifests).await;
    }
    let triple = document.triple();
    let instance = read_project_env_instance(&platform.system_database_url, &triple).await?;
    let cdc_name = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let project = super::analyse::connect_project(&project_url(platform, &triple).await?).await?;
    let mut full = project
        .query(
            wamn_schema_control::select_replica_identity_sql(),
            &[&schemas],
        )
        .await
        .context("read captured replica identities")?
        .into_iter()
        .filter(|row| row.get::<_, String>(2) == "f")
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<std::collections::BTreeSet<_>>();
    for manifest in &manifests {
        for model in manifest.models.values() {
            if !full.insert((model.schema.clone(), model.table.clone())) {
                continue;
            }
            let sql = wamn_schema_control::alter_replica_identity_sql(
                &model.schema,
                &model.table,
                wamn_schema_control::ReplicaIdentity::Full,
            );
            project.batch_execute(&sql).await.with_context(|| {
                format!(
                    "capture {}.{} with full row images",
                    model.schema, model.table
                )
            })?;
        }
    }
    if !cdc_in_place(&project, &cdc_name, &schemas).await? {
        enable_cdc(platform, document, &schemas, &project, &instance).await?;
    }
    ensure_streams(platform, document, &manifests).await?;
    let namespace = platform.target.namespace.as_str();
    crate::enable_cdc_project_env::record_event_reader(
        &platform.system_database_url,
        &triple,
        &schemas,
        &cdc_name,
        Some(namespace),
    )
    .await
    .context("record the CDC reader")
}

/// Provision the environment's source and advisory streams, with the same
/// scope, consumers and duplicate window as [`enable_cdc`], and no slot,
/// publication or reader. Without an event broker there is nothing to provision.
async fn ensure_streams(
    platform: &Platform,
    document: &EnvironmentDocument,
    manifests: &[wamn_catalog::manifest::PackageManifest],
) -> anyhow::Result<()> {
    let Some(events) = &platform.events else {
        return Ok(());
    };
    let triple = document.triple();
    let consumers = wamn_control_provision::events::entity_consumers(
        &triple,
        manifests
            .iter()
            .flat_map(wamn_catalog::manifest::PackageManifest::entity_pairs),
    );
    let broker = async_nats::jetstream::new(
        events
            .nats
            .connect()
            .await
            .context("connect the event provisioning credential")?,
    );
    crate::event_streams::provision(
        &broker,
        &triple,
        events.stream_replicas,
        std::time::Duration::from_secs(events.dup_window_secs),
        &consumers,
    )
    .await
}

/// Apply capture declarations after the packages exist in the project database.
async fn enable_cdc(
    platform: &Platform,
    document: &EnvironmentDocument,
    schemas: &[String],
    project: &tokio_postgres::Client,
    instance: &str,
) -> anyhow::Result<()> {
    platform.events.as_ref().context(
        "the environment captures application models and has no event broker; \
         set WAMN_NATS_URL and its WAMN_NATS_TLS_* certificate",
    )?;
    let cluster = resolve_cluster(
        &platform.system_database_url,
        &document.org,
        &document.policy,
    )
    .await?;
    let database = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        instance,
    );
    let capture = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        instance,
    );
    project
        .batch_execute(&crate::enable_cdc_project_env::cdc_sql_bundle(
            schemas, &capture, &database,
        ))
        .await?;
    let publication = wamn_control_provision::database::render_project_env_publication(
        &document.triple(),
        instance,
        &cluster,
        &platform.database_namespace,
        &capture,
        schemas,
    );
    kubectl(
        platform,
        &platform.database_namespace,
        &["apply", "-f", "-"],
        Some(&serde_json::to_vec(&publication)?),
    )
    .await?;
    wait_applied(platform, "publication", &database).await
}

/// Whether the environment's CDC is in place, judged by what it observes: the
/// publication `cdc_name` captures exactly `schemas` in database `project`, the
/// connection the probe runs on. [`ensure_cdc`] writes the publication last
/// among the CDC writes and records the reader on every apply.
///
/// # Errors
///
/// When the probe fails.
pub async fn cdc_in_place(
    project: &tokio_postgres::Client,
    cdc_name: &str,
    schemas: &[String],
) -> anyhow::Result<bool> {
    Ok(project
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_publication p WHERE p.pubname = $1 \
         AND NOT p.puballtables \
         AND coalesce((SELECT array_agg(n.nspname::text ORDER BY n.nspname) \
              FROM pg_publication_namespace pn JOIN pg_namespace n ON n.oid = pn.pnnspid \
              WHERE pn.pnpubid = p.oid), ARRAY[]::text[]) = ARRAY(SELECT DISTINCT unnest($2::text[]) ORDER BY 1))",
            &[&cdc_name, &schemas],
        )
        .await
        .context("probe the CDC publication schema")?
        .get(0))
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

/// Run `kubectl` against the platform's cluster in `namespace`.
pub(super) async fn kubectl(
    platform: &Platform,
    namespace: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> anyhow::Result<String> {
    crate::kubectl::kubectl(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        arguments,
        input,
    )
    .await
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

/// Create the environment account and immutable grants before the release starts.
async fn ensure_credential_access(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<()> {
    let account = wamn_credential_store::kubernetes::service_account_name(
        &document.org,
        &document.project,
        &document.env,
    );
    let secret = wamn_credential_store::kubernetes::secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    for (resource, object) in credential_access(&account, &secret) {
        let namespace = &platform.target.namespace;
        let observed = kubectl(
            platform,
            namespace,
            &[
                "get",
                resource,
                &account,
                "--ignore-not-found",
                "-o",
                "json",
            ],
            None,
        )
        .await?;
        if !observed.trim().is_empty() {
            continue;
        }
        kubectl(
            platform,
            namespace,
            &["create", "-f", "-"],
            Some(&serde_json::to_vec(&object)?),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_writes_the_registry_reader_secret_of_the_cdc_reader() {
        assert!(
            HOST_CREDENTIALS.iter().any(|credential| {
                credential.family == WorkloadRoleFamily::RegistryReader
                    && credential.database == CredentialDatabase::Control
            }),
            "the registry reader addresses the control database, not the project database"
        );
    }

    #[test]
    fn apply_prepares_a_secret_for_every_credential() {
        let platform = Platform {
            system_database_url: "postgresql://control".into(),
            target: wamn_release_control::release_chart::Target {
                kubeconfig: PathBuf::new(),
                context: String::new(),
                namespace: "hosts".into(),
            },
            chart: PathBuf::new(),
            release_artifact_base: String::new(),
            registry_auth_file: PathBuf::new(),
            oci_ca_paths: Vec::new(),
            insecure_registry: false,
            component_artifact_base: None,
            package_artifact_base: None,
            database_namespace: String::new(),
            database_host: Some("database".into()),
            database_port: 5432,
            host_group: None,
            pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
            events: None,
            auth_nats: None,
        };
        let document: EnvironmentDocument = serde_json::from_value(serde_json::json!({
            "org": "example", "project": "fixture", "env": "dev",
            "target": "test",
            "policy": "default",
            "applications": {}}))
        .unwrap();
        let private = PrivateDirectory::create().unwrap();
        let group = super::super::write::host_group(None, "example", "fixture", "dev").unwrap();
        for credential in HOST_CREDENTIALS {
            let request = credential_request(
                credential,
                &platform,
                &document,
                &private,
                "cluster",
                "postgresql://project",
            );
            assert_eq!(request.action.family, credential.family);
            assert_eq!(request.action.verb, WorkloadActionVerb::Prepare);
            assert_eq!(request.action.generation, CredentialGeneration::A);
            assert_eq!(request.secret, Some(private.path(credential.file_name)));
            assert_eq!(
                request.system_database_url.as_deref(),
                Some("postgresql://control")
            );
            assert_eq!(
                request.target_admin_database_url.as_deref(),
                (credential.database == CredentialDatabase::Project)
                    .then_some("postgresql://project")
            );
            if let Some(variable) = credential.host_variable {
                let reference = group["env"]
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["name"] == variable)
                    .unwrap();
                assert_eq!(
                    reference["valueFrom"]["secretKeyRef"]["name"],
                    wamn_control_provision::workload_secret_name(
                        request.action.family,
                        "example",
                        "fixture",
                        "dev"
                    )
                );
            }
        }
    }
}

#[cfg(test)]
mod credential_access_tests {
    use super::*;

    #[test]
    fn colliding_coordinates_read_only_their_own_secret() {
        use wamn_credential_store::kubernetes::{secret_name, service_account_name};

        let coordinates = [("a", "b-c", "d"), ("a", "b", "c-d")];
        let accounts =
            coordinates.map(|(org, project, env)| service_account_name(org, project, env));
        let secrets = coordinates.map(|(org, project, env)| secret_name(org, project, env));
        assert_eq!(accounts, ["wamn-host-a--b-c--d", "wamn-host-a--b--c-d"]);
        assert_eq!(
            secrets,
            ["wamn-credential-a--b-c--d", "wamn-credential-a--b--c-d"]
        );
        assert_ne!(accounts[0], accounts[1]);
        assert_ne!(secrets[0], secrets[1]);

        for index in 0..2 {
            let account = &accounts[index];
            let secret = &secrets[index];
            let objects = credential_access(account, secret);
            for (_, object) in &objects {
                assert_eq!(object["metadata"]["name"], *account);
            }
            assert_eq!(
                objects[1].1["rules"],
                serde_json::json!([{"apiGroups":[""],"resources":["secrets"],"resourceNames":[secret],"verbs":["get"]}])
            );
            assert!(
                !objects[1].1["rules"][0]["resourceNames"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(secrets[1 - index]))
            );
            assert_eq!(
                objects[2].1["roleRef"]["name"],
                objects[1].1["metadata"]["name"]
            );
            assert_eq!(objects[2].1["subjects"].as_array().unwrap().len(), 1);
            assert_eq!(objects[2].1["subjects"][0]["name"], *account);
        }
    }
}
