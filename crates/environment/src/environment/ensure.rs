//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use sqlx::Row as _;

use anyhow::Context as _;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple, name};

use super::Platform;
use super::document::EnvironmentDocument;
use wamn_postgres::connect;

/// Remove one project's runtime access and wait until its credentials are unmounted.
pub(super) async fn suspend_project(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let name = name::environment_host_name(&triple.org, &triple.env);
    let values = host_values(
        platform,
        &triple.org,
        &triple.env,
        &[],
        Some(&triple.project),
        None,
    )
    .await?;
    wamn_platform_install::environment::prepare_host_upgrade(
        &platform.target,
        &platform.chart,
        name.clone(),
        &values,
    )
    .await?
    .await?;
    let connection =
        wamn_platform_install::environment::connection(&platform.system_database_url, triple)
            .await?;
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let pods = super::analyse::read_pods(platform, &name).await?;
            let mounted = pods["items"]
                .as_array()
                .context("host pod list has no items")?
                .iter()
                .any(|pod| {
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
                                    .any(|source| {
                                        source["secret"]["name"] == connection.server_ca.name
                                    })
                        })
                });
            if !mounted {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
    .await
    .context("project still has active runtime resources")?
}

/// Prepare the host upgrade with both configured and newly reserved projects.
pub(super) async fn prepare_host_upgrade<'a>(
    platform: &'a Platform,
    org: &OrgId,
    env: &EnvName,
    policy: &EnvName,
    reserved: &[&str],
) -> anyhow::Result<(u32, impl Future<Output = anyhow::Result<()>> + use<'a>)> {
    let name = name::environment_host_name(org, env);
    let values = host_values(platform, org, env, reserved, None, Some(policy)).await?;
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
    let upgrade = wamn_platform_install::environment::prepare_host_upgrade(
        &platform.target,
        &platform.chart,
        name,
        &values,
    )
    .await?;
    Ok((replicas, upgrade))
}

async fn host_values(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    reserved: &[&str],
    excluded: Option<&ProjectId>,
    requested_policy: Option<&EnvName>,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;

    let mut system = connect(&platform.system_database_url).await?;
    let mut tx = crate::provision_project_env::provisioning_transaction(&mut system).await?;
    let projects: Vec<ProjectId> = sqlx::query_scalar(
        "SELECT project FROM registry.environment_projects WHERE org=$1 AND env=$2
         AND ($3::text IS NULL OR project<>$3)
         AND (schema_mode IS NOT NULL OR project=ANY($4)) ORDER BY project",
    )
    .bind(org)
    .bind(env)
    .bind(excluded)
    .bind(reserved)
    .fetch_all(&mut *tx)
    .await?;
    let policy = match requested_policy {
        Some(policy) => super::analyse::read_policy(&mut tx, org, policy).await?,
        None => {
            let policy = wamn_registry::deployment::environment_policy(&mut tx, org, env).await?;
            super::analyse::read_policy(&mut tx, org, &policy).await?
        }
    };
    tx.commit().await?;
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
    let image = wamn_platform_install::release_chart::image_set(&platform.chart)
        .await?
        .host;

    let mut eligible = serde_json::Map::new();
    for project in projects {
        let triple = Triple::new(org.clone(), env.clone(), project);
        let connection =
            wamn_platform_install::environment::connection(&platform.system_database_url, &triple)
                .await?;
        anyhow::ensure!(
            connection.credentials.namespace == org.as_str()
                && connection.server_ca.namespace == org.as_str(),
            "project login Secrets and host must share an org"
        );
        let mut logins = serde_json::Map::new();
        for (family, config) in crate::provision_project_env::workload::connections(&connection)? {
            let role = match family {
                wamn_provision::WorkloadRoleFamily::App => "guest-sql",
                wamn_provision::WorkloadRoleFamily::Query => "dynamic-sql",
                wamn_provision::WorkloadRoleFamily::Platform => "platform",
                wamn_provision::WorkloadRoleFamily::Migration => continue,
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
        "drainBoundSeconds":policy.drain_bound_seconds,
        "projects":eligible,
        "natsUrl":platform.nats_url,
        "identityIssuer":platform.identity_issuer,
        "registryTokenMetadata":platform.registry_token_metadata,
        "insecureRegistry":platform.insecure_registry,
        "ociCaPaths":platform.oci_ca_paths,
        "oauthEncryption":platform.oauth_encryption,
        "environmentNats":{
            "scheduler":name::environment_nats_secret(org,env,"scheduler"),
            "data":name::environment_nats_secret(org,env,"data"),
            "native":name::environment_nats_secret(org,env,"workflow-native"),
            "workflow":workflow,
            "ingress":ingress,
            "subjects":name::environment_event_subjects(org, env),
        },
    }))
}

/// Read every installed baseline, independent of the selected application release.
pub async fn installed_manifests(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_catalog::manifest::PackageManifest>> {
    let mut manifests = Vec::new();
    for (id, baseline) in super::stage::installed(platform, document).await? {
        let package = super::stage::open(platform, &id, &baseline.current).await?;
        let path = wamn_codegen::package_manifest_path(package.root());
        manifests.push(wamn_catalog::manifest::PackageManifest::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )?);
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
    wamn_broker::streams::provision(
        &broker,
        triple,
        events.stream_replicas,
        std::time::Duration::from_secs(events.dup_window_secs),
        &[],
    )
    .await
}

/// Whether the environment's CDC is in place, judged by what it observes: the
/// publication `cdc_name` captures exactly `schemas` in database `project`, the
/// connection the probe runs on.
///
/// # Errors
///
/// When the probe fails.
pub async fn cdc_in_place(
    project: &mut sqlx::PgConnection,
    cdc_name: &str,
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
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

pub(super) async fn postgres_settings(platform: &Platform) -> anyhow::Result<serde_json::Value> {
    let configuration: serde_json::Value = wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        "configmap",
        "wamn-platform",
    )
    .await?
    .context("installation configuration is absent")?;
    let postgres = configuration["data"]["postgres.json"]
        .as_str()
        .context("installation PostgreSQL settings are absent")?;
    serde_json::from_str(postgres).context("decode installation PostgreSQL settings")
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

/// Resolve the schemas already recorded in the project database.
pub(super) async fn publication_schemas(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Vec<wamn_catalog::canonical_name::CanonicalName>> {
    Ok(installed_manifests(platform, document)
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

/// Wait for the chart's publication before creating the failover slot.
pub(super) async fn prepare_cdc(
    owner: &str,
    capture: &str,
    database: &str,
    publication: &str,
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
) -> anyhow::Result<()> {
    let mut project = connect(owner).await?;
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        while !cdc_in_place(&mut project, publication, schemas).await? {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| format!("wait for publication {publication}"))??;
    super::cdc_reader::apply_cdc_sql(owner, capture, schemas, publication, database).await
}
