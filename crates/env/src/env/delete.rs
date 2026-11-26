//! `env deactivate-project <coordinate> [--data]`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Each step accepts absent resources so interrupted deactivation can resume.
//! Under the lifecycle lock, test ownership and existing unfinished runs before
//! uninstalling application releases. Drain their hosts and test runs again.
//! Delete and drain the CDC reader before removing credentials or replication.
//! Retire the host and remove the project stream before retiring broker credentials.
//! Then remove the replication slot, Publication CR, and Database CR.
//! With `--data`, remove the instance roles.
//! Remove the credential store Secret and its account before the final transaction.
//! The final transaction marks the deployment inactive and removes its
//! app bindings and memberships.
//! Disable connection principals in a retained database.
//!
//! Helm preserves the Cluster, ObjectStore, and certificates unless deletion includes data.
//! With `--data`, remove kept generations after deactivation.
//! An absent or unreachable project database skips database-side cleanup.

use super::database::{DatabaseConnection, LifecycleLock};
use crate::env_sql_effects::{self, PostgresSession};

use anyhow::bail;

use wamn_registry::name::{cdc_object_name, project_env_database_name};
use wamn_registry::{Triple, name};

use super::Platform;
use super::analyse::app_releases;

/// Deactivate the project in env. With `data`, the database and its roles are
/// dropped too, and an unfinished run or unreachable project database does
/// not stop deactivation. The caller confirms `data`. Returns one line per step.
///
/// # Errors
///
/// When the lock is held, pods are still live after the drain bound, a run is
/// unfinished and `data` is false, the replication slot is active, the event
/// broker is needed and not configured, or a step fails.
pub(crate) async fn deactivate(
    platform: &mut Platform,
    triple: &Triple,
    data: bool,
) -> anyhow::Result<Vec<String>> {
    super::runtime::resolve_platform(platform)?;
    LifecycleLock::hold(
        DatabaseConnection::Direct(&platform.system_database_url),
        triple,
        async |lock| Box::pin(deactivate_locked(platform, triple, data, lock)).await,
    )
    .await
}

/// The steps of [`deactivate`] under the lifecycle lock.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered sequence of if-present steps, as §12.3 lists them"
)]
async fn deactivate_locked(
    platform: &Platform,
    triple: &Triple,
    data: bool,
    system: &PostgresSession,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let release = name::project_env_resource_name(org, project, env);
    let release_present = data && platform.cluster.project_release_exists(&release).await?;
    let mut lines = Vec::new();
    let Some((instance, drain_bound_seconds)) = system.deactivation_inputs(triple).await? else {
        if data {
            if release_present {
                wamn_platform_install::deployment::remove_database(
                    &platform.cluster,
                    &platform.chart,
                    &release,
                    data,
                )
                .await?;
                lines.push("uninstalled the project release".into());
            }
            lines.extend(
                platform
                    .cluster
                    .delete_kept_project_resources(triple, &release)
                    .await?,
            );
        } else {
            lines.push(format!("{triple} has no active deployment"));
        }
        return Ok(lines);
    };
    let database = project_env_database_name(org, project, env, &instance);

    platform
        .cluster
        .require_ownership(triple, &database)
        .await?;
    let apps = app_releases(platform, system, triple).await?;
    let cr_present = platform.cluster.database_exists(&database).await?;
    let database_managed = cr_present && (!data || release_present);
    let cluster_missing = data
        && database_managed
        && !platform
            .cluster
            .database_cluster_exists(&name::project_cluster(&database))
            .await?;
    let project_connection = (database_managed && !cluster_missing)
        .then(|| wamn_postgres::database::connection_for(triple, &instance));
    let project_lock = if cluster_missing {
        lines.push(format!(
            "--data: project database {database} unreachable (Cluster is absent). Deleted without the database-side steps."
        ));
        None
    } else if let Some(connection) = &project_connection {
        platform.prepare_database(connection).await?;
        let url = connection.to_json()?;
        match env_sql_effects::lock_env(DatabaseConnection::Delete { input: &url, data }, org, env)
            .await
        {
            Ok(lock) => Some(lock),
            Err(error) if data && env_sql_effects::database_unreachable(&error) => {
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
    let namespace = platform.cluster.namespace();
    let deleting = async {
        if let Some(lock) = &project_lock
            && !data
        {
            let unfinished = lock.session.unfinished(env, None).await?;
            anyhow::ensure!(
                unfinished.is_empty(),
                "{}; wait for the runs to finish or delete with --data",
                super::drain::describe(&unfinished)
            );
        }
        for name in apps.values() {
            platform.cluster.uninstall(name).await?;
            lines.push(format!("uninstalled {name}"));
        }
        super::ensure::suspend_project(platform, system, triple, Some(drain_bound_seconds)).await?;

        // Runtime state can change during the drain, outside lifecycle exclusion.
        if let Some(lock) = &project_lock {
            let unfinished = lock.session.unfinished(env, None).await;
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
        wamn_platform_install::deployment::delete_reader(
            &platform.cluster,
            &platform.chart,
            triple,
            reader_release,
        )
        .await?;
        lines.push(format!(
            "deleted the CDC reader of {triple} (or it was gone)"
        ));

        if let Some(broker) = super::broker::admin(platform, org, env).await? {
            let stream = name::stream_name(org, project, env);
            if platform
                .broker
                .remove(&platform.nats_url, &broker, &stream)
                .await?
            {
                lines.push(format!("deleted stream {stream}"));
            }
        }

        // Slot administration belongs to the capture login on the recorded cluster.
        if let Some(project_connection) = project_connection {
            let replication_name = cdc_object_name(org, project, env, &instance);
            let drop_capture = async {
                if env_sql_effects::capture_exists(&project_connection, &replication_name, data)
                    .await?
                {
                    let mut capture =
                        wamn_postgres::database::capture_configuration(project_connection)?;
                    capture.database = "postgres".into();
                    platform.prepare_database(&capture).await?;
                    env_sql_effects::drop_capture(&capture, &replication_name, data).await?;
                }
                Ok(())
            };
            match drop_capture.await {
                Ok(()) => {}
                Err(error) if data && env_sql_effects::database_unreachable(&error) => lines.push(format!(
                    "--data: project database {database} unreachable ({error:#}). Deleted without replication slot cleanup."
                )),
                Err(error) => return Err(error),
            }
        }

        if let Some(lock) = &project_lock
            && !data
        {
            lock.session.disable_connection_principals().await?;
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
        wamn_platform_install::deployment::remove_database(
            &platform.cluster,
            &platform.chart,
            &release,
            data,
        )
        .await?;
        lines.push("uninstalled the project release".into());
    }
    if data {
        lines.extend(
            platform
                .cluster
                .delete_kept_project_resources(triple, &release)
                .await?,
        );
        let logins: Vec<_> = wamn_identity_protocol::workload_role::WorkloadRoleFamily::ALL
            .into_iter()
            .map(wamn_identity_protocol::workload_role::WorkloadRoleFamily::login_name)
            .chain([wamn_role::CDC_LOGIN])
            .collect();
        platform
            .cluster
            .delete_database_certificate_secrets(&database, &logins)
            .await?;
    }

    let remaining = system.project_count(org, env).await?.saturating_sub(1);
    if remaining == 0 {
        let host = name::env_host_name(org, env);
        platform.cluster.uninstall(&host).await?;
    }
    super::broker::after_delete(platform, triple, remaining).await?;
    // Keep the registry inputs until the credential store and its account are gone.
    let secret = wamn_registry::credential_secret_name(org, project, env);
    if platform.cluster.delete_secret(&secret).await? {
        lines.push(format!(
            "secret \"{secret}\" deleted from {namespace} namespace"
        ));
    }
    lines.extend(system.deactivate_row(triple).await?);
    Ok(lines)
}
