//! Complete CNPG declarations derived from registry coordinates and platform code.
use std::collections::BTreeMap;

use anyhow::Context as _;
use serde_json::{Value, json};
use sqlx::Connection as _;
use wamn_control_provision::connection::{ManagedRole, managed_replication_role, managed_role};
use wamn_control_provision::migrations::MigrationDatabase;
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, workload_login,
};
use wamn_registry::Triple;

pub type Declarations = BTreeMap<String, Vec<ManagedRole>>;

/// Fixed bootstrap credentials precede every org and project row.
pub fn platform(namespace: &str, cluster: &str, database: &str) -> anyhow::Result<Declarations> {
    Ok(BTreeMap::from([(
        cluster.to_owned(),
        vec![
            managed_role("wamn_apply".into(), "wamn_db_owner", "wamn-apply".into()),
            MigrationDatabase::System.managed_role(database),
            managed_role(
                wamn_control_provision::identity_role::identity_issuer_login(
                    &crate::platform::identity::issuer(namespace),
                )?,
                wamn_control_provision::identity_role::IDENTITY_ISSUER_ROLE,
                "wamn-identity-db".into(),
            ),
        ],
    )]))
}

/// Rebuild the whole org declaration; installed Helm role values are never read.
pub async fn registered(registry_input: &str, namespace: &str) -> anyhow::Result<Declarations> {
    let options = wamn_postgres::connect_options(registry_input).await?;
    let cluster = "wamn-pg";
    let database = options.get_database().context("registry database")?;
    let mut declarations = platform(namespace, cluster, database)?;
    let mut connection = wamn_postgres::connect(registry_input).await?;
    let mut snapshot = connection.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *snapshot)
        .await?;
    let orgs: Vec<String> = sqlx::query_scalar(wamn_registry::sql::INSTALL_ORGS)
        .bind(namespace)
        .fetch_all(&mut *snapshot)
        .await?;
    for org in orgs {
        let role = managed_role(
            workload_login(
                WorkloadRoleFamily::Control,
                WorkloadRoleIdentity::Org {
                    org: &org,
                    database,
                },
            )?,
            WorkloadRoleFamily::Control.acl_role(),
            wamn_control_provision::org_workload_secret_name(WorkloadRoleFamily::Control, &org),
        );
        declarations.entry(cluster.into()).or_default().push(role);
    }
    type ProjectFact = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    );
    let projects: Vec<ProjectFact> = sqlx::query_as(wamn_registry::sql::INSTALL_PROJECTS)
        .bind(namespace)
        .fetch_all(&mut *snapshot)
        .await?;
    for (org, project_name, environment, instance, placement, pool, policy_name) in projects {
        let policy = sqlx::query(sqlx::AssertSqlSafe(
            wamn_registry::sql::select_env_policy_sql(),
        ))
        .bind(&org)
        .bind(&policy_name)
        .fetch_one(&mut *snapshot)
        .await?;
        let policy = wamn_release_control::env_policies::env_policy_from_row(&policy)?;
        let placement = match placement.as_str() {
            "pooled" => wamn_control_provision::Placement::Pooled {
                pool: pool.context("registered org pool")?,
            },
            "dedicated" => wamn_control_provision::Placement::Dedicated,
            _ => anyhow::bail!("invalid registered org placement"),
        };
        let org = wamn_control_provision::Org { id: org, placement };
        let project_cluster = wamn_control_provision::cluster_of(&org, &policy).name;
        let triple = Triple::new(org.id, environment.as_str(), project_name);
        project(
            &mut declarations,
            &triple,
            &instance,
            &project_cluster,
            cluster,
            database,
        )?;
    }
    snapshot.commit().await?;
    Ok(declarations)
}

fn project(
    declarations: &mut Declarations,
    triple: &Triple,
    instance: &str,
    cluster: &str,
    system_cluster: &str,
    system_database: &str,
) -> anyhow::Result<()> {
    let database = wamn_control_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        instance,
    );
    let key = wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
    for family in WorkloadRoleFamily::project_env_families() {
        let (owner, database) = if family.identity_type() == WorkloadRoleIdentityType::Control {
            (system_cluster, system_database)
        } else {
            (cluster, database.as_str())
        };
        let identity = match family.identity_type() {
            WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: &key,
                database,
            },
            WorkloadRoleIdentityType::ProjectEnvironment => {
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: &triple.org,
                    project: &triple.project,
                    environment: &triple.env,
                    database,
                }
            }
            WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
                org: &triple.org,
                project: &triple.project,
                environment: &triple.env,
                database,
            },
            WorkloadRoleIdentityType::Org => {
                unreachable!("org family is not provisioned per project")
            }
        };
        let mut role = managed_role(
            workload_login(family, identity)?,
            family.acl_role(),
            wamn_control_provision::workload_secret_name(
                family,
                &triple.org,
                &triple.project,
                &triple.env,
            ),
        );
        if family.is_platform_grain() {
            role.in_roles
                .push(wamn_control_provision::workload_role::PLATFORM_GROUP_ROLE.into());
        }
        declarations.entry(owner.into()).or_default().push(role);
    }
    let roles = declarations.entry(cluster.into()).or_default();
    roles.push(MigrationDatabase::Project.managed_role(&database));
    roles.push(managed_replication_role(
        wamn_control_provision::cdc_object_name(
            &triple.org,
            &triple.project,
            &triple.env,
            instance,
        ),
        wamn_control_provision::project_env_cdc_secret_name(
            &triple.org,
            &triple.project,
            &triple.env,
        ),
    ));
    Ok(())
}

/// Declare inherited NOLOGIN roles before logins, including during an empty bootstrap.
pub fn values(declarations: &Declarations) -> anyhow::Result<Value> {
    let mut result = serde_json::Map::new();
    for (cluster, logins) in declarations {
        let mut roles = BTreeMap::new();
        for login in logins {
            for group in &login.in_roles {
                roles.entry(group.as_str()).or_insert_with(|| json!({"name":group,"ensure":"present","login":false,
                    "superuser":false,"createdb":false,"createrole":false,"inherit":false,"replication":false,"bypassrls":false}));
            }
        }
        let mut roles = roles.into_values().collect::<Vec<_>>();
        for login in logins {
            roles.push(serde_json::to_value(login)?);
        }
        result.insert(cluster.to_owned(), roles.into());
    }
    Ok(result.into())
}
