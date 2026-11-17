//! Explicit environment provisioning, shared credentials, and CDC resources.
//! Application activation reads these resources without creating or reconciling them.

use sqlx::Row as _;
pub(super) mod plan;
pub(super) use plan::management_author_pat_name;


use anyhow::Context as _;
use wamn_registry::Triple;

use super::Platform;
use super::document::EnvironmentDocument;
use crate::provision_project_env::read_project_env_instance;
use wamn_postgres::connect;


/// Remove one project's runtime access while leaving the other projects served.
pub(super) async fn suspend_project(platform: &Platform, triple: &Triple) -> anyhow::Result<()> {
    let name = wamn_registry::name::environment_host_name(&triple.org, &triple.env);
    let Some(mut values) = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &name,
    )
    .await?
    else {
        return Ok(());
    };
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    let plugins = group["plugins"]
        .as_array_mut()
        .context("environment host has no plugins")?;
    let postgres = plugins
        .iter_mut()
        .find(|plugin| plugin["id"] == "wamn-postgres")
        .context("environment host has no wamn-postgres plugin")?;
    let config = postgres["config"]
        .as_object_mut()
        .context("host has no project configuration")?;
    let retired: serde_json::Value = serde_json::from_str(
        config
            .get(triple.project.as_str())
            .and_then(serde_json::Value::as_str)
            .context("host has no project configuration")?,
    )?;
    let retired: std::collections::BTreeSet<_> = retired["logins"]
        .as_object()
        .context("project has no logins")?
        .values()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect();
    config.remove(triple.project.as_str());
    if config.is_empty() {
        values["runtime-operator"]["runtime"]["hostGroups"] = serde_json::json!([]);
        wamn_platform_install::environment::apply(
            &platform.target,
            &platform.chart.join("platform/environment"),
            &name,
            values,
        )
        .await?;
        super::drain::wait_stopped(platform, &name).await?;
        return wamn_kubernetes::helm::uninstall(&platform.target, &name).await;
    }
    postgres["secretFrom"] = postgres_secrets(&postgres["config"])?;
    let bound = values["runtime-operator"]["runtime"]["terminationGracePeriodSeconds"]
        .as_u64()
        .context("environment host has no shutdown bound")?;
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        values,
    )
    .await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(bound.saturating_add(15)),
        async {
            loop {
                let pods = super::analyse::read_pods(platform, &name).await?;
                let still_mounted =
                    pods["items"]
                        .as_array()
                        .context("host pod list has no items")?
                        .iter()
                        .any(|pod| {
                            !matches!(
                                pod["status"]["phase"].as_str(),
                                Some("Succeeded" | "Failed")
                            ) && pod["spec"]["volumes"].as_array().into_iter().flatten().any(
                                |volume| {
                                    volume["secret"]["secretName"]
                                        .as_str()
                                        .is_some_and(|name| retired.contains(name))
                                },
                            )
                        });
                if !still_mounted {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        },
    )
    .await
    .context("project still has active runtime resources")??;
    let mut values = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &name,
    )
    .await?
    .context("environment host disappeared while suspending project")?;
    let mut removed = vec![wamn_registry::credential_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )];
    removed.extend(
        wamn_provision::WorkloadRoleFamily::project_env_families().map(|family| {
            wamn_provision::workload_secret_name(
                family,
                &triple.org,
                &triple.project,
                &triple.env,
            )
        }),
    );
    if let Some(secrets) = values["credentialAccess"]["secrets"].as_array_mut() {
        secrets.retain(|secret| !removed.iter().any(|name| secret.as_str() == Some(name)));
    }
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        values,
    )
    .await
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

/// Publish each project's login locations in the upstream native plugin entry.
pub(super) async fn host_project(
    platform: &Platform,
    document: &EnvironmentDocument,
    connection: &wamn_postgres::ConnectionConfig,
) -> anyhow::Result<()> {
    let name = wamn_registry::name::environment_host_name(&document.org, &document.env);
    let mut values = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &name,
    )
    .await?
    .unwrap_or_else(|| serde_json::json!({}));
    let groups = &mut values["runtime-operator"]["runtime"]["hostGroups"];
    if groups.is_null() {
        *groups = serde_json::json!([]);
    }
    let groups = groups
        .as_array_mut()
        .context("host groups are not a list")?;
    if groups.is_empty() {
        let mut group =
            serde_json::to_value(super::write::host_group(platform.host_group.as_deref())?)?;
        group["name"] = serde_json::json!(name);
        groups.push(group);
    }
    let group = &mut groups[0];
    anyhow::ensure!(
        group["namespace"]
            .as_str()
            .is_none_or(|namespace| namespace == document.org.as_str())
            && connection.credentials.namespace == document.org.as_str(),
        "project login Secrets and host must share the environment namespace"
    );
    for (field, ca) in [
        (
            "volumes",
            serde_json::json!({"name":"identity-ca","configMap":{"name":"identity-ca"}}),
        ),
        (
            "volumeMounts",
            serde_json::json!({"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true}),
        ),
    ] {
        let entries = &mut group[field];
        if entries.is_null() {
            *entries = serde_json::json!([]);
        }
        let entries = entries
            .as_array_mut()
            .with_context(|| format!("host {field} must be a list"))?;
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry["name"] == "identity-ca")
        {
            *entry = ca;
        } else {
            entries.push(ca);
        }
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
    let app = wamn_provision::workload_secret_name(
        wamn_provision::WorkloadRoleFamily::App,
        &document.org,
        &document.project,
        &document.env,
    );
    let query = wamn_provision::workload_secret_name(
        wamn_provision::WorkloadRoleFamily::Query,
        &document.org,
        &document.project,
        &document.env,
    );
    let platform_login = wamn_provision::workload_role::platform_managed_role(&connection.database)
        .password_secret
        .name;
    let config = serde_json::json!({"host":connection.host,"port":connection.port,"database":connection.database,
        "logins":{"guest-sql":app,"dynamic-sql":query,"platform":platform_login},
        "guest_pool_max_size":guest_pool_max_size,
        "wait_timeout_ms":wait_timeout_ms,"statement_timeout_ms":statement_timeout_ms});
    postgres["workloadConfig"] = "deny".into();
    postgres["config"][document.project.as_str()] = config.to_string().into();
    postgres["secretFrom"] = postgres_secrets(&postgres["config"])?;
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        &name,
        values,
    )
    .await
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


/// Provision the environment's source and advisory streams, with the same
/// target, consumers and duplicate window as [`enable_cdc`], and no slot,
/// publication or reader. Without an event broker there is nothing to provision.
pub(super) async fn ensure_streams(
    platform: &Platform,
    document: &EnvironmentDocument,
    manifests: &[wamn_catalog::manifest::PackageManifest],
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
            .flat_map(wamn_catalog::manifest::PackageManifest::entity_pairs)
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

/// Slot creation uses the capture role; schema ownership grants no replication privilege.
pub(super) async fn prepare_cdc_with_connection(
    platform: &Platform,
    document: &EnvironmentDocument,
    capture: &str,
) -> anyhow::Result<()> {
    let instance =
        read_project_env_instance(&platform.system_database_url, &document.triple()).await?;
    let role: wamn_identity_protocol::LoginName = wamn_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    )
    .parse()?;
    let mut connection = connect(capture).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::sql::create_failover_slot_sql(role.as_str()),
    ))
    .execute(&mut connection)
    .await?;
    let name = wamn_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    wamn_kubernetes::kubectl::apply(&platform.target.kubeconfig,&platform.target.context,document.org.as_str(),&serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":document.org},"data":{"connection.json":capture}})).await
}
