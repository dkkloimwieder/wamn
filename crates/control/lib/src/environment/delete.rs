//! `env delete <coordinate> [--data]` (docs/plan/platform-deploy.md §12.3,
//! R19, R20 (5)).
//!
//! The one destructive verb, re-entrant: every step is "if present", so a run
//! that fails at any boundary is resumed by running it again. Under the
//! lifecycle lock, in order: uninstall the release chart, which starts the
//! drain; wait for the live set to be empty; refuse on a run pinned to a
//! release with no pod, unless `--data`; revoke the provisioning PATs; delete
//! the credential Secrets the verb owns; remove the environment's key from the
//! org's control administration Secret; delete the event streams; drop the
//! replication slot; delete the `Database` CR and wait until it is gone; with
//! `--data`, drop the instance's roles; delete the control rows of the tenant;
//! delete the environment row last.
//!
//! The CR keeps `databaseReclaimPolicy: retain`, so the database and its data
//! stay on the cluster, with the roles that own them, for manual recovery.
//! With `--data` the verb first patches the policy to `delete`, CloudNativePG
//! drops the database before the CR is gone, and then the roles go.

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
use super::analyse::{connect, release_present};
use super::ensure::{HOST_FAMILIES, kubectl, management_author_pat_name, project_url};
use super::lock::LifecycleLock;
use crate::release_chart;

/// How long CloudNativePG may take to finalize the `Database` CR.
const DATABASE_DELETE_TIMEOUT: &str = "--timeout=600s";

/// The control tables that key rows by `tenant_id`, leaves first, each with
/// its immutability trigger. The foreign keys have no `ON DELETE` clause, so
/// this order is the one that deletes.
pub const CONTROL_TABLES: [(&str, Option<&str>); 9] = [
    (
        "catalog.deployment_attestations",
        Some("deployment_attestations_immutable"),
    ),
    (
        "catalog.connection_requirements",
        Some("connection_requirements_immutable"),
    ),
    (
        "catalog.component_library",
        Some("component_library_immutable"),
    ),
    (
        "catalog.effective_release_packages",
        Some("effective_release_packages_immutable"),
    ),
    (
        "catalog.effective_releases",
        Some("effective_releases_immutable"),
    ),
    (
        "catalog.package_migrations",
        Some("package_migrations_immutable"),
    ),
    ("catalog.packages", Some("packages_immutable")),
    (
        "catalog.authoring_command_audit",
        Some("authoring_command_audit_immutable"),
    ),
    ("wamn_run.gate_reports", Some("gate_reports_immutable")),
];

/// The statements that delete one tenant's rows of one control table. An
/// immutable table gets its trigger off for the one delete only.
pub fn delete_control_rows_sql(table: &str, trigger: Option<&str>, tenant: &str) -> Vec<String> {
    let delete = format!(
        "DELETE FROM {table} WHERE tenant_id = {}",
        quote_literal(tenant)
    );
    match trigger {
        Some(trigger) => vec![
            format!(
                "ALTER TABLE {table} DISABLE TRIGGER {}",
                quote_ident(trigger)
            ),
            delete,
            format!(
                "ALTER TABLE {table} ENABLE TRIGGER {}",
                quote_ident(trigger)
            ),
        ],
        None => vec![delete],
    }
}

/// The families whose generation roles name the instance's database: a
/// control or org family's scope names the control database, so its roles
/// stay with the triple or the org.
pub fn instance_families() -> impl Iterator<Item = WorkloadRoleFamily> {
    WorkloadRoleFamily::ALL.into_iter().filter(|family| {
        matches!(
            family.scope_kind(),
            WorkloadRoleScopeKind::Tenant | WorkloadRoleScopeKind::ProjectEnvironment
        )
    })
}

/// The roles of one instance: the CDC role, then `a` and `b` of each
/// instance family.
///
/// # Errors
///
/// When a role name cannot be derived.
pub fn instance_roles(
    triple: &Triple,
    instance: &str,
    tenant: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let database = project_env_database_name(org, project, env, instance);
    let mut roles = vec![cdc_object_name(org, project, env, instance)];
    for family in instance_families() {
        let scope = match family.scope_kind() {
            WorkloadRoleScopeKind::Tenant => WorkloadRoleScope::Tenant {
                tenant,
                database: &database,
            },
            _ => WorkloadRoleScope::ProjectEnvironment {
                org,
                project,
                environment: env,
                database: &database,
            },
        };
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            roles.push(
                workload_generation_role(family, scope, generation)
                    .with_context(|| format!("derive the {family:?} generation role"))?,
            );
        }
    }
    Ok(roles)
}

/// Delete the environment. With `data`, the database and its roles are
/// dropped too, and a stranded run does not stop the delete. The caller
/// confirms `data`. Returns what the verb did, one line per step.
///
/// # Errors
///
/// When the lock is held, pods are still live after the drain bound, a run is
/// stranded and `data` is false, the replication slot is active, the event
/// broker is needed and not configured, or a step fails.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered sequence of if-present steps, as §12.3 lists them"
)]
pub async fn delete(
    platform: &Platform,
    triple: &Triple,
    data: bool,
) -> anyhow::Result<Vec<String>> {
    let _lock = LifecycleLock::acquire(&platform.system_database_url, triple).await?;
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

    let mut system = connect(&platform.system_database_url).await?;
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
    let tenant = wamn_control_provision::project_env_tenant(org, project, env);

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
        match super::drain::stranded(&url, &tenant, env, &live).await {
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
                &platform.system_database_url,
                &prefix,
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
    secrets.extend(HOST_FAMILIES.iter().map(|(family, _)| {
        wamn_control_provision::workload_secret_name(*family, org, project, env)
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
            "the environment captures events; set WAMN_EVENT_NATS_URL, \
             WAMN_EVENT_NATS_USERNAME and WAMN_EVENT_NATS_PASSWORD_FILE to delete its streams",
        )?;
        let broker = jetstream::new(
            crate::event_streams::connection_options(
                &broker.nats_username,
                &broker.nats_password_file,
            )?
            .connect(&broker.nats_url)
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
    let mut admin_url =
        url::Url::parse(&platform.system_database_url).context("parse the system URL")?;
    admin_url.set_path("/postgres");
    let admin = connect(admin_url.as_str()).await?;
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
        let roles = instance_roles(triple, &instance, &tenant)?;
        for role in &roles {
            admin
                .batch_execute(&format!("DROP ROLE IF EXISTS {}", quote_ident(role)))
                .await
                .with_context(|| format!("drop role {role}"))?;
        }
        lines.push(format!("dropped {} roles (or they were gone)", roles.len()));
    }

    // The control rows of the tenant, then the environment row, last.
    let transaction = system.transaction().await.context("begin the row delete")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    for (table, trigger) in CONTROL_TABLES {
        for statement in delete_control_rows_sql(table, trigger, &tenant) {
            transaction
                .execute(statement.as_str(), &[])
                .await
                .with_context(|| format!("delete the control rows of {table}"))?;
        }
    }
    transaction
        .execute(
            "DELETE FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the registry.project_envs row")?;
    let tenant_table: bool = transaction
        .query_one(
            "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.tenant_environments")?
        .get(0);
    if tenant_table {
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
    lines.push(format!(
        "deleted the control rows of tenant {tenant} and the environment row of {triple}"
    ));
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

fn stream_not_found(kind: GetStreamErrorKind) -> bool {
    matches!(kind, GetStreamErrorKind::JetStream(error) if error.error_code() == ErrorCode::STREAM_NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_control_rows_go_leaves_first_with_the_trigger_off_for_one_delete() {
        let order = CONTROL_TABLES.map(|(table, _)| table);
        let position = |table: &str| order.iter().position(|t| *t == table).unwrap();
        // Each child table before the table its foreign key names.
        assert!(
            position("catalog.effective_release_packages") < position("catalog.effective_releases")
        );
        assert!(position("catalog.package_migrations") < position("catalog.packages"));
        assert!(position("catalog.component_library") < position("catalog.packages"));
        assert_eq!(
            delete_control_rows_sql("catalog.packages", Some("packages_immutable"), "dev"),
            [
                "ALTER TABLE catalog.packages DISABLE TRIGGER \"packages_immutable\"",
                "DELETE FROM catalog.packages WHERE tenant_id = 'dev'",
                "ALTER TABLE catalog.packages ENABLE TRIGGER \"packages_immutable\"",
            ]
        );
        assert_eq!(
            delete_control_rows_sql("catalog.packages", None, "dev"),
            ["DELETE FROM catalog.packages WHERE tenant_id = 'dev'"]
        );
    }
}
