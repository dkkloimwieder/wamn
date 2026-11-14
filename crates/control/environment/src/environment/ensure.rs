//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use sqlx::Row as _;
pub(super) mod plan;
pub(super) use plan::management_author_pat_name;

use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use wamn_registry::Triple;

use super::Platform;
use super::document::EnvironmentDocument;
use crate::provision_project_env::{
    ProvisionProjectEnvRequest, provision_project_env, read_project_env_instance, resolve_cluster,
};
use wamn_postgres::connect;

/// How long one CR may take to report `status.applied`.
const APPLIED_TIMEOUT: &str = "--timeout=120s";

/// Provision shared resources for the document's environment.
///
/// # Errors
///
/// When an ensure fails. A rerun repairs a partial run.
pub async fn provision_resources(
    platform: &Platform,
    control: &serde_json::Value,
    document: &EnvironmentDocument,
    packages: &[wamn_compiler::manifest::PackageManifest],
    admin_url: &str,
) -> anyhow::Result<()> {
    platform
        .events
        .as_ref()
        .context("environment provisioning requires the event broker")?;
    let mut admin = connect(admin_url).await?;
    let triple = document.triple();
    let policy = document.policy.as_str();
    let cluster = resolve_cluster(&platform.system_database_url, &document.org, policy).await?;
    let private = PrivateDirectory::create()?;
    let namespace = platform.target.namespace.as_str();

    // The row, the role posture, the Database CR, the base privileges, the
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
    crate::platform::operations::provision(
        &platform.target,
        &platform.chart,
        &triple,
        &cluster,
        &outcome.database,
    )
    .await?;
    ensure_credential_access(platform, document, &cluster).await?;
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &wamn_control_provision::environment_names::environment_resource_name(
            &document.org,
            &document.project,
            &document.env,
        )?,
        outcome.database_values.clone(),
    )
    .await?;
    wait_applied(platform, "database", &outcome.database).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(outcome.privilege_sql.clone()))
        .execute(&mut admin)
        .await
        .map(drop)
        .context("ensure the database base privileges")?;
    private.apply_and_shred(platform, &database_secret).await?;
    if issue_pat {
        private.apply_and_shred(platform, &pat_secret).await?;
    }

    // The run plane: the run schema the credentials are granted on.
    let project_admin =
        ensure_project_database(platform, document, packages, &mut admin, admin_url).await?;
    restore_project_database_access(platform, document, &project_admin).await?;

    crate::provision_project_env::workload::provision(
        platform,
        document,
        &cluster,
        &project_admin,
        admin_url,
    )
    .await?;

    let mut client = connect(&platform.system_database_url).await?;
    let policy = super::analyse::read_policy(&mut client, &document.org, &document.policy).await?;
    crate::platform::control::wait_administration(
        &platform.target,
        &triple,
        true,
        std::time::Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?),
    )
    .await?;
    let certificates = wamn_broker::authorization::update_authorization(
        &platform.target,
        &document.org,
        Some(&document.triple()),
    )
    .await?;
    let name = wamn_control_provision::environment_names::environment_resource_name(
        &document.org,
        &document.project,
        &document.env,
    )?;
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        wamn_platform_install::environment::broker_certificates(certificates),
    )
    .await?;
    prepare_cdc(platform, document, &mut admin, &project_admin).await?;
    ensure_streams(platform, document, &[]).await?;
    provision_environment_host(platform, control, document).await?;
    Ok(())
}

/// Observe the resources at the end of provisioning, not merely its first row.
/// The environment host starts after credentials, the database and CDC preparation succeed.
pub(super) async fn resources_in_place(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<bool> {
    let mut system = connect(&platform.system_database_url).await?;
    if super::analyse::read_row(&mut system, document)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    if !super::analyse::release_present(platform, &name).await? {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn environment_host_name(org: &str, project: &str, env: &str) -> anyhow::Result<String> {
    Ok(wamn_control_provision::environment_names::environment_resource_name(org, project, env)?)
}

/// Render the shared host without application release arguments or labels.
pub fn environment_host_values(
    document: &EnvironmentDocument,
    drain_bound_seconds: u32,
    stream_replicas: usize,
    dup_window_secs: u64,
    host_group: Option<serde_yaml::Mapping>,
    insecure_registry: bool,
    installed: Option<&serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    use wamn_release_control::release_chart;
    let group = super::write::plan::host_group(
        host_group.unwrap_or_default(),
        &document.org,
        &document.project,
        &document.env,
    )?;
    let registry_arguments = super::write::plan::registry_arguments(&group);
    let mut group = serde_json::to_value(group)?;
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
    let mut arguments = vec![format!("--environment={}", document.env)];
    arguments.extend(registry_arguments);
    if insecure_registry {
        arguments.push("--allow-insecure-registries".into());
    }
    group["extraArgs"] = json!(arguments);
    let variables = group["env"]
        .as_array_mut()
        .context("host env is not a list")?;
    for variable in &mut *variables {
        if matches!(
            variable["name"].as_str(),
            Some(
                "WAMN_COMPONENT_ARTIFACT_BASE"
                    | "WAMN_PACKAGE_ARTIFACT_BASE"
                    | "WAMN_RELEASE_ARTIFACT_BASE"
            )
        ) && let Some(base) = variable["value"].as_str()
        {
            variable["value"] = super::runtime::pod_artifact_base(insecure_registry, base)?
                .into_owned()
                .into();
        }
    }
    for (key, value) in [
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
    if let Some(image) = super::runtime::host_image(installed, group["name"].as_str().unwrap()) {
        group["image"] = image.clone();
    }
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    Ok(json!({
        "roles": [],
        "runtime-operator": {"runtime": {
            "terminationGracePeriodSeconds": drain_bound_seconds,
            "serviceAccount": {"create":false, "name": wamn_registry::service_account_name(&document.org, &document.project, &document.env)},
            "automountServiceAccountToken": true,
            "hostGroups": [group]
        }},
        "global": {"certificates": {"dataCertSecretName": wamn_registry::name::nats_certificate(&project_env_key)}}
    }))
}

pub(super) async fn provision_environment_host(
    platform: &Platform,
    control: &serde_json::Value,
    document: &EnvironmentDocument,
) -> anyhow::Result<()> {
    let mut system = connect(&platform.system_database_url).await?;
    let policy = super::analyse::read_policy(&mut system, &document.org, &document.policy).await?;
    let events = platform
        .events
        .as_ref()
        .context("environment provisioning requires the event broker")?;
    let name = environment_host_name(&document.org, &document.project, &document.env)?;
    let installed = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &name,
    )
    .await?;
    let component_base = super::runtime::pod_artifact_base(
        platform.insecure_registry,
        platform
            .component_artifact_base
            .as_deref()
            .context("missing component registry")?,
    )?;
    let group = super::runtime::host_group(control, None, &component_base)?;
    let desired = environment_host_values(
        document,
        u32::try_from(policy.drain_bound_seconds)?,
        events.stream_replicas,
        events.dup_window_secs,
        Some(group),
        platform.insecure_registry,
        installed.as_ref(),
    )?;
    let mut values: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(platform.chart.join("values.yaml"))?)?;
    crate::platform::resources::merge(&mut values, desired);
    let chart = platform.chart.join("platform/environment");
    super::runtime::require_chart_references(&platform.target, &chart, &name, &values).await?;
    wamn_platform_install::environment::apply(&platform.target, &chart, &name, values).await?;
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(outcome.role_sql.clone()))
        .execute(&mut connect(admin_url).await?)
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
    packages: &[wamn_compiler::manifest::PackageManifest],
    admin: &mut sqlx::PgConnection,
    admin_url: &str,
) -> anyhow::Result<String> {
    ensure_project_database_with_migrations(
        platform,
        document,
        packages,
        admin,
        admin_url,
        &crate::platform_migrations::embedded_sources(
            wamn_control_provision::migrations::MigrationDatabase::Project,
        ),
    )
    .await
}

pub(crate) async fn ensure_project_database_with_migrations(
    platform: &Platform,
    document: &EnvironmentDocument,
    _packages: &[wamn_compiler::manifest::PackageManifest],
    admin: &mut sqlx::PgConnection,
    admin_url: &str,
    sources: &[sqlx::migrate::Migration],
) -> anyhow::Result<String> {
    let project_url = project_url(platform, &document.triple()).await?;
    let database = wamn_postgres::identity(&project_url)?.1;
    let project_admin = wamn_postgres::database_configuration(admin_url, &database)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::install_platform_extensions_sql(),
    ))
    .execute(&mut connect(&project_admin).await?)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::install_platform_extensions_sql(),
    ))
    .execute(&mut connect(&project_admin).await?)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_platform_group_role_sql(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT CONNECT ON DATABASE {} TO wamn_apply; GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE;",
        postgres_protocol::escape::escape_identifier(&database),
    ))).execute(&mut *admin).await.map(drop)?;
    let mut project = connect(&project_admin).await?;
    if crate::platform_migrations::install_project(&mut project).await? {
        let migration_input = crate::provision_project_env::workload::provision_platform_migration(
            &platform.system_database_url,
            &project_admin,
            wamn_control_provision::migrations::MigrationDatabase::Project,
        )
        .await?;
        crate::platform_migrations::restore_project(&migration_input, sources).await?;
    }
    let key = wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    wamn_platform_identity::environment::fill(
        &platform.system_database_url,
        &project_admin,
        wamn_platform_identity::application::ApplicationTarget {
            org: &document.org,
            project: &document.project,
            env: &document.env,
            project_env_key: &key,
        },
        &platform.auth_changes().await?,
    )
    .await?;
    Ok(project_admin)
}

/// Restore the existing workload credentials' database-local grants.
///
/// # Errors
///
/// Returns an error if a credential has drifted or a grant cannot be restored.
pub async fn restore_project_database_access(
    platform: &Platform,
    document: &EnvironmentDocument,
    project_admin: &str,
) -> anyhow::Result<()> {
    crate::provision_project_env::workload::restore_database_access(
        &platform.system_database_url,
        project_admin,
        &document.triple(),
    )
    .await
    .context("restore workload database access")
}

/// Prepare replication authority before the admin connection leaves ensure.
pub(super) async fn prepare_cdc(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin: &mut sqlx::PgConnection,
    project_admin_url: &str,
) -> anyhow::Result<()> {
    let mut project_admin = connect(project_admin_url).await?;
    let instance =
        read_project_env_instance(&platform.system_database_url, &document.triple()).await?;
    let role = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let cluster = super::sql::reset::recorded_cluster(platform, document).await?;
    let secret_name = wamn_control_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    crate::provision_project_env::workload::reconcile(
        &platform.system_database_url,
        &platform.target.kubeconfig,
        &platform.target.context,
        &document.org,
        &cluster,
        &wamn_control_provision::connection::managed_replication_role(
            role.clone(),
            secret_name.clone(),
        ),
    )
    .await?;
    crate::provision_project_env::workload::wait_role(admin, &role).await?;
    let options = wamn_postgres::connect_options(project_admin_url).await?;
    let config = crate::provision_project_env::workload::configuration(
        &document.org,
        &cluster,
        options.get_host().into(),
        options.get_port(),
        options
            .get_database()
            .context("capture database is absent")?
            .into(),
        role.clone(),
        secret_name.clone(),
    );
    wamn_kubernetes::kubectl::apply(&platform.target.kubeconfig,&platform.target.context,&document.org,
        &serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"namespace":document.org,"name":secret_name},"data":{"connection.json":config.to_json()?}})
    ).await?;

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::create_failover_slot_sql(&role),
    ))
    .execute(&mut project_admin)
    .await
    .map(drop)?;
    Ok(())
}

/// Read every installed baseline, independent of the selected application release.
pub async fn installed_manifests(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_compiler::manifest::PackageManifest>> {
    let mut manifests = Vec::new();
    for (id, baseline) in super::stage::installed(platform, document).await? {
        let package = super::stage::open(platform, &id, &baseline.current).await?;
        let path = wamn_compiler::package_manifest_path(package.root());
        manifests.push(wamn_compiler::manifest::PackageManifest::from_slice(
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
    ensure_cdc_inputs(platform, document, None).await
}

pub(crate) async fn ensure_cdc_inputs(
    platform: &Platform,
    document: &EnvironmentDocument,
    administrator: Option<&str>,
) -> anyhow::Result<()> {
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
    wamn_events::capture_gap::require_resynchronized(&platform.system_database_url, &triple)
        .await?;
    let instance = read_project_env_instance(&platform.system_database_url, &triple).await?;
    let cdc_name = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let mut project =
        super::analyse::connect_project(&project_url(platform, &triple).await?).await?;
    let mut full = sqlx::query(sqlx::AssertSqlSafe(
        (crate::replica_identity::select_replica_identity_sql()).to_string(),
    ))
    .bind(&schemas)
    .fetch_all(&mut project)
    .await
    .context("read captured replica identities")?
    .into_iter()
    .filter(|row| row.get::<String, _>(2) == "f")
    .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
    .collect::<std::collections::BTreeSet<_>>();
    for manifest in &manifests {
        for model in manifest.models.values() {
            if !full.insert((model.schema.clone(), model.table.clone())) {
                continue;
            }
            let sql = crate::replica_identity::alter_replica_identity_sql(
                &model.schema,
                &model.table,
                crate::replica_identity::ReplicaIdentity::Full,
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))
                .execute(&mut project)
                .await
                .map(drop)
                .with_context(|| {
                    format!(
                        "capture {}.{} with full row images",
                        model.schema, model.table
                    )
                })?;
        }
    }
    let database = wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let administrator_env;
    let administrator = if let Some(url) = administrator {
        url
    } else {
        administrator_env =
            std::env::var("WAMN_SYSTEM_ADMIN_URL").context("CDC administrator URL")?;
        &administrator_env
    };
    let admin_url = wamn_postgres::database_configuration(administrator, &database)?;
    let capture = if wamn_postgres::configuration(&platform.system_database_url)?
        .trim_start()
        .starts_with('{')
    {
        wamn_platform_install::environment::capture_connection(
            &platform.system_database_url,
            &document.triple(),
        )
        .await?
    } else {
        admin_url.clone()
    };
    wamn_events::lifecycle::apply_cdc_sql(
        admin_url.as_str(),
        &capture,
        &schemas,
        &cdc_name,
        &database,
    )
    .await?;
    if !cdc_in_place(&mut project, &cdc_name, &schemas).await? {
        enable_cdc(platform, document, &schemas, &instance).await?;
    }
    ensure_streams(platform, document, &manifests).await?;
    let namespace = platform.target.namespace.as_str();
    wamn_events::lifecycle::record_event_reader(
        &platform.system_database_url,
        &triple,
        &schemas,
        &cdc_name,
        Some(namespace),
    )
    .await
    .context("record the CDC reader")?;
    let image = wamn_release_control::release_chart::image_set(&platform.chart)
        .await?
        .host;
    let configuration = wamn_events::lifecycle::reader_configuration(
        &triple,
        &cdc_name,
        crate::platform::identity::NATS_URL,
    );
    let config_name = wamn_control_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    wamn_kubernetes::kubectl::apply(&platform.target.kubeconfig, &platform.target.context,
        namespace, &serde_json::json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":config_name,"namespace":namespace},
            "data":{"connection.json":capture,"reader.json":serde_json::to_string(&configuration)?}})).await?;
    let deployment = wamn_events::lifecycle::reader_deployment(&triple, namespace, &image);
    wamn_kubernetes::kubectl::apply(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        &deployment,
    )
    .await?;
    kubectl(
        platform,
        namespace,
        &[
            "rollout",
            "status",
            &format!(
                "deployment/{}",
                deployment["metadata"]["name"]
                    .as_str()
                    .context("CDC reader name")?
            ),
            "--timeout=120s",
        ],
        None,
    )
    .await?;
    Ok(())
}

/// Provision the environment's source and advisory streams, with the same
/// target, consumers and duplicate window as [`enable_cdc`], and no slot,
/// publication or reader. Without an event broker there is nothing to provision.
pub(super) async fn ensure_streams(
    platform: &Platform,
    document: &EnvironmentDocument,
    manifests: &[wamn_compiler::manifest::PackageManifest],
) -> anyhow::Result<()> {
    let Some(events) = &platform.events else {
        return Ok(());
    };
    let triple = document.triple();
    let mut consumers = Vec::new();
    for (application, selection) in &document.applications {
        for (package, entity) in manifests
            .iter()
            .filter(|manifest| selection.packages.contains(&manifest.package.id))
            .flat_map(wamn_compiler::manifest::PackageManifest::entity_pairs)
        {
            consumers.push(wamn_broker::events::application_consumer(
                &triple,
                application,
                package,
                entity,
            )?);
        }
    }
    let broker = async_nats::jetstream::new(
        events
            .nats
            .connect()
            .await
            .context("connect the event provisioning credential")?,
    );
    wamn_broker::streams::provision(
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
    instance: &str,
) -> anyhow::Result<()> {
    platform.events.as_ref().context(
        "the environment captures application models and has no event broker; \
         set WAMN_NATS_URL and its WAMN_NATS_TLS_* certificate",
    )?;
    let cluster = super::sql::reset::recorded_cluster(platform, document).await?;
    let mut database = wamn_control_provision::project_env_database_name(
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
    let publication = wamn_platform_install::environment::publication_values(
        &document.triple(),
        instance,
        &cluster,
        &capture,
        schemas,
    );
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        // The database probe found missing or changed capture. A retained CR can
        // still report applied for the destroyed database, so replace its status.
        kubectl(
            platform,
            &platform.database_namespace,
            &[
                "delete",
                "publication",
                &mut database,
                "--ignore-not-found",
                "--wait=true",
                APPLIED_TIMEOUT,
            ],
            None,
        )
        .await?;
        let name = wamn_control_provision::environment_names::environment_resource_name(
            &document.org,
            &document.project,
            &document.env,
        )?;
        wamn_platform_install::environment::apply(
            &platform.target,
            &platform.chart.join("platform/environment"),
            &name,
            publication.clone(),
        )
        .await?;
        wait_applied(platform, "publication", &database).await?;
        let mut project =
            super::analyse::connect_project(&project_url(platform, &document.triple()).await?)
                .await?;
        while !cdc_in_place(&mut project, &capture, schemas).await? {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| format!("rebuild publication {capture} in database {database}"))?
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
    project: &mut sqlx::PgConnection,
    cdc_name: &str,
    schemas: &[String],
) -> anyhow::Result<bool> {
    Ok(sqlx::query("SELECT EXISTS (SELECT FROM pg_publication p WHERE p.pubname = $1 \
         AND NOT p.puballtables \
         AND coalesce((SELECT array_agg(n.nspname::text ORDER BY n.nspname) \
              FROM pg_publication_namespace pn JOIN pg_namespace n ON n.oid = pn.pnnspid \
              WHERE pn.pnpubid = p.oid), ARRAY[]::text[]) = ARRAY(SELECT DISTINCT unnest($2::text[]) ORDER BY 1))").bind(cdc_name).bind(schemas).fetch_one(&mut *project)
        .await
        .context("probe the CDC publication schema")?
        .get(0))
}

/// The URL of the environment's project database: the system URL's login on
/// the database the registry row names.
pub(super) async fn project_url(platform: &Platform, triple: &Triple) -> anyhow::Result<String> {
    if wamn_postgres::configuration(&platform.system_database_url)?
        .trim_start()
        .starts_with('{')
    {
        return wamn_platform_install::environment::connection(
            &platform.system_database_url,
            triple,
        )
        .await?
        .to_json();
    }
    let instance = read_project_env_instance(&platform.system_database_url, triple).await?;
    wamn_postgres::database_configuration(
        &platform.system_database_url,
        &wamn_control_provision::project_env_database_name(
            &triple.org,
            &triple.project,
            &triple.env,
            &instance,
        ),
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
pub(super) async fn wait_applied(
    platform: &Platform,
    type_: &str,
    name: &str,
) -> anyhow::Result<()> {
    kubectl(
        platform,
        &platform.database_namespace,
        &[
            "wait",
            &format!("{type_}/{name}"),
            "--for=jsonpath={.status.applied}=true",
            APPLIED_TIMEOUT,
        ],
        None,
    )
    .await
    .map(drop)
    .with_context(|| format!("wait for {type_} {name} to report status.applied"))
}

/// Run `kubectl` against the platform's cluster in `namespace`.
pub(crate) async fn kubectl(
    platform: &Platform,
    namespace: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> anyhow::Result<String> {
    wamn_kubernetes::kubectl::kubectl(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        arguments,
        input,
    )
    .await
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
    cluster: &str,
) -> anyhow::Result<()> {
    let account =
        wamn_registry::service_account_name(&document.org, &document.project, &document.env);
    let secret =
        wamn_registry::credential_secret_name(&document.org, &document.project, &document.env);
    let mut secrets = vec![
        secret,
        format!("{}-ca", cluster),
        "wamn-pg-ca".into(),
        wamn_control_provision::project_env_cdc_secret_name(
            &document.org,
            &document.project,
            &document.env,
        ),
    ];
    secrets.extend(
        wamn_control_provision::WorkloadRoleFamily::project_env_families()
            .filter(|family| {
                *family != wamn_control_provision::WorkloadRoleFamily::SessionRoleReader
            })
            .map(|family| {
                wamn_control_provision::workload_secret_name(
                    family,
                    &document.org,
                    &document.project,
                    &document.env,
                )
            }),
    );
    let name = wamn_control_provision::environment_names::environment_resource_name(
        &document.org,
        &document.project,
        &document.env,
    )?;
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        serde_json::json!({"credentialAccess":{"account":account,"secrets":secrets}}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_host_reapply_keeps_only_its_named_host_image() -> anyhow::Result<()> {
        use serde_json::json;
        let document = EnvironmentDocument::from_json(br#"{"org":"acme","project":"apps","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"applications":{}}"#)?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let chart = crate::platform::artifact_plan::test_util::prepared_chart(&repository)?;
        let fresh = environment_host_values(&document, 70, 1, 120, None, false, None)?;
        let mut installed = fresh.clone();
        let image = json!({"registry":"selected.example", "repository":"host",
            "tag":format!("selected@sha256:{}", "b".repeat(64)), "pull_policy":"IfNotPresent"});
        let group = &mut installed["runtime-operator"]["runtime"]["hostGroups"][0];
        group["image"] = image.clone();
        group["replicas"] = 9.into();
        group["env"] = json!([]);
        installed["roles"] = json!([{"name":"stale"}]);
        installed["runtime-operator"]["runtime"]["hostGroups"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"name":"unrelated", "image":{"tag":"wrong"}}));
        let mut reapplied =
            environment_host_values(&document, 70, 1, 120, None, false, Some(&installed))?;
        assert_eq!(
            reapplied["runtime-operator"]["runtime"]["hostGroups"][0]["image"],
            image
        );
        let entry = json!({"chart":chart.0, "version":crate::platform::artifact_plan::select(&chart.0)?.version,
            "name":environment_host_name(&document.org, &document.project, &document.env)?,
            "namespace":"hosts", "values":reapplied});
        let objects = wamn_kubernetes::helm::template(&entry)?;
        let host = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        assert_eq!(
            host["spec"]["template"]["spec"]["containers"][0]["image"],
            format!("selected.example/host:selected@sha256:{}", "b".repeat(64))
        );
        reapplied["runtime-operator"]["runtime"]["hostGroups"][0]
            .as_object_mut()
            .unwrap()
            .remove("image");
        assert_eq!(reapplied, fresh);
        Ok(())
    }
}

/// Slot creation uses the capture role; schema ownership grants no replication privilege.
pub(super) async fn prepare_cdc_with_connection(
    platform: &Platform,
    document: &EnvironmentDocument,
    capture: &str,
) -> anyhow::Result<()> {
    let instance =
        read_project_env_instance(&platform.system_database_url, &document.triple()).await?;
    let role = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    let mut connection = connect(capture).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::create_failover_slot_sql(&role),
    ))
    .execute(&mut connection)
    .await?;
    let name = wamn_control_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    wamn_kubernetes::kubectl::apply(&platform.target.kubeconfig,&platform.target.context,&document.org,&serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":document.org},"data":{"connection.json":capture}})).await
}
