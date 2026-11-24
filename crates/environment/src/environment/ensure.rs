//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use crate::environment_sql_effects::PostgresSession;
use anyhow::Context as _;
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
    let values = host_values(
        platform,
        system,
        &triple.org,
        &triple.env,
        &[],
        Some(&triple.project),
        drain_bound_seconds,
    )
    .await?;
    platform
        .cluster
        .prepare_host_upgrade(&platform.chart, name.clone(), &values)
        .await?
        .await?;
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

/// Prepare the host upgrade with both configured and newly reserved projects.
pub(super) async fn prepare_host_upgrade<'a>(
    platform: &'a Platform,
    system: &PostgresSession,
    org: &OrgId,
    env: &EnvName,
    drain_bound_seconds: i32,
    reserved: &[&str],
) -> anyhow::Result<(u32, impl Future<Output = anyhow::Result<()>> + use<'a>)> {
    let name = name::environment_host_name(org, env);
    let values = host_values(
        platform,
        system,
        org,
        env,
        reserved,
        None,
        Some(drain_bound_seconds),
    )
    .await?;
    // The runtime operator defaults the group to one replica.
    let replicas = if values["placement"]["replicas"].is_null() {
        1
    } else {
        u32::try_from(
            values["placement"]["replicas"]
                .as_u64()
                .context("invalid host replica count")?,
        )?
    };
    let upgrade = platform
        .cluster
        .prepare_host_upgrade(&platform.chart, name, &values)
        .await?;
    Ok((replicas, upgrade))
}

async fn host_values(
    platform: &Platform,
    system: &PostgresSession,
    org: &OrgId,
    env: &EnvName,
    reserved: &[&str],
    excluded: Option<&ProjectId>,
    drain_bound_seconds: Option<i32>,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;

    let (projects, drain_bound_seconds) = system
        .host_projects(org, env, reserved, excluded, drain_bound_seconds)
        .await?;
    if projects.is_empty() {
        return Ok(json!({"org":org, "env":env}));
    }

    let group = super::write::host_group(platform.host_group.as_deref())?;
    let placement: serde_json::Map<String, serde_json::Value> = [
        "replicas",
        "nodeSelector",
        "tolerations",
        "affinity",
        "topologySpreadConstraints",
        "resources",
    ]
    .into_iter()
    .filter_map(|key| group.get(key).map(|value| (key, value)))
    .map(|(key, value)| Ok((key.into(), serde_json::to_value(value)?)))
    .collect::<serde_json::Result<_>>()?;
    let image = platform.cluster.host_image(&platform.chart).await?;

    let mut eligible = serde_json::Map::new();
    for project in projects {
        let triple = Triple::new(org.clone(), env.clone(), project);
        let connection = system.connection(&triple).await?;
        anyhow::ensure!(
            connection.credentials.namespace == org.as_str()
                && connection.server_ca.namespace == org.as_str(),
            "project login Secrets and host must share an org"
        );
        let mut logins = serde_json::Map::new();
        for (family, config) in crate::provision_project_env::workload::connections(&connection)? {
            let role = match family {
                wamn_identity_protocol::workload_role::WorkloadRoleFamily::App => "guest-sql",
                wamn_identity_protocol::workload_role::WorkloadRoleFamily::Query => "dynamic-sql",
                wamn_identity_protocol::workload_role::WorkloadRoleFamily::Platform => "platform",
                wamn_identity_protocol::workload_role::WorkloadRoleFamily::Migration => continue,
            };
            logins.insert(role.into(), config.credentials.name.into());
        }
        eligible.insert(
            triple.project.to_string(),
            json!({
                "host":connection.host,
                "port":connection.port,
                "database":connection.database,
                "cluster":name::project_cluster(&connection.database),
                "logins":logins,
                "serverCa":connection.server_ca.name,
                "credentialSecret":name::credential_secret_name(org, &triple.project, env),
                "stream":name::stream_name(org, &triple.project, env),
            }),
        );
    }
    let (workflow, ingress) = tokio::try_join!(
        super::broker::binding(platform, org, env, "workflow"),
        super::broker::binding(platform, org, env, "ingress"),
    )?;
    Ok(json!({
        "org":org,
        "env":env,
        "hostName":name::environment_host_name(org, env),
        "serviceAccount":name::environment_service_account(org, env),
        "image":image,
        "placement":placement,
        "drainBoundSeconds":drain_bound_seconds,
        "projects":eligible,
        "natsUrl":platform.nats_url,
        "identityIssuer":platform.identity_issuer,
        "registryTokenMetadata":platform.registry_token_metadata,
        "insecureRegistry":platform.insecure_registry,
        "ociCaPaths":platform.oci_ca_paths,
        "oauthEncryption":platform.oauth_encryption,
        "environmentNats":{
            "data":name::environment_nats_secret(org,env,"data"),
            "native":name::environment_nats_secret(org,env,"workflow-native"),
            "workflow":workflow,
            "ingress":ingress,
            "subjects":name::environment_event_subjects(org, env),
        },
    }))
}

/// Read every installed baseline, independent of the selected application release.
async fn installed_manifests(
    platform: &Platform,
    system: &PostgresSession,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_component_policy::PackageManifest>> {
    let mut manifests = Vec::new();
    for (id, baseline) in super::stage::installed(system, document).await? {
        let package = super::stage::open(platform, &id, &baseline.current).await?;
        manifests.push(package.read_manifest()?);
    }
    Ok(manifests)
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
