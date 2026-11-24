//! `env delete <coordinate> [--data]`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Each step accepts absent resources so an interrupted delete can resume.
//! Under the lifecycle lock, test ownership and existing unfinished runs before
//! uninstalling application releases. Drain their hosts and test runs again.
//! Delete and drain the CDC reader before removing credentials or replication.
//! Retire the host and remove the project stream before retiring broker credentials.
//! Then remove the replication slot, Publication CR, and Database CR.
//! With `--data`, remove the instance roles.
//! Remove the credential store Secret and its account before the final transaction.
//! That transaction removes connection references after external resources are gone.
//! Delete the applied configuration with the environment row
//! last and commit. Disable connection principals in a retained database.
//!
//! Helm preserves the Cluster, ObjectStore, and certificates unless deletion includes data.
//! With `--data`, remove kept generations even when the registry row is gone.
//! An absent or unreachable project database skips database-side cleanup.

use sqlx::{Connection as _, Row as _};
use wamn_postgres::LifecycleLock;
use wamn_registry::lifecycle_lock::LifecycleLocks as _;
mod resources;
mod rows;
pub use rows::delete_registry_rows;

use anyhow::{Context as _, bail};
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use std::time::Duration;
use wamn_provision::sql::drop_replication_slot_sql;

use wamn_provision::{cdc_object_name, project_env_database_name};
use wamn_registry::{Triple, instance_suffix::InstanceSuffix, name};

use super::Platform;
use super::analyse::application_releases;
use wamn_kubernetes::{
    client::{delete_named, resource},
    helm,
};
use wamn_postgres::connect;

/// Delete the environment. With `data`, the database and its roles are
/// dropped too, and an unfinished run or unreachable project database does
/// not stop the delete. The caller confirms `data`. Returns one line per step.
///
/// # Errors
///
/// When the lock is held, pods are still live after the drain bound, a run is
/// unfinished and `data` is false, the replication slot is active, the event
/// broker is needed and not configured, or a step fails.
pub async fn delete(
    platform: &Platform,
    triple: &Triple,
    data: bool,
) -> anyhow::Result<Vec<String>> {
    let mut platform = platform.clone();
    super::runtime::resolve_platform(&mut platform)?;
    LifecycleLock::hold(
        wamn_postgres::connect(&platform.system_database_url).await?,
        triple,
        async |lock| Box::pin(delete_locked(&platform, triple, data, lock)).await,
    )
    .await
}

/// The steps of [`delete`] under the lifecycle lock.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered sequence of if-present steps, as §12.3 lists them"
)]
async fn delete_locked(
    platform: &Platform,
    triple: &Triple,
    data: bool,
    system: &LifecycleLock,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let release = name::environment_resource_name(org, project, env);
    let release_present = data && resources::project_release_exists(platform, &release).await?;
    let mut lines = Vec::new();
    let Some((instance, drain_bound_seconds)) = sqlx::query_as::<_, (String, i32)>(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED,
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *system.client().await)
    .await
    .context("read the environment row")?
    else {
        if data {
            if release_present {
                resources::uninstall_project(platform, triple, data, &mut lines).await?;
            }
            lines.extend(
                resources::delete_kept_project_resources(platform, triple, &release).await?,
            );
        } else {
            lines.push(format!(
                "{triple} has no environment row: nothing else to delete"
            ));
        }
        return Ok(lines);
    };
    let instance: InstanceSuffix =
        wamn_postgres::decode_text(instance, 0).context("read the environment row")?;
    let database = project_env_database_name(org, project, env, &instance);

    resources::require_ownership(platform, triple, &database).await?;
    let applications = application_releases(platform, system, triple).await?;
    let target = &platform.target;
    let databases = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Database",
        "databases",
    );
    let cr_present = databases.get_opt(&database).await?.is_some();
    let database_managed = cr_present && (!data || release_present);
    let clusters = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Cluster",
        "clusters",
    );
    let cluster_missing = data
        && database_managed
        && clusters
            .get_opt(&name::project_cluster(&database))
            .await?
            .is_none();
    let project_connection = (database_managed && !cluster_missing)
        .then(|| wamn_platform_install::environment::connection_for(triple, &instance));
    let project_lock = if cluster_missing {
        lines.push(format!(
            "--data: project database {database} unreachable (Cluster is absent). Deleted without the database-side steps."
        ));
        None
    } else if let Some(connection) = &project_connection {
        platform.prepare_database(connection).await?;
        let url = connection.to_json()?;
        let lock = async {
            let client = connect_for_delete(super::analyse::connect_project(&url), data).await?;
            LifecycleLock::acquire(client, triple).await
        };
        match lock.await {
            Ok(lock) => Some(lock),
            Err(error) if data && database_unreachable(&error) => {
                lines.push(format!(
                    "--data: project database {database} unreachable ({error:#}). Deleted without the database-side steps."
                ));
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let namespace = platform.target.namespace.as_str();
    let deleting = async {
        if let Some(lock) = &project_lock
            && !data
        {
            let unfinished = super::drain::unfinished(&mut *lock.client().await, env, None).await?;
            anyhow::ensure!(
                unfinished.is_empty(),
                "{}; wait for the runs to finish or delete with --data",
                super::drain::describe(&unfinished)
            );
        }
        for name in applications.values() {
            let namespace = helm::org_release_namespace(&platform.target.namespace);
            helm::uninstall(&platform.target, &namespace, name, helm::Wait::Watcher).await?;
            lines.push(format!("uninstalled {name}"));
        }
        super::ensure::suspend_project(platform, system, triple, Some(drain_bound_seconds)).await?;

        // Runtime state can change during the drain, outside lifecycle exclusion.
        if let Some(lock) = &project_lock {
            let unfinished = super::drain::unfinished(&mut *lock.client().await, env, None).await;
            match unfinished {
                Ok(unfinished) if !unfinished.is_empty() && !data => bail!(
                    "{}; shared resources were retained",
                    super::drain::describe(&unfinished)
                ),
                Ok(unfinished) if !unfinished.is_empty() => {
                    lines.push(format!("--data: {}", super::drain::describe(&unfinished)));
                }
                Ok(_) => {}
                // The CR may still exist while its database is already gone.
                Err(error) if data => lines.push(format!("--data: runs not read: {error:#}")),
                Err(error) => return Err(error),
            }
        }

        let reader_release = (!data || release_present).then_some(release.as_str());
        resources::delete_reader(platform, triple, reader_release).await?;
        lines.push(format!(
            "deleted the CDC reader of {triple} (or it was gone)"
        ));

        if let Some(broker) = super::broker::admin(platform, org, env).await? {
            let stream = name::stream_name(org, project, env);
            if wamn_broker::streams::delete(&broker, &stream).await? {
                lines.push(format!("deleted stream {stream}"));
            }
        }

        // Slot administration belongs to the capture login on the recorded cluster.
        if let Some(project_connection) = project_connection {
            let slot = cdc_object_name(org, project, env, &instance);
            let drop_slot = async {
                let observer = wamn_postgres::database_configuration(
                    &project_connection.to_json()?,
                    "postgres",
                )?;
                let mut observer = connect_for_delete(connect(&observer), data).await?;
                if let Some(row) =
                    sqlx::query("SELECT active FROM pg_replication_slots WHERE slot_name=$1")
                        .bind(&slot)
                        .fetch_optional(&mut observer)
                        .await?
                {
                    if row.get::<bool, _>(0) {
                        bail!("replication slot is active");
                    }
                    let mut capture = wamn_platform_install::environment::capture_configuration(
                        project_connection,
                    )?;
                    capture.database = "postgres".into();
                    platform.prepare_database(&capture).await?;
                    let mut capture =
                        connect_for_delete(connect(&capture.to_json()?), data).await?;
                    sqlx::raw_sql(sqlx::AssertSqlSafe(drop_replication_slot_sql(&slot)))
                        .execute(&mut capture)
                        .await?;
                }
                Ok(())
            };
            match drop_slot.await {
                Ok(()) => {}
                Err(error) if data && database_unreachable(&error) => lines.push(format!(
                    "--data: project database {database} unreachable ({error:#}). Deleted without replication slot cleanup."
                )),
                Err(error) => return Err(error),
            }
        }

        if let Some(lock) = &project_lock
            && !data
        {
            let mut project = lock.client().await;
            let mut transaction = project.begin().await?;
            wamn_provision::bind_platform_principal(
                &mut transaction,
                wamn_provision::PlatformComponent::Provisioning,
            )
            .await?;
            wamn_postgres::identity::application::disable_connection_services(&mut transaction)
                .await
                .context("disable the retained database's connection principals")?;
            transaction.commit().await?;
        }
        Ok(())
    };
    if let Some(lock) = &project_lock {
        lock.protect(deleting).await?;
    } else {
        deleting.await?;
    }
    if let Some(lock) = project_lock {
        lock.release().await;
    }
    if release_present || (!data && cr_present) {
        resources::uninstall_project(platform, triple, data, &mut lines).await?;
    }
    if data {
        lines.extend(resources::delete_kept_project_resources(platform, triple, &release).await?);
        resources::delete_database_certificate_secrets(platform, &database).await?;
    }

    let remaining = super::broker::project_count(&mut *system.client().await, org, env)
        .await?
        .saturating_sub(1);
    if remaining == 0 {
        let host = name::environment_host_name(org, env);
        let namespace = helm::org_release_namespace(&platform.target.namespace);
        helm::uninstall(&platform.target, &namespace, &host, helm::Wait::Watcher).await?;
    }
    super::broker::after_delete(platform, triple, remaining).await?;
    // Keep the registry inputs until the credential store and its account are gone.
    let secret = wamn_registry::credential_secret_name(org, project, env);
    let secrets = Api::<Secret>::namespaced(target.client.client(), namespace);
    if delete_named(&secrets, &secret, true).await? {
        lines.push(format!("secret \"{secret}\" deleted from {namespace} namespace"));
    }
    lines.extend(delete_registry_rows(&mut *system.client().await, triple).await?);
    Ok(lines)
}

async fn connect_for_delete(
    connection: impl Future<Output = anyhow::Result<sqlx::PgConnection>>,
    data: bool,
) -> anyhow::Result<sqlx::PgConnection> {
    if data {
        tokio::time::timeout(Duration::from_secs(10), connection)
            .await
            .context("connect to project database")?
    } else {
        connection.await
    }
}

fn database_unreachable(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Io(_)) => true,
        Some(sqlx::Error::Database(error)) => error.code().as_deref() == Some("3D000"),
        _ => error.is::<tokio::time::error::Elapsed>(),
    }
}
