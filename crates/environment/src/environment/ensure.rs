//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use crate::environment_sql_effects::PostgresSession;
use anyhow::Context as _;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use wamn_postgres::database::ConnectionConfig;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple, name};

use super::Platform;
use super::document::EnvironmentDocument;

/// Remove one project's runtime access and wait until its credentials are unmounted.
pub(super) async fn suspend_project(
    platform: &Platform,
    system: &PostgresSession,
    triple: &Triple,
    drain_bound_seconds: Option<i32>,
) -> anyhow::Result<()> {
    use std::time::Duration;
    use tokio::time::{Instant, sleep_until, timeout_at};

    let name = name::environment_host_name(&triple.org, &triple.env);
    let values = read_host_values(
        platform,
        system,
        &triple.org,
        &triple.env,
        &[],
        Some(&triple.project),
        drain_bound_seconds,
    )
    .await?;
    upgrade_host(platform, &triple.org, &triple.env, &values).await?;
    let connection = system.connection(triple).await?;
    let started = Instant::now();
    let mut deadline = started + Duration::from_secs(120);
    let mut next_check = started;
    loop {
        let pods = timeout_at(deadline, async {
            sleep_until(next_check).await;
            super::analyse::read_pods(platform, &name).await
        })
        .await
        .context("project still has active runtime resources")??;
        let grace = pods["items"]
            .as_array()
            .context("host pod list has no items")?
            .iter()
            .filter(|pod| {
                !matches!(
                    pod["status"]["phase"].as_str(),
                    Some("Succeeded" | "Failed")
                ) && pod["spec"]["volumes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|volume| {
                        volume["name"] == "project-database"
                            && volume["projected"]["sources"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|source| source["secret"]["name"] == connection.server_ca.name)
                    })
            })
            .map(|pod| {
                pod["spec"]["terminationGracePeriodSeconds"]
                    .as_u64()
                    .unwrap_or(0)
            })
            .max();
        let Some(grace) = grace else {
            return Ok(());
        };
        // Allow time for the pod list to reflect shutdown, as in drain::wait_stopped.
        deadline = deadline.max(started + Duration::from_secs(grace.saturating_add(15)));
        next_check = Instant::now() + Duration::from_secs(2);
    }
}

/// Upgrade the environment host and wait for readiness.
pub(super) async fn upgrade_host(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    values: &Value,
) -> anyhow::Result<()> {
    wamn_platform_install::deployment::prepare_host_upgrade(
        &platform.cluster,
        &platform.chart,
        name::environment_host_name(org, env),
        values,
    )
    .await?
    .await
}

pub(super) async fn read_host_values(
    platform: &Platform,
    system: &PostgresSession,
    org: &OrgId,
    env: &EnvName,
    reserved: &[&str],
    excluded: Option<&ProjectId>,
    drain_bound_seconds: Option<i32>,
) -> anyhow::Result<Value> {
    let (projects, drain_bound_seconds) = system
        .host_projects(org, env, reserved, excluded, drain_bound_seconds)
        .await?;
    if projects.is_empty() {
        return Ok(json!({"org":org, "env":env}));
    }

    let group = super::write::host_group(platform.host_group.as_deref())?;
    let placement: Map<String, Value> = ["replicas", "nodeSelector", "tolerations", "resources"]
        .into_iter()
        .filter_map(|key| group.get(key).map(|value| (key, value)))
        .map(|(key, value)| Ok((key.into(), serde_json::to_value(value)?)))
        .collect::<serde_json::Result<_>>()?;
    let image = wamn_platform_install::deployment::host_image(&platform.chart).await?;

    let mut eligible = Map::new();
    for project in projects {
        let triple = Triple::new(org.clone(), env.clone(), project);
        let connection = system.connection(&triple).await?;
        eligible.insert(
            triple.project.to_string(),
            host_project_values(&triple, &connection)?,
        );
    }
    let (workflow, ingress) = tokio::try_join!(
        super::broker::inbox(platform, org, env, "workflow"),
        super::broker::inbox(platform, org, env, "ingress"),
    )?;
    Ok(host_values(
        org,
        env,
        &image,
        &placement,
        drain_bound_seconds,
        &eligible,
        &platform.nats_url,
        &platform.identity_issuer,
        platform.registry_token_metadata,
        platform.insecure_registry,
        &platform.oci_ca_paths,
        &serde_json::to_value(&platform.oauth_encryption)?,
        &workflow,
        &ingress,
    ))
}

/// Build the host chart values for one project's connection and workload logins.
pub fn host_project_values(
    triple: &Triple,
    connection: &ConnectionConfig,
) -> anyhow::Result<Value> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    anyhow::ensure!(
        connection.credentials.namespace == org.as_str()
            && connection.server_ca.namespace == org.as_str(),
        "project login Secrets and host must share an org"
    );
    let mut logins = Map::new();
    for (family, config) in crate::provision_project_env::workload::connections(connection)? {
        let role = match family {
            wamn_identity_protocol::workload_role::WorkloadRoleFamily::App => "guest-sql",
            wamn_identity_protocol::workload_role::WorkloadRoleFamily::Query => "dynamic-sql",
            wamn_identity_protocol::workload_role::WorkloadRoleFamily::Platform => "platform",
            wamn_identity_protocol::workload_role::WorkloadRoleFamily::Migration => continue,
        };
        logins.insert(role.into(), config.credentials.name.into());
    }
    Ok(json!({
        "host":connection.host,
        "port":connection.port,
        "database":connection.database,
        "cluster":name::project_cluster(&connection.database),
        "logins":logins,
        "serverCa":connection.server_ca.name,
        "credentialSecret":name::credential_secret_name(org, project, env),
        "stream":name::stream_name(org, project, env),
    }))
}

/// Build the environment host chart values from resolved deployment inputs.
#[expect(clippy::too_many_arguments, reason = "resolved inputs for one chart")]
pub fn host_values(
    org: &OrgId,
    env: &EnvName,
    image: &str,
    placement: &Map<String, Value>,
    drain_bound_seconds: i32,
    projects: &Map<String, Value>,
    nats_url: &str,
    identity_issuer: &str,
    registry_token_metadata: bool,
    insecure_registry: bool,
    oci_ca_paths: &[PathBuf],
    oauth_encryption: &Value,
    workflow_inbox: &str,
    ingress_inbox: &str,
) -> Value {
    json!({
        "org":org,
        "env":env,
        "hostName":name::environment_host_name(org, env),
        "serviceAccount":name::environment_service_account(org, env),
        "image":image,
        "placement":placement,
        "drainBoundSeconds":drain_bound_seconds,
        "projects":projects,
        "natsUrl":nats_url,
        "identityIssuer":identity_issuer,
        "registryTokenMetadata":registry_token_metadata,
        "insecureRegistry":insecure_registry,
        "ociCaPaths":oci_ca_paths,
        "oauthEncryption":oauth_encryption,
        "environmentNats":{
            "data":name::environment_nats_secret(org,env,"data"),
            "native":name::environment_nats_secret(org,env,"workflow-native"),
            "workflow":{"secret":name::environment_nats_secret(org,env,"workflow"), "inbox":workflow_inbox},
            "ingress":{"secret":name::environment_nats_secret(org,env,"ingress"), "inbox":ingress_inbox},
            "subjects":name::environment_event_subjects(org, env),
        },
    })
}

/// Read every installed baseline, independent of the selected application release.
async fn installed_manifests(
    platform: &Platform,
    system: &PostgresSession,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_component_policy::PackageManifest>> {
    let installed = super::stage::installed(system, document)
        .await?
        .into_iter()
        .map(|(id, baseline)| (id, baseline.artifact_digest));
    Ok(super::stage::open(platform, installed)
        .await?
        .into_iter()
        .map(|(_, _, manifest)| manifest)
        .collect())
}

/// Provision the project source stream through its environment account.
pub(super) async fn ensure_stream(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let events = platform
        .events
        .as_ref()
        .context("event stream policy is absent")?;
    let broker = super::broker::admin(platform, &triple.org, &triple.env)
        .await?
        .context("environment stream-admin credential is absent")?;
    platform
        .broker
        .provision(&platform.nats_url, &broker, triple, events)
        .await
}

/// Resolve the schemas already recorded in the project database.
pub(super) async fn publication_schemas(
    platform: &Platform,
    system: &PostgresSession,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_package::canonical_name::CanonicalName>> {
    Ok(installed_manifests(platform, system, document)
        .await?
        .into_iter()
        .flat_map(|manifest| {
            manifest
                .models
                .into_values()
                .map(|model| model.schema.into())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect())
}
