//! The `delete-project-env` verb (`wamn-psss`, docs/plan/environment-teardown.md).
//!
//! One run deletes one project environment and everything its instance names:
//! the event streams, the replication slot, the database, the roles that can
//! connect to it, the control rows of its tenant, and last the registry rows.
//! A trigger then records the old suffix in `registry.retired_project_envs`, and
//! the next `provision-project-env` of the triple mints a new one.
//!
//! Every name comes from the registry row, the tenant row and the naming
//! functions, never from a Kubernetes object or a database ACL, so a second run
//! after a failed first run finds the same names. Every step answers a missing
//! object as done. The verb has no Kubernetes client: it prints the objects the
//! operator deletes before the run, and the refusal on database sessions is the
//! check that the workloads are stopped.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use async_nats::jetstream::context::GetStreamErrorKind;
use async_nats::jetstream::{self, ErrorCode};
use tokio_postgres::{Client, NoTls};

use wamn_control_provision::sql::{drop_database_named_sql, drop_replication_slot_sql};
use wamn_control_provision::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind, workload_generation_role,
};
use wamn_control_provision::{
    CredentialGeneration, cdc_object_name, event_stream_name, project_env_cdc_secret_name,
    project_env_database_name, project_env_secret_name, validate_instance_suffix,
    validate_project_env_cdc, workload_secret_name,
};
use wamn_control_registry::Triple;
use wamn_event_wire::delivery_advisory_stream;
use wamn_pg_core::{quote_ident, quote_literal};

/// The control tables that key rows by `tenant_id`, leaves first, each with
/// its immutability trigger. The foreign keys have no `ON DELETE` clause, so
/// this order is the one that deletes.
pub const CONTROL_TABLES: [(&str, Option<&str>); 10] = [
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
    ("catalog.effective_release_heads", None),
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

/// Inputs of one environment delete.
#[derive(Debug)]
pub struct DeleteProjectEnvRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Superuser URL of `wamn_system`. The verb sets `ROLE wamn_system`.
    pub system_database_url: String,

    /// Superuser URL of the `postgres` database of the target cluster.
    pub admin_database_url: String,

    /// Event broker, and the provisioning user of the stream.
    pub nats_url: String,
    pub nats_username: String,
    pub nats_password_file: PathBuf,

    /// Without it, the verb prints the plan and changes nothing.
    pub confirm: bool,
}

/// Every name one run deletes, derived from the triple, the instance and the
/// tenant.
#[derive(Debug, PartialEq, Eq)]
pub struct TeardownPlan {
    pub triple: Triple,
    pub instance: String,
    /// `None` when no `catalog.tenant_environments` row names the triple.
    pub tenant: Option<String>,
    pub database: String,
    /// The slot, the publication and the replication role share this name.
    pub cdc_object: String,
    pub source_stream: String,
    pub advisory_stream: String,
    /// The CDC role, then `a` and `b` of each family whose scope names the
    /// database.
    pub roles: Vec<String>,
    /// The Secrets of the instance, with the namespace where one is recorded.
    pub secrets: Vec<(String, Option<String>)>,
}

/// The families whose generation roles the run drops: their scope names the
/// database of the instance. A control or org family's scope names the
/// control database, so its roles stay with the triple or the org.
pub fn instance_families() -> impl Iterator<Item = WorkloadRoleFamily> {
    WorkloadRoleFamily::ALL.into_iter().filter(|family| {
        matches!(
            family.scope_kind(),
            WorkloadRoleScopeKind::Tenant | WorkloadRoleScopeKind::ProjectEnvironment
        )
    })
}

/// Derive the plan. `db_namespace` and `cdc_namespace` are the Secret
/// namespaces the registry rows record.
pub fn plan(
    triple: &Triple,
    instance: &str,
    tenant: Option<&str>,
    db_namespace: Option<&str>,
    cdc_namespace: Option<&str>,
) -> anyhow::Result<TeardownPlan> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let database = project_env_database_name(org, project, env, instance);
    let cdc_object = cdc_object_name(org, project, env, instance);
    let source_stream = event_stream_name(org, project, env);
    let advisory_stream = delivery_advisory_stream(&source_stream);

    let mut roles = vec![cdc_object.clone()];
    let mut secrets = vec![
        (
            project_env_secret_name(org, project, env),
            db_namespace.map(str::to_owned),
        ),
        (
            project_env_cdc_secret_name(org, project, env),
            cdc_namespace.map(str::to_owned),
        ),
    ];
    for family in instance_families() {
        let scope = match family.scope_kind() {
            WorkloadRoleScopeKind::Tenant => match tenant {
                Some(tenant) => WorkloadRoleScope::Tenant {
                    tenant,
                    database: &database,
                },
                None => continue,
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
        secrets.push((workload_secret_name(family, org, project, env), None));
    }
    Ok(TeardownPlan {
        triple: triple.clone(),
        instance: instance.to_owned(),
        tenant: tenant.map(str::to_owned),
        database,
        cdc_object,
        source_stream,
        advisory_stream,
        roles,
        secrets,
    })
}

impl TeardownPlan {
    /// The plan output. The Kubernetes objects and the workloads come first,
    /// because the operator deletes and stops them before the run.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "delete before this run: CloudNativePG Database {}",
            self.database
        )];
        for (name, namespace) in &self.secrets {
            let place = namespace.as_ref().map_or(String::new(), |namespace| {
                format!(" in namespace {namespace}")
            });
            lines.push(format!(
                "delete before this run: Secret {name}{place}; stop every workload that reads it"
            ));
        }
        lines.push(format!(
            "delete: streams {} and {}, with the consumers of {}",
            self.source_stream, self.advisory_stream, self.source_stream
        ));
        lines.push(format!(
            "drop: replication slot {} and database {}",
            self.cdc_object, self.database
        ));
        lines.push(format!("drop: roles {}", self.roles.join(", ")));
        match &self.tenant {
            Some(tenant) => lines.push(format!(
                "delete: control rows of tenant {tenant} in {}",
                CONTROL_TABLES.map(|(table, _)| table).join(", ")
            )),
            None => lines.push(format!(
                "no catalog.tenant_environments row names {}: no control rows and no tenant-scope roles",
                self.triple
            )),
        }
        lines.push(format!(
            "delete: registry.project_envs and catalog.tenant_environments rows of {}, instance {}",
            self.triple, self.instance
        ));
        lines
    }
}

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

/// Run the verb: print the plan, refuse on the checks of section 4.3, and with
/// `confirm` run the steps.
pub async fn delete_project_env(args: &DeleteProjectEnvRequest) -> anyhow::Result<()> {
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("environment names: {e}"))?;
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    let env = triple.env.as_str();
    let broker_options =
        crate::event_streams::connection_options(&args.nats_username, &args.nats_password_file)?;

    let mut system = connect(&args.system_database_url).await?;
    // Forced row-level security shows a catalog.tenant_environments row only to
    // a session that claims its tenant, and the tenant is what this read finds.
    // So the read runs as the superuser of the URL, before SET ROLE. Every
    // delete then runs as wamn_system with the tenant claimed.
    let tenant_row: Option<(String, String)> = system
        .query_opt(
            "SELECT tenant_id, instance_suffix FROM catalog.tenant_environments \
             WHERE org = $1 AND project = $2 AND env = $3",
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read the catalog.tenant_environments row")?
        .map(|row| (row.get(0), row.get(1)));
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let Some(row) = system
        .query_opt(
            &wamn_control_registry::sql::select_project_env_sql(),
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read the registry.project_envs row")?
    else {
        bail!("refused: no registry.project_envs row for {triple}");
    };
    let instance: String = row.get("instance_suffix");
    let db_namespace: Option<String> = row.get("secret_namespace");
    validate_instance_suffix(&instance)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    let tenant = match tenant_row {
        Some((tenant, suffix)) => {
            if suffix != instance {
                bail!(
                    "refused: catalog.tenant_environments names tenant {tenant} with suffix \
                     {suffix}, and registry.project_envs names {instance}"
                );
            }
            Some(tenant)
        }
        None => None,
    };
    // The cluster of the environment, by the one rule that provisioning places
    // it with: the pool of a pooled org, `<org>-<owner(env policy)>` of a
    // dedicated one (`wamn-3icz`).
    let cluster = crate::provision_project_env::resolve_cluster_on(&system, &triple.org, env)
        .await
        .context("derive the cluster of the environment")?;
    let cdc_namespace: Option<String> = system
        .query_opt(
            "SELECT replication_secret_namespace FROM registry.event_readers \
             WHERE org = $1 AND project = $2 AND env = $3",
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read the registry.event_readers row")?
        .and_then(|row| row.get(0));

    let plan = plan(
        &triple,
        &instance,
        tenant.as_deref(),
        db_namespace.as_deref(),
        cdc_namespace.as_deref(),
    )?;
    for line in plan.lines() {
        println!("{line}");
    }

    let admin = connect(&args.admin_database_url).await?;
    let current: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .context("read the database of --admin-database-url")?
        .get(0);
    if current != "postgres" {
        bail!("refused: --admin-database-url reaches database {current}, not postgres");
    }
    // CloudNativePG sets cluster_name to the name of its Cluster.
    let reached: String = admin
        .query_one("SHOW cluster_name", &[])
        .await
        .context("read cluster_name of --admin-database-url")?
        .get(0);
    if reached != cluster {
        bail!(
            "refused: --admin-database-url reaches cluster {reached:?}, and {triple} is placed on \
             {cluster}"
        );
    }
    let active = admin
        .query_opt(
            "SELECT active FROM pg_replication_slots WHERE slot_name = $1",
            &[&plan.cdc_object],
        )
        .await
        .context("read the replication slot")?
        .is_some_and(|row| row.get::<_, bool>(0));
    if active {
        bail!(
            "refused: slot {} is active: a reader still streams from it",
            plan.cdc_object
        );
    }
    let logins: Vec<String> = admin
        .query(
            "SELECT DISTINCT usename::text FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid() AND usename IS NOT NULL \
             ORDER BY 1",
            &[&plan.database],
        )
        .await
        .context("read the sessions of the database")?
        .iter()
        .map(|row| row.get(0))
        .collect();
    if !logins.is_empty() {
        bail!(
            "refused: database {} has sessions of {}: stop the workloads that use these logins",
            plan.database,
            logins.join(", ")
        );
    }

    if !args.confirm {
        println!("plan only: nothing changed; run again with --confirm");
        return Ok(());
    }

    let broker = jetstream::new(
        broker_options
            .connect(&args.nats_url)
            .await
            .context("connect the event provisioning credential")?,
    );
    delete_stream(&broker, &plan.source_stream).await?;
    delete_stream(&broker, &plan.advisory_stream).await?;

    admin
        .batch_execute(&drop_replication_slot_sql(&plan.cdc_object))
        .await
        .context("drop the replication slot")?;
    println!("dropped slot {} (or it was gone)", plan.cdc_object);
    admin
        .batch_execute(&drop_database_named_sql(&plan.database))
        .await
        .context("drop the database")?;
    println!("dropped database {} (or it was gone)", plan.database);
    for role in &plan.roles {
        admin
            .batch_execute(&format!("DROP ROLE IF EXISTS {}", quote_ident(role)))
            .await
            .with_context(|| format!("drop role {role}"))?;
    }
    println!("dropped {} roles (or they were gone)", plan.roles.len());

    if let Some(tenant) = &plan.tenant {
        let transaction = system
            .transaction()
            .await
            .context("begin the control rows transaction")?;
        claim_tenant(&transaction, tenant).await?;
        for (table, trigger) in CONTROL_TABLES {
            let statements = delete_control_rows_sql(table, trigger, tenant);
            let mut deleted = 0;
            for statement in &statements {
                deleted += transaction
                    .execute(statement.as_str(), &[])
                    .await
                    .with_context(|| format!("delete the control rows of {table}"))?;
            }
            println!("deleted {deleted} rows of {table}");
        }
        transaction
            .commit()
            .await
            .context("commit the control rows transaction")?;
    }

    let transaction = system
        .transaction()
        .await
        .context("begin the registry transaction")?;
    if let Some(tenant) = &plan.tenant {
        claim_tenant(&transaction, tenant).await?;
    }
    transaction
        .execute(
            "DELETE FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("delete the registry.project_envs row")?;
    let tenants = transaction
        .execute(
            "DELETE FROM catalog.tenant_environments \
             WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4",
            &[&triple.org, &triple.project, &env, &instance],
        )
        .await
        .context("delete the catalog.tenant_environments row")?;
    transaction
        .commit()
        .await
        .context("commit the registry transaction")?;
    println!(
        "deleted the registry row of {triple} and {tenants} tenant row: \
         registry.retired_project_envs records instance {instance}"
    );
    Ok(())
}

/// Delete one stream. A stream delete removes its consumers, and the
/// provisioning user may not list them. A missing stream is done.
async fn delete_stream(broker: &jetstream::Context, name: &str) -> anyhow::Result<()> {
    match broker.delete_stream(name).await {
        Ok(_) => println!("deleted stream {name}"),
        Err(error) if stream_not_found(error.kind()) => println!("stream {name} not found: done"),
        Err(error) => return Err(error).with_context(|| format!("delete stream {name}")),
    }
    Ok(())
}

/// Claim the tenant for this transaction, the platform's own claim pattern for
/// the tenant policies of the control store.
async fn claim_tenant(
    transaction: &tokio_postgres::Transaction<'_>,
    tenant: &str,
) -> anyhow::Result<()> {
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant in app.tenant")?;
    Ok(())
}

fn stream_not_found(kind: GetStreamErrorKind) -> bool {
    matches!(kind, GetStreamErrorKind::JetStream(error) if error.error_code() == ErrorCode::STREAM_NOT_FOUND)
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("database connect")?;
    tokio::spawn(connection);
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple() -> Triple {
        Triple::new("dkk", "receiving", "dev")
    }

    #[test]
    fn the_plan_names_the_instance_objects_and_lists_kubernetes_first() {
        let plan = plan(
            &triple(),
            "zf7o454t",
            Some("dev"),
            Some("hosts"),
            Some("platform"),
        )
        .unwrap();
        assert_eq!(plan.database, "wamn-db-dkk--receiving--dev--zf7o454t");
        assert_eq!(plan.cdc_object, "wamn_cdc_dkk__receiving__dev__zf7o454t");
        assert_eq!(plan.source_stream, "EVT_3_dkk_9_receiving_3_dev");
        assert_eq!(
            plan.advisory_stream,
            "WAMN_EVENT_ADVISORIES_EVT_3_dkk_9_receiving_3_dev"
        );
        // The CDC role, then a and b of the ten instance families.
        assert_eq!(instance_families().count(), 10);
        assert_eq!(plan.roles.len(), 1 + 10 * 2);
        assert_eq!(plan.roles[0], plan.cdc_object);
        assert!(plan.roles.iter().all(|role| role.len() <= 63));
        let secrets: Vec<&str> = plan.secrets.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            secrets,
            [
                "wamn-db-dkk--receiving--dev",
                "wamn-cdc-dkk--receiving--dev",
                "wamn-mgmt-admitter-dkk--receiving--dev",
                "wamn-service-reader-dkk--receiving--dev",
                "wamn-guest-dkk--receiving--dev",
                "wamn-retention-dkk--receiving--dev",
                "wamn-executor-platform-dkk--receiving--dev",
                "wamn-http-admitter-dkk--receiving--dev",
                "wamn-event-materializer-dkk--receiving--dev",
                "wamn-session-role-reader-dkk--receiving--dev",
                "wamn-audit-retention-dkk--receiving--dev",
                "wamn-administration-dkk--receiving--dev",
            ]
        );
        let lines = plan.lines();
        assert_eq!(
            lines[0],
            "delete before this run: CloudNativePG Database wamn-db-dkk--receiving--dev--zf7o454t"
        );
        assert_eq!(
            lines[1],
            "delete before this run: Secret wamn-db-dkk--receiving--dev in namespace hosts; \
             stop every workload that reads it"
        );
        assert_eq!(
            lines[3],
            "delete before this run: Secret wamn-mgmt-admitter-dkk--receiving--dev; \
             stop every workload that reads it"
        );
    }

    #[test]
    fn without_a_tenant_the_plan_keeps_the_tenant_scope_roles_out() {
        let plan = plan(&triple(), "zf7o454t", None, None, None).unwrap();
        let tenant_families = instance_families()
            .filter(|family| family.scope_kind() == WorkloadRoleScopeKind::Tenant)
            .count();
        assert_eq!(plan.roles.len(), 1 + (10 - tenant_families) * 2);
    }

    #[test]
    fn the_control_rows_go_leaves_first_with_the_trigger_off_for_one_delete() {
        let order = CONTROL_TABLES.map(|(table, _)| table);
        let position = |table: &str| order.iter().position(|t| *t == table).unwrap();
        // Each child table before the table its foreign key names.
        assert!(
            position("catalog.effective_release_heads") < position("catalog.effective_releases")
        );
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
            delete_control_rows_sql("catalog.effective_release_heads", None, "dev"),
            ["DELETE FROM catalog.effective_release_heads WHERE tenant_id = 'dev'"]
        );
    }
}
