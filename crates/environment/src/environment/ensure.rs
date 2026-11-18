//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use sqlx::Row as _;

use anyhow::Context as _;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple, name};

use super::Platform;
use super::document::EnvironmentDocument;
use crate::provision_project_env::read_project_env_instance;
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
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        values,
    )
    .await?;
    let connection =
        wamn_platform_install::environment::connection(&platform.system_database_url, triple)
            .await?;
    let retired = crate::provision_project_env::workload::connections(triple, &connection)?;
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
                            retired.iter().any(|(_, connection)| {
                                volume["secret"]["secretName"] == connection.credentials.name
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

fn postgres_secrets(config: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let mut secrets = std::collections::BTreeSet::new();
    for entry in config
        .as_object()
        .context("host project config is not a map")?
        .values()
    {
        let entry: serde_json::Value =
            serde_json::from_str(entry.as_str().context("project config is not JSON text")?)?;
        for secret in entry["logins"]
            .as_object()
            .context("project has no logins")?
            .values()
        {
            secrets.insert(
                secret
                    .as_str()
                    .context("login Secret is not a name")?
                    .to_owned(),
            );
        }
    }
    Ok(serde_json::to_value(secrets)?)
}

/// Set the stream ceiling from the projects that this group serves.
fn set_nats_plugin(
    plugins: &mut Vec<serde_json::Value>,
    org: &wamn_registry::OrgId,
    env: &wamn_registry::EnvName,
) -> anyhow::Result<()> {
    let projects = plugins
        .iter()
        .find(|plugin| plugin["id"] == "wamn-postgres")
        .and_then(|plugin| plugin["config"].as_object())
        .context("host has no project configuration")?;
    let streams = projects
        .keys()
        .map(|project| {
            let project = project.parse()?;
            Ok(name::stream_name(org, &project, env))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .join(",");
    let secret = name::environment_nats_secret(org, env);
    let directory = format!("/etc/wasmcloud/host-plugin-secrets/{secret}");
    let entry = serde_json::json!({
        "id": "wasmcloud-nats",
        "workloadConfig": "deny",
        "secretFrom": [secret],
        "config": {
            "servers": wamn_platform_install::platform::identity::NATS_URL,
            "tls-ca": format!("{directory}/ca.crt"),
            "tls-cert": format!("{directory}/tls.crt"),
            "tls-key": format!("{directory}/tls.key"),
            "stream-allow": streams,
            "subject-allow": name::environment_event_subjects(org, env),
        },
    });
    if let Some(plugin) = plugins
        .iter_mut()
        .find(|plugin| plugin["id"] == "wasmcloud-nats")
    {
        *plugin = entry;
    } else {
        plugins.push(entry);
    }
    Ok(())
}

/// Submit the shared host with both configured and newly reserved projects.
pub(super) async fn submit_host(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    policy: &EnvName,
    reserved: &[&str],
) -> anyhow::Result<String> {
    let name = name::environment_host_name(org, env);
    let values = host_values(platform, org, env, reserved, None, Some(policy)).await?;
    wamn_platform_install::environment::apply_without_wait(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        &values,
    )
    .await?;
    Ok(name)
}

pub(super) async fn wait_host_ready(platform: &Platform, host: &str) -> anyhow::Result<()> {
    kubectl(
        platform,
        &platform.target.namespace,
        &[
            "rollout",
            "status",
            &format!("deployment/hostgroup-{host}"),
            "--timeout=600s",
        ],
        None,
    )
    .await
    .map(drop)
    .with_context(|| format!("wait for environment host {host}"))
}

/// Wait for the shared certificate, including when its object has yet to appear.
pub(super) async fn wait_broker_certificate(
    platform: &Platform,
    triple: &Triple,
) -> anyhow::Result<()> {
    use std::time::Duration;
    let name = name::environment_nats_certificate(&triple.org, &triple.env);
    let target = &platform.target;
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let certificate = wamn_kubernetes::kubectl::get::<serde_json::Value>(
                &target.kubeconfig,
                &target.context,
                triple.org.as_str(),
                "certificate",
                &name,
            )
            .await
            .with_context(|| format!("read broker certificate {name}"))?;
            if certificate.is_some_and(|certificate| {
                certificate["status"]["conditions"]
                    .as_array()
                    .is_some_and(|conditions| {
                        conditions.iter().any(|condition| {
                            condition["type"] == "Ready" && condition["status"] == "True"
                        })
                    })
            }) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .with_context(|| format!("broker certificate {name} was not ready within 120 seconds"))?
}

async fn host_values(
    platform: &Platform,
    org: &OrgId,
    env: &EnvName,
    reserved: &[&str],
    excluded: Option<&ProjectId>,
    requested_policy: Option<&EnvName>,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::{Value, json};

    let name = name::environment_host_name(org, env);
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
        return Ok(json!({"runtime-operator":{"runtime":{"hostGroups":[]}}}));
    }

    let mut group =
        serde_json::to_value(super::write::host_group(platform.host_group.as_deref())?)?;
    group["name"] = json!(name);
    group["namespace"] = json!(org);
    let variables = group["env"]
        .as_array_mut()
        .context("host environment is not a list")?;
    variables.retain(|entry| entry["name"] != "WAMN_ORG");
    variables.push(json!({"name":"WAMN_ORG","value":org}));
    let mut arguments = group["extraArgs"]
        .take()
        .as_array_mut()
        .map(std::mem::take)
        .unwrap_or_default();
    arguments.retain(|argument| {
        !argument
            .as_str()
            .is_some_and(|s| s.starts_with("--environment="))
    });
    arguments.push(format!("--environment={}", env).into());
    group["extraArgs"] = arguments.into();
    for (field, ca) in [
        (
            "volumes",
            json!({"name":"identity-ca","configMap":{"name":"identity-ca"}}),
        ),
        (
            "volumeMounts",
            json!({"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true}),
        ),
    ] {
        let entries = &mut group[field];
        if entries.is_null() {
            *entries = json!([]);
        }
        let entries = entries
            .as_array_mut()
            .with_context(|| format!("host {field} must be a list"))?;
        entries.retain(|entry| entry["name"] != "identity-ca");
        entries.push(ca);
    }
    let defaults: serde_json::Value = serde_yaml::from_str(include_str!(
        "../../../../deploy/charts/environment/values.yaml"
    ))?;
    let base = wamn_sql::WamnPostgresConfig::from_env();
    let setting = |name: &str, fallback: u64| -> anyhow::Result<u64> {
        group["env"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                defaults["runtime-operator"]["runtime"]["env"]
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .find(|entry| entry["name"] == name)
            .map_or(Ok(fallback), |entry| {
                entry["value"]
                    .as_str()
                    .context("pool setting is not text")?
                    .parse()
                    .context("pool setting is not a number")
            })
    };
    let guest_pool_max_size = setting(
        "WAMN_PG_GUEST_POOL_MAX",
        u64::try_from(base.guest_pool_max_size)?,
    )?;
    let wait_timeout_ms = setting("WAMN_PG_WAIT_TIMEOUT_MS", base.wait_timeout_ms)?;
    let statement_timeout_ms = setting(
        "WAMN_PG_STATEMENT_TIMEOUT_MS",
        u64::from(base.statement_timeout_ms),
    )?;
    if group["plugins"].is_null() {
        group["plugins"] = serde_json::json!([]);
    }
    let plugins = group["plugins"]
        .as_array_mut()
        .context("host plugins are not a list")?;
    if !plugins.iter().any(|plugin| plugin["id"] == "wamn-postgres") {
        plugins.push(serde_json::json!({"id":"wamn-postgres","workloadConfig":"deny","config":{}}));
    }
    let postgres = plugins
        .iter_mut()
        .find(|plugin| plugin["id"] == "wamn-postgres")
        .expect("inserted plugin");
    let mut secrets = Vec::new();
    postgres["config"] = json!({});
    let mut add_project =
        |project: &Triple, connection: &wamn_postgres::ConnectionConfig| -> anyhow::Result<()> {
            anyhow::ensure!(
                connection.credentials.namespace == org.as_str(),
                "project login Secrets and host must share an org"
            );
            let mut logins = serde_json::Map::new();
            for (family, config) in
                crate::provision_project_env::workload::connections(project, connection)?
            {
                let plugin_role = match family {
                    wamn_provision::WorkloadRoleFamily::App => "guest-sql",
                    wamn_provision::WorkloadRoleFamily::Query => "dynamic-sql",
                    wamn_provision::WorkloadRoleFamily::Platform => "platform",
                    wamn_provision::WorkloadRoleFamily::Migration => continue,
                };
                logins.insert(plugin_role.into(), config.credentials.name.into());
            }
            secrets.push(name::credential_secret_name(
                &project.org,
                &project.project,
                &project.env,
            ));
            postgres["config"][project.project.as_str()] = json!({
                "host":connection.host,"port":connection.port,"database":connection.database,
                "logins":logins,"guest_pool_max_size":guest_pool_max_size,
                "wait_timeout_ms":wait_timeout_ms,"statement_timeout_ms":statement_timeout_ms,
            })
            .to_string()
            .into();
            Ok(())
        };
    for project in projects {
        let project = Triple::new(org.clone(), env.clone(), project);
        let connection =
            wamn_platform_install::environment::connection(&platform.system_database_url, &project)
                .await?;
        add_project(&project, &connection)?;
    }
    postgres["workloadConfig"] = "deny".into();
    postgres["secretFrom"] = postgres_secrets(&postgres["config"])?;
    secrets.extend(
        postgres["secretFrom"]
            .as_array()
            .context("host login Secrets")?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    set_nats_plugin(plugins, org, env)?;
    let account = name::environment_service_account(org, env);
    let certificate = name::environment_nats_certificate(org, env);
    let certificate = wamn_broker::authorization::certificate(
        &certificate,
        &name::environment_nats_secret(org, env),
        &name::environment_nats_user(org, env),
    );
    Ok(json!({
        "runtime-operator":{"runtime":{"hostGroups":[group],
            "terminationGracePeriodSeconds":policy.drain_bound_seconds,
            "serviceAccount":{"name":account}}},
        "credentialAccess":{"account":account,"secrets":secrets},
        "brokerCertificates":[certificate],
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
    let broker = wamn_broker::authorization::environment_context(
        &platform.target,
        triple,
        wamn_platform_install::platform::identity::NATS_URL,
    )
    .await?
    .context("environment broker credential is absent")?;
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
        &wamn_provision::project_env_database_name(
            &triple.org,
            &triple.project,
            &triple.env,
            &instance,
        ),
    )
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
