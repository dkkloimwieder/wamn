//! `env delete <coordinate> [--data]`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Each step accepts absent resources so an interrupted delete can resume.
//! Under the lifecycle lock, test ownership and existing unfinished runs before
//! uninstalling application releases. Drain their hosts and test runs again.
//! Delete and drain the CDC reader before removing credentials or replication.
//! Retire the host and remove the project stream before retiring broker credentials.
//! Then remove the replication slot, Publication CR, and Database CR.
//! Remove control logins and, with `--data`, the instance roles.
//! Remove the credential store Secret and its account before the final transaction.
//! That transaction removes connection references after external resources are gone.
//! Delete the applied configuration with the environment row
//! last and commit. Disable connection principals in a retained database.
//!
//! Helm preserves the Cluster and ObjectStore unless deletion includes data.

use sqlx::{Connection as _, Row as _};
mod resources;
mod rows;
pub use rows::delete_registry_rows;

use anyhow::{Context as _, bail};
use wamn_provision::sql::drop_replication_slot_sql;

use wamn_provision::{
    cdc_object_name, control_administration_secret_name, project_env_database_name,
};
use wamn_registry::{Triple, name};

use super::Platform;
use super::analyse::application_releases;
use super::ensure::{kubectl, project_url};
use wamn_broker::authorization::{ADMIN_CREDENTIAL_DIR, admin_context, environment_account_exists};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_kubernetes::helm;
use wamn_postgres::connect;

/// Delete the environment. With `data`, the database and its roles are
/// dropped too, and an unfinished run does not stop the delete. The caller
/// confirms `data`. Returns what the verb did, one line per step.
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
    admin_url: &str,
) -> anyhow::Result<Vec<String>> {
    let mut platform = platform.clone();
    super::runtime::resolve_platform(&mut platform)?;
    LifecycleLock::hold(
        wamn_postgres::connect(&platform.system_database_url).await?,
        triple,
        async |_| Box::pin(delete_locked(&platform, triple, data, admin_url)).await,
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
    admin_url: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let mut lines = Vec::new();
    // A lifecycle verb: the provisioning credential, as the install Jobs use.
    let mut system = connect(admin_url).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)
        .context("SET ROLE wamn_system")?;
    let Some(row) =
        sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED)
            .bind(org)
            .bind(project)
            .bind(env)
            .fetch_optional(&mut system)
            .await
            .context("read the environment row")?
    else {
        lines.push(format!(
            "{triple} has no environment row: nothing else to delete"
        ));
        return Ok(lines);
    };
    let instance: wamn_registry::instance_suffix::InstanceSuffix = row.get(0);
    let database = project_env_database_name(org, project, env, &instance);

    resources::require_ownership(platform, triple, &database).await?;
    let applications = application_releases(platform, triple).await?;
    let cr_present = !kubectl(
        platform,
        &platform.target.namespace,
        &[
            "get",
            "database",
            &database,
            "--ignore-not-found",
            "-o",
            "name",
        ],
        None,
    )
    .await?
    .is_empty();
    let project_lock = if cr_present {
        let url = project_url(platform, triple).await?;
        match super::analyse::connect_project(&url).await {
            Ok(client) => Some(LifecycleLock::acquire(client, triple).await?),
            Err(error)
                if data
                    && error
                        .downcast_ref::<sqlx::Error>()
                        .and_then(sqlx::Error::as_database_error)
                        .and_then(sqlx::error::DatabaseError::code)
                        .as_deref()
                        == Some("3D000") =>
            {
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
        super::ensure::suspend_project(platform, triple).await?;

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

        resources::delete_reader(platform, triple).await?;
        lines.push(format!(
            "deleted the CDC reader of {triple} (or it was gone)"
        ));

        // The environment's key of the org's control administration Secret.
        let control = control_administration_secret_name(org);
        let control_present = !kubectl(
            platform,
            namespace,
            &[
                "get",
                "configmap",
                &control,
                "--ignore-not-found",
                "-o",
                "name",
            ],
            None,
        )
        .await?
        .is_empty();
        if control_present {
            let patch = serde_json::to_vec(
                &serde_json::json!({"data":{wamn_provision::control_administration_key(&triple.project,&triple.env):null}}),
            )?;
            kubectl(
                platform,
                namespace,
                &[
                    "patch",
                    "configmap",
                    &control,
                    "--type",
                    "merge",
                    "--patch-file",
                    "/dev/stdin",
                ],
                Some(&patch),
            )
            .await
            .with_context(|| format!("remove the environment's key of Secret {control}"))?;
            let policy_name = row.try_get(1)?;
            let policy = super::analyse::read_policy(&mut system, org, &policy_name).await?;
            crate::platform::control::wait_administration(
                &platform.target,
                triple,
                false,
                std::time::Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?),
            )
            .await?;
            lines.push(format!("removed the environment's key of Secret {control}"));
        }

        if environment_account_exists(&platform.target, triple).await? {
            let broker = admin_context(
                triple,
                wamn_platform_install::platform::identity::NATS_URL,
                std::path::Path::new(ADMIN_CREDENTIAL_DIR),
            )
            .await?;
            let stream = name::stream_name(org, project, env);
            if wamn_broker::streams::delete(&broker, &stream).await? {
                lines.push(format!("deleted stream {stream}"));
            }
        }


        // Slot administration belongs to the capture login on the recorded cluster.
        if cr_present {
            let project_connection =
                wamn_platform_install::environment::connection(admin_url, triple).await?;
            let mut observer = connect(&wamn_postgres::database_configuration(
                &project_connection.to_json()?,
                "postgres",
            )?)
            .await?;
            let slot = cdc_object_name(org, project, env, &instance);
            if let Some(row) =
                sqlx::query("SELECT active FROM pg_replication_slots WHERE slot_name=$1")
                    .bind(&slot)
                    .fetch_optional(&mut observer)
                    .await?
            {
                if row.get::<bool, _>(0) {
                    bail!("replication slot is active");
                }
                let capture =
                    wamn_platform_install::environment::capture_connection(admin_url, triple)
                        .await?;
                let mut capture = connect(&capture).await?;
                sqlx::raw_sql(sqlx::AssertSqlSafe(drop_replication_slot_sql(&slot)))
                    .execute(&mut capture)
                    .await?;
            }
        }


        if let Some(lock) = &project_lock
            && !data
        {
            let mut project = lock.client().await;
            let mut transaction = project.begin().await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                wamn_provision::bind_platform_principal_sql(
                    wamn_provision::PlatformComponent::Provisioning,
                ),
            ))
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"UPDATE platform."user" u SET status='disabled'
                   FROM platform.connection c
                   WHERE u.id=c.service_principal_id AND u.issuer='wamn'
                     AND u.type='service' AND u.status<>'disabled'"#,
            )
            .execute(&mut *transaction)
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
    if cr_present {
        wamn_platform_install::environment::remove_database(
            &platform.target,
            &platform.chart,
            triple,
            data,
        )
        .await?;
        lines.push("uninstalled the project release".into());
    }

    let last = wamn_broker::authorization::remove_project(&platform.target, triple).await?;
    resources::delete_event_certificates(platform, triple, last).await?;
    // Keep the registry inputs until the credential store and its account are gone.
    let secret = wamn_registry::credential_secret_name(org, project, env);
    let deleted = kubectl(
        platform,
        namespace,
        &["delete", "secret", &secret, "--ignore-not-found"],
        None,
    )
    .await?;
    if !deleted.is_empty() {
        lines.push(deleted);
    }
    lines.extend(delete_registry_rows(&mut system, triple).await?);
    Ok(lines)
}
