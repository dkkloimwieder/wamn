//! `env delete <coordinate> [--data]`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Each step accepts absent resources so an interrupted delete can resume.
//! Under the lifecycle lock, test ownership and existing stranded runs before
//! uninstalling application releases. Drain their hosts and test runs again.
//! Delete and drain the CDC reader before removing credentials or replication.
//! Remove the environment host, PATs, Secrets, control administration key, streams,
//! event certificate, Publication CR, replication slot, and Database CR.
//! Remove control logins and, with `--data`, the instance roles.
//! Remove the credential store Secret and its account before the final transaction.
//! That transaction removes connection references after external resources are gone.
//! Delete the applied configuration with the environment row
//! last and commit. A retained database keeps its rows.
//!
//! The CR keeps `databaseReclaimPolicy: retain`, so the database and its data
//! stay on the cluster, with the roles that own them, for manual recovery.
//! With `--data` the verb first patches the policy to `delete`, CloudNativePG
//! drops the database before the CR is gone, and then the roles go.

use sqlx::Row as _;
pub(super) mod plan;
mod resources;
mod roles;
mod rows;
pub use plan::{instance_families, instance_roles};
pub use rows::delete_registry_rows;

use anyhow::{Context as _, bail};
use async_nats::jetstream;
use wamn_control_provision::sql::drop_replication_slot_sql;
use wamn_control_provision::workload_role::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, workload_login,
};
use wamn_control_provision::{
    cdc_object_name, control_administration_secret_name, project_env_database_name,
};
use wamn_event_wire::delivery_advisory_stream;
use wamn_registry::Triple;

use super::Platform;
use super::analyse::application_releases;
use super::ensure::{kubectl, management_author_pat_name, project_url};
use super::lock::LifecycleLock;
use wamn_postgres::connect;
use wamn_release_control::release_chart;

/// Delete the environment. With `data`, the database and its roles are
/// dropped too, and a stranded run does not stop the delete. The caller
/// confirms `data`. Returns what the verb did, one line per step.
///
/// # Errors
///
/// When the lock is held, pods are still live after the drain bound, a run is
/// stranded and `data` is false, the replication slot is active, the event
/// broker is needed and not configured, or a step fails.
pub async fn delete(
    platform: &Platform,
    triple: &Triple,
    data: bool,
    admin_url: &str,
) -> anyhow::Result<Vec<String>> {
    LifecycleLock::hold(&platform.system_database_url, triple, async |_| {
        delete_locked(platform, triple, data, admin_url).await
    })
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
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
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
    let instance: String = row.get(0);
    let database = project_env_database_name(org, project, env, &instance);
    let project_env_key = wamn_control_provision::project_env_key(org, project, env);

    resources::require_ownership(platform, triple, &database).await?;
    let applications = application_releases(platform, triple).await?;
    let observed = wamn_kubernetes::environment_state::observe_environment(
        &platform.target.namespace,
        &triple.to_string(),
        |path| async move {
            let body = kubectl(
                platform,
                &platform.target.namespace,
                &["get", "--raw", &path],
                None,
            )
            .await?;
            serde_json::from_str::<serde_json::Value>(&body).map_err(anyhow::Error::from)
        },
    )
    .await?;
    let cr_present = !kubectl(
        platform,
        &platform.database_namespace,
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
    if cr_present && !data {
        let url = project_url(platform, triple).await?;
        let live = wamn_kubernetes::environment_state::live_set(&observed.pods);
        let stranded = super::drain::stranded(&url, &project_env_key, env, None, &live).await?;
        anyhow::ensure!(
            stranded.is_empty(),
            "{}; apply a document that names their release to recover them, or delete \
             with --data; nothing was deleted",
            super::drain::describe(&stranded)
        );
    }
    for name in applications.values() {
        release_chart::uninstall(&platform.target, name).await?;
        lines.push(format!("uninstalled {name}"));
    }
    // Pods can outlive their Helm record after an interrupted uninstall.
    let mut groups: std::collections::BTreeSet<_> = applications.values().cloned().collect();
    groups.extend(wamn_kubernetes::environment_state::application_states(&observed).into_keys());
    for name in groups {
        let live = super::drain::wait(platform, &name, None).await?;
        if !live.is_empty() {
            bail!(
                "application host group {name} is still live: {live:?}; shared resources were retained"
            );
        }
    }
    let live = std::collections::BTreeMap::new();

    // Runtime state can change during the drain, outside lifecycle exclusion.
    if cr_present {
        let url = project_url(platform, triple).await?;
        match super::drain::stranded(&url, &project_env_key, env, None, &live).await {
            Ok(stranded) if !stranded.is_empty() && !data => bail!(
                "{}; apply a document that names their release to recover them, or delete \
                 with --data; shared resources were retained",
                super::drain::describe(&stranded)
            ),
            Ok(stranded) if !stranded.is_empty() => {
                lines.push(format!("--data: {}", super::drain::describe(&stranded)));
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

    // Shared event resources stop only after every application host drains.
    let shared = super::ensure::environment_host_name(org, project, env)?;
    if super::analyse::release_present(platform, &shared).await? {
        release_chart::uninstall(&platform.target, &shared).await?;
        lines.push(format!("uninstalled {shared}"));
    }
    anyhow::ensure!(
        super::drain::wait(platform, &shared, None)
            .await?
            .is_empty(),
        "shared environment hosts are still live"
    );

    // The provisioning PATs, then the credential Secrets the verb owns.
    let namespace = platform.target.namespace.as_str();
    let pat_secrets = [
        management_author_pat_name(triple),
        wamn_registry::name::operator_secret(org, project, env),
    ];
    for secret in &pat_secrets {
        let prefix = kubectl(
            platform,
            namespace,
            &[
                "get",
                "secret",
                secret,
                "--ignore-not-found",
                "-o",
                "jsonpath={.metadata.annotations.wamn\\.io/pat-prefix}",
            ],
            None,
        )
        .await?;
        if !prefix.is_empty() {
            crate::provision_project_env::revoke_provisioning_pat(
                admin_url,
                &prefix,
                &platform.auth_changes().await?,
            )
            .await
            .with_context(|| format!("revoke the PAT of {secret}"))?;
            lines.push(format!("revoked the PAT of {secret}"));
        }
    }
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
            &serde_json::json!({"data":{wamn_control_provision::control_administration_key(&triple.project,&triple.env):null}}),
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
        let policy = super::analyse::read_policy(&mut system, org, row.get(1)).await?;
        crate::platform::control::wait_administration(
            &platform.target,
            triple,
            false,
            std::time::Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?),
        )
        .await?;
        lines.push(format!("removed the environment's key of Secret {control}"));
    }

    // Provisioning creates streams even before the reader is registered.
    let broker = platform.events.as_ref().context(
        "environment provisioning creates event streams; set WAMN_NATS_URL and its \
         WAMN_NATS_TLS_* certificate to delete them",
    )?;
    let broker = jetstream::new(
        broker
            .nats
            .connect()
            .await
            .context("connect the event provisioning credential")?,
    );
    let source = wamn_event_wire::stream_name(org, project, env);
    for stream in [delivery_advisory_stream(&source), source] {
        if wamn_broker::streams::delete(&broker, &stream).await? {
            lines.push(format!("deleted stream {stream}"));
        }
    }
    resources::delete_event_certificate(platform, &project_env_key).await?;
    lines.push(format!(
        "deleted the event certificate and Secret of {triple} (or they were gone)"
    ));
    resources::delete_publication(platform, &database).await?;
    lines.push(format!("deleted publication {database} (or it was gone)"));

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
        if let Some(row) = sqlx::query("SELECT active FROM pg_replication_slots WHERE slot_name=$1")
            .bind(&slot)
            .fetch_optional(&mut observer)
            .await?
        {
            if row.get::<bool, _>(0) {
                bail!("replication slot is active");
            }
            let capture =
                wamn_platform_install::environment::capture_connection(admin_url, triple).await?;
            let mut capture = connect(&capture).await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(drop_replication_slot_sql(&slot)))
                .execute(&mut capture)
                .await?;
        }
    }

    if cr_present {
        wamn_platform_install::environment::remove_database(
            &platform.target,
            &platform.chart,
            admin_url,
            triple,
            data,
        )
        .await?;
        lines.push("removed the chart-owned database resource".into());
    }

    // The roles, only with the database: a retained database keeps its
    // owner roles for manual recovery.
    roles::delete_control_roles(admin_url, triple, platform).await?;
    lines.push("deleted the environment's control logins (or they were gone)".into());
    if data {
        let mut roles = instance_roles(triple, &instance, &project_env_key)?;
        roles.push(wamn_control_provision::migrations::MigrationDatabase::Project.login(&database));
        let project_connection =
            wamn_platform_install::environment::connection(admin_url, triple).await?;
        let cluster = project_connection
            .server_ca
            .name
            .strip_suffix("-ca")
            .context("project cluster CA")?;
        crate::provision_project_env::workload::remove_roles(
            &platform.system_database_url,
            &platform.target.kubeconfig,
            &platform.target.context,
            org,
            cluster,
            &roles,
        )
        .await?;
        lines.push(format!("dropped {} roles (or they were gone)", roles.len()));
    }

    let configurations = [
        wamn_control_provision::project_env_secret_name(org, project, env),
        wamn_control_provision::project_env_cdc_secret_name(org, project, env),
    ];
    let mut arguments = vec!["delete", "configmap", "--ignore-not-found"];
    arguments.extend(configurations.iter().map(String::as_str));
    let deleted = kubectl(platform, org, &arguments, None).await?;
    if !deleted.is_empty() {
        lines.push(deleted);
    }
    let mut secrets = WorkloadRoleFamily::project_env_families()
        .map(|family| wamn_control_provision::workload_secret_name(family, org, project, env))
        .collect::<Vec<_>>();
    secrets.push(wamn_control_provision::project_env_cdc_secret_name(
        org, project, env,
    ));
    secrets.extend(pat_secrets);
    let mut arguments = vec!["delete", "secret", "--ignore-not-found"];
    arguments.extend(secrets.iter().map(String::as_str));
    let deleted = kubectl(platform, org, &arguments, None).await?;
    if !deleted.is_empty() {
        lines.push(deleted);
    }

    // Keep the registry inputs until the credential store and its account are gone.
    let account = wamn_registry::service_account_name(org, project, env);
    let secret = wamn_registry::credential_secret_name(org, project, env);
    for (resource, object) in [
        ("secret", &secret),
        ("rolebinding", &account),
        ("role", &account),
        ("serviceaccount", &account),
    ] {
        let deleted = kubectl(
            platform,
            namespace,
            &["delete", resource, object, "--ignore-not-found"],
            None,
        )
        .await?;
        if !deleted.is_empty() {
            lines.push(deleted);
        }
    }
    // Resolve the publisher before the final commit, while retry inputs remain.
    let changes = platform.auth_changes().await?;
    lines.extend(delete_registry_rows(&mut system, triple, &instance).await?);
    let mut drops = wamn_broker::connection_changes::ConnectionDrops::default();
    drops.project_env_key(&project_env_key);
    changes.publish(drops).await;
    Ok(lines)
}
