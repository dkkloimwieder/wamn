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
use crate::provision_project_env::read_project_env_instance;
use wamn_postgres::connect;

/// How long one CR may take to report `status.applied`.
const APPLIED_TIMEOUT: &str = "--timeout=120s";

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
        return wamn_release_control::release_chart::uninstall(&platform.target, &name).await;
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

pub(crate) async fn ensure_cdc_inputs(
    platform: &Platform,
    document: &EnvironmentDocument,
    administrator: Option<&str>,
) -> anyhow::Result<()> {
    let manifests = installed_manifests(platform, document).await?;
    let schemas = manifests
        .iter()
        .flat_map(|manifest| {
            manifest.models.values().map(|model| {
                wamn_catalog::canonical_name::CanonicalName::from(model.schema.clone())
            })
        })
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
            if !full.insert((model.schema.to_string(), model.table.to_string())) {
                continue;
            }
            let sql = crate::replica_identity::alter_replica_identity_sql(
                &model.schema.clone().into(),
                &model.table.clone().into(),
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
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
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
