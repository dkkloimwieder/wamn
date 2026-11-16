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
    let projects = values["projects"]
        .as_array_mut()
        .context("environment host has no project projection")?;
    projects.retain(|project| project.as_str() != Some(triple.project.as_str()));
    if projects.is_empty() {
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
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    for field in ["volumes", "volumeMounts"] {
        if let Some(entries) = group[field].as_array_mut() {
            entries.retain(|entry| {
                entry["name"] != format!("project-{}", triple.project)
                    && entry["name"] != format!("events-{}", triple.project)
            });
        }
    }
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
                                |volume| volume["name"] == format!("project-{}", triple.project),
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
        wamn_control_provision::WorkloadRoleFamily::project_env_families().map(|family| {
            wamn_control_provision::workload_secret_name(
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
        &wamn_control_provision::project_env_database_name(
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
    let role: wamn_identity_protocol::LoginName = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    )
    .parse()?;
    let mut connection = connect(capture).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::create_failover_slot_sql(role.as_str()),
    ))
    .execute(&mut connection)
    .await?;
    let name = wamn_control_provision::project_env_cdc_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    wamn_kubernetes::kubectl::apply(&platform.target.kubeconfig,&platform.target.context,document.org.as_str(),&serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":document.org},"data":{"connection.json":capture}})).await
}
