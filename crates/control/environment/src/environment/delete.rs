//! `env delete <coordinate> [--data]`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The one destructive verb, re-entrant: every step is "if present", so a run
//! that fails at any boundary is resumed by running it again. Under the
//! lifecycle lock, in order: uninstall the release chart, which starts the
//! drain; wait for the live set to be empty; refuse on a run pinned to a
//! release with no pod, unless `--data`; revoke the provisioning PATs; delete
//! the credential Secrets the verb owns; remove the environment's key from the
//! org's control administration Secret; delete the event streams; drop the
//! replication slot; delete the `Database` CR and wait until it is gone; with
//! `--data`, drop the instance's roles; delete the control rows of the tenant
//! and the environment's connections, then the environment row; after that
//! commit, delete the credential store Secret, its host account and the
//! credential files. A retained database keeps its projected rows.
//!
//! The CR keeps `databaseReclaimPolicy: retain`, so the database and its data
//! stay on the cluster, with the roles that own them, for manual recovery.
//! With `--data` the verb first patches the policy to `delete`, CloudNativePG
//! drops the database before the CR is gone, and then the roles go.

pub(super) mod plan;
use plan::{credential_files, stream_not_found};
pub use plan::{delete_control_rows_sql, instance_families, instance_roles};

use anyhow::{Context as _, bail};
use async_nats::jetstream::context::GetStreamErrorKind;
use async_nats::jetstream::{self, ErrorCode};
use wamn_control_provision::sql::drop_replication_slot_sql;
use wamn_control_provision::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind, workload_generation_role,
};
use wamn_control_provision::{
    CredentialGeneration, cdc_object_name, control_administration_secret_name, event_stream_name,
    project_env_database_name, render_control_administration_patch,
};
use wamn_control_registry::Triple;
use wamn_event_wire::delivery_advisory_stream;
use wamn_pg_core::{quote_ident, quote_literal};

use super::Platform;
use super::analyse::release_present;
use super::ensure::{kubectl, management_author_pat_name, project_url};
use super::lock::LifecycleLock;
use wamn_control_provision::host_credentials::HOST_CREDENTIALS;
use wamn_package_control::database::connect;
use wamn_release_control::release_chart;

/// How long CloudNativePG may take to finalize the `Database` CR.
const DATABASE_DELETE_TIMEOUT: &str = "--timeout=600s";

/// The control tables that key rows by `env_key`, each with its
/// immutability trigger.
pub const CONTROL_TABLES: [(&str, Option<&str>); 2] = [
    (
        "catalog.authoring_command_audit",
        Some("authoring_command_audit_immutable"),
    ),
    ("wamn_run.gate_reports", Some("gate_reports_immutable")),
];

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
    let name = release_chart::release_name(org, project, env)?;
    let mut lines = Vec::new();

    // The release chart, which starts the drain.
    if release_present(platform, &name).await? {
        release_chart::uninstall(&platform.target, &name).await?;
        lines.push(format!("uninstalled {name}"));
    }
    let live = super::drain::wait(platform, &name, None).await?;
    if !live.is_empty() {
        bail!(
            "host pods of {live:?} are still live after the drain bound; nothing else was deleted"
        );
    }

    // A lifecycle verb: the provisioning credential, as the install Jobs use.
    let mut system = connect(admin_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let Some(row) = system
        .query_opt(
            "SELECT instance_suffix FROM registry.project_envs \
              WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
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
    let env_key = wamn_control_provision::project_env_key(org, project, env);

    // A run pinned to a release with no pod.
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
    if cr_present {
        let url = project_url(platform, triple).await?;
        match super::drain::stranded(&url, &env_key, env, &live).await {
            Ok(stranded) if !stranded.is_empty() && !data => bail!(
                "{}; apply a document that names their release to recover them, or delete \
                 with --data; nothing else was deleted",
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

    // The provisioning PATs, then the credential Secrets the verb owns.
    let namespace = platform.target.namespace.as_str();
    let pat_secrets = [
        management_author_pat_name(triple),
        format!("wamn-pat-operator-{org}--{project}--{env}"),
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
    let mut secrets = vec![
        wamn_control_provision::project_env_secret_name(org, project, env),
        wamn_control_provision::project_env_cdc_secret_name(org, project, env),
    ];
    secrets.extend(HOST_CREDENTIALS.iter().map(|credential| {
        wamn_control_provision::workload_secret_name(credential.family, org, project, env)
    }));
    secrets.extend(pat_secrets);
    let mut arguments = vec!["delete", "secret", "--ignore-not-found"];
    arguments.extend(secrets.iter().map(String::as_str));
    let deleted = kubectl(platform, namespace, &arguments, None).await?;
    if !deleted.is_empty() {
        lines.push(deleted);
    }

    // The environment's key of the org's control administration Secret.
    let control = control_administration_secret_name(org);
    let control_present = !kubectl(
        platform,
        namespace,
        &[
            "get",
            "secret",
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
        let patch = serde_json::to_vec(&render_control_administration_patch(triple, None))?;
        kubectl(
            platform,
            namespace,
            &[
                "patch",
                "secret",
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
        lines.push(format!("removed the environment's key of Secret {control}"));
    }

    // The event streams, when the environment captures events.
    let reader = system
        .query_opt(
            "SELECT 1 FROM registry.event_readers WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
        .await
        .context("read the registry.event_readers row")?
        .is_some();
    if reader {
        let broker = platform.events.as_ref().context(
            "the environment captures events; set WAMN_NATS_URL and its WAMN_NATS_TLS_* \
             certificate to delete its streams",
        )?;
        let broker = jetstream::new(
            broker
                .nats
                .connect()
                .await
                .context("connect the event provisioning credential")?,
        );
        let source = event_stream_name(org, project, env);
        for stream in [delivery_advisory_stream(&source), source] {
            if delete_stream(&broker, &stream).await? {
                lines.push(format!("deleted stream {stream}"));
            }
        }
    }

    // The replication slot, on the cluster's `postgres` database.
    let mut postgres_url = url::Url::parse(admin_url).context("parse the admin URL")?;
    postgres_url.set_path("/postgres");
    let admin = connect(postgres_url.as_str()).await?;
    let slot = cdc_object_name(org, project, env, &instance);
    if let Some(row) = admin
        .query_opt(
            "SELECT active FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await
        .context("read the replication slot")?
    {
        if row.get::<_, bool>(0) {
            bail!("refused: replication slot {slot} is active: a reader still streams from it");
        }
        admin
            .batch_execute(&drop_replication_slot_sql(&slot))
            .await
            .context("drop the replication slot")?;
        lines.push(format!("dropped replication slot {slot}"));
    }

    // The Database CR: retained, or with --data, dropped with its database.
    if cr_present {
        if data {
            kubectl(
                platform,
                &platform.database_namespace,
                &[
                    "patch",
                    "database",
                    &database,
                    "--type",
                    "merge",
                    "-p",
                    r#"{"spec":{"databaseReclaimPolicy":"delete"}}"#,
                ],
                None,
            )
            .await
            .context("set the Database CR's reclaim policy to delete")?;
        }
        kubectl(
            platform,
            &platform.database_namespace,
            &[
                "delete",
                "database",
                &database,
                "--ignore-not-found",
                "--wait=true",
                DATABASE_DELETE_TIMEOUT,
            ],
            None,
        )
        .await
        .context("delete the Database CR")?;
        lines.push(if data {
            format!("deleted the Database CR {database} and dropped its database")
        } else {
            format!("deleted the Database CR {database}; the database is retained on the cluster")
        });
    }

    // The roles, only with the database: a retained database keeps its
    // owner roles for manual recovery.
    if data {
        let roles = instance_roles(triple, &instance, &env_key)?;
        for role in &roles {
            admin
                .batch_execute(&format!("DROP ROLE IF EXISTS {}", quote_ident(role)))
                .await
                .with_context(|| format!("drop role {role}"))?;
        }
        lines.push(format!("dropped {} roles (or they were gone)", roles.len()));
    }

    // The control rows of the tenant, then the environment row, last.
    let transaction = crate::provision_project_env::provisioning_transaction(&mut system)
        .await
        .context("begin the row delete")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    for (table, trigger) in CONTROL_TABLES {
        for statement in delete_control_rows_sql(table, trigger, &env_key) {
            transaction
                .execute(statement.as_str(), &[])
                .await
                .with_context(|| format!("delete the control rows of {table}"))?;
        }
    }
    let credentials = transaction
        .query(
            wamn_control_registry::sql::delete_environment_connections_sql(),
            &[&org, &project, &env],
        )
        .await
        .context("delete the environment's connections")?;
    transaction
        .execute(
            "DELETE FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the registry.project_envs row")?;
    let env_key_table: bool = transaction
        .query_one(
            "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.tenant_environments")?
        .get(0);
    if env_key_table {
        transaction
            .execute(
                "DELETE FROM catalog.tenant_environments \
                  WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4",
                &[&org, &project, &env, &instance],
            )
            .await
            .context("delete the catalog.tenant_environments row")?;
    }
    transaction
        .commit()
        .await
        .context("commit the row delete")?;
    let mut drops = wamn_platform_identity::connection_changes::ConnectionDrops::default();
    drops.env_key(&env_key);
    wamn_platform_identity::connection_changes::ConnectionChanges::from(
        &platform.auth_changes().await?,
    )
    .publish(drops)
    .await;
    lines.push(format!(
        "deleted the control rows of tenant {env_key}, {} connection credentials and the \
         environment row of {triple}",
        credentials.len()
    ));

    // The credential store, after the commit: the Secret and the host account
    // that reads it, and the credential files of a file target.
    let account = wamn_credential_store::kubernetes::service_account_name(org, project, env);
    let secret = wamn_credential_store::kubernetes::secret_name(org, project, env);
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
    let files = credential_files(
        credentials
            .iter()
            .map(|row| (row.get(1), row.get(2), row.get(3))),
    );
    if !files.is_empty() {
        let store =
            wamn_credential_store::open("file").context("open the file credential store")?;
        for (reference, version) in &files {
            store
                .destroy(reference, version)
                .await
                .with_context(|| format!("destroy credential file {reference}"))?;
        }
        lines.push(format!("destroyed {} credential files", files.len()));
    }
    Ok(lines)
}

/// Delete one stream; a stream delete removes its consumers. Returns whether
/// it was present.
async fn delete_stream(broker: &jetstream::Context, name: &str) -> anyhow::Result<bool> {
    match broker.delete_stream(name).await {
        Ok(_) => Ok(true),
        Err(error) if stream_not_found(error.kind()) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("delete stream {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_targets_destroy_credential_files() {
        let row = |reference: &str, target_type: &str| {
            (
                reference.to_owned(),
                reference.to_owned(),
                target_type.to_owned(),
            )
        };
        assert_eq!(
            credential_files([row("on-local", "local"), row("on-cloud", "cloud")]),
            [("on-local".to_owned(), "on-local".to_owned())]
        );
        assert!(credential_files([row("on-cloud", "cloud")]).is_empty());
    }

    fn triple() -> Triple {
        Triple::new("dkk", "receiving", "dev")
    }

    #[test]
    fn the_instance_roles_are_the_cdc_role_then_a_and_b_of_each_family() {
        let roles = instance_roles(&triple(), "zf7o454t", "dev").unwrap();
        // The CDC role, then a and b of the ten instance families.
        assert_eq!(instance_families().count(), 10);
        assert_eq!(roles.len(), 1 + 10 * 2);
        assert_eq!(roles[0], "wamn_cdc_dkk__receiving__dev__zf7o454t");
        assert!(roles.iter().all(|role| role.len() <= 63));
    }

    #[test]
    fn the_control_rows_go_with_the_trigger_off_for_one_delete() {
        let statements = delete_control_rows_sql(
            "catalog.authoring_command_audit",
            Some("authoring_command_audit_immutable"),
            "dev",
        );
        assert_eq!(statements.len(), 3);
        assert_eq!(
            statements[0],
            "ALTER TABLE catalog.authoring_command_audit DISABLE TRIGGER \"authoring_command_audit_immutable\""
        );
        assert!(statements[1].starts_with("DELETE FROM catalog.authoring_command_audit "));
        assert_eq!(
            statements[2],
            "ALTER TABLE catalog.authoring_command_audit ENABLE TRIGGER \"authoring_command_audit_immutable\""
        );
        let statements = delete_control_rows_sql("catalog.authoring_command_audit", None, "dev");
        assert_eq!(statements.len(), 1);
        assert!(statements[0].starts_with("DELETE FROM catalog.authoring_command_audit "));
    }
}
