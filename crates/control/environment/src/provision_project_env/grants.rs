//! Observed database privileges and exact workload role grant tests.

pub(super) mod plan;
pub(super) use plan::stable_grant_set;

use anyhow::Context as _;
use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_READ_COLUMNS, AUDIT_RETENTION_TARGETS_SQL, WRITE_LOG_RETENTION_READ_COLUMNS,
    WRITE_LOG_RETENTION_TARGET_SQL,
};

use super::{
    BTreeMap, BTreeSet, GenericClient, PgConfig, SystemReader, WorkloadRoleFamily, connect_config,
    sql,
};

pub(super) async fn verify_public_access_floor(
    client: &(impl GenericClient + Sync),
    label: &str,
) -> anyhow::Result<()> {
    let databases: Vec<String> = client
        .query(sql::public_connect_databases_sql(), &[])
        .await
        .context("verify cluster PUBLIC CONNECT floor")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    anyhow::ensure!(
        databases.is_empty(),
        "{label} generation actions require PUBLIC CONNECT revoked on every connectable database (template1 included); still granted on {databases:?}"
    );
    let public_temporary: bool = client
        .query_one(sql::public_temporary_on_current_database_sql(), &[])
        .await
        .context("verify target database PUBLIC TEMPORARY floor")?
        .get(0);
    anyhow::ensure!(
        !public_temporary,
        "{label} generation actions require PUBLIC TEMPORARY revoked on the exact database"
    );
    Ok(())
}

pub(super) use plan::StableGrantSet;

pub(super) use plan::RoleAclExpectation;

pub(super) async fn verify_role_grants(
    admin_config: &PgConfig,
    role: &str,
    expectation: RoleAclExpectation<'_>,
) -> anyhow::Result<()> {
    let (catalog, catalog_task) = connect_config(admin_config, "role grants").await?;
    // Only the databases this login can connect to. The provisioning worker
    // records the others in its step detail.
    let databases: Vec<String> = catalog
        .query(sql::non_template_databases_sql(), &[])
        .await
        .context("list databases for role grants")?
        .into_iter()
        .filter(|row| row.get::<_, bool>(1))
        .map(|row| row.get(0))
        .collect();
    drop(catalog);
    catalog_task
        .await
        .context("join ACL catalog connection")??;

    for database in databases {
        let mut config = admin_config.clone();
        config.dbname(&database);
        let (client, task) = connect_config(&config, "cross-database role grants").await?;
        let rows = client
            .query(sql::role_database_grants_sql(), &[&role])
            .await
            .with_context(|| format!("read role grants in database {database:?}"))?;
        let grants: Vec<RoleAcl> = rows
            .into_iter()
            .map(|row| RoleAcl {
                object_type: row.get("object_type"),
                schema_name: row.get("schema_name"),
                object_name: row.get("object_name"),
                privilege: row.get("privilege_type"),
                grantable: row.get("is_grantable"),
            })
            .collect();
        for acl in &grants {
            anyhow::ensure!(
                !acl.grantable,
                "role {role:?} may grant {} on {} {}.{} in database {database:?}",
                acl.privilege,
                acl.object_type,
                acl.schema_name,
                acl.object_name,
            );
        }
        match expectation {
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database,
            } => {
                // The audit retention grants follow the retention source that
                // the retention verb reads, in this same database.
                let dynamic_targets = if grant_set == StableGrantSet::AuditRetention {
                    client
                        .query(
                            &format!("SELECT schema_name, history_name FROM ({AUDIT_RETENTION_TARGETS_SQL}) AS history UNION ALL {WRITE_LOG_RETENTION_TARGET_SQL}"),
                            &[],
                        )
                        .await
                        .with_context(|| {
                            format!("read retention targets in database {database:?}")
                        })?
                        .into_iter()
                        .map(|row| (row.get("schema_name"), row.get("history_name")))
                        .collect()
                } else if grant_set == StableGrantSet::ExecutorPlatform {
                    executor_history_targets(&client).await?
                } else {
                    Vec::new()
                };
                grant_set.verify(
                    role,
                    &database,
                    required_database,
                    &grants,
                    &dynamic_targets,
                )?;
            }
            expectation => {
                for acl in &grants {
                    let allowed = match expectation {
                        RoleAclExpectation::None => false,
                        RoleAclExpectation::Generation { database: expected } => {
                            acl.object_type == "database"
                                && database == expected
                                && acl.object_name == expected
                                && acl.privilege == "CONNECT"
                        }
                        RoleAclExpectation::StableGrantSet { .. } => {
                            unreachable!("handled above")
                        }
                    };
                    anyhow::ensure!(
                        allowed,
                        "role {role:?} carries unexpected direct {} on {} {}.{} in database {database:?}",
                        acl.privilege,
                        acl.object_type,
                        acl.schema_name,
                        acl.object_name,
                    );
                }
            }
        }
        drop(client);
        task.await.context("join cross-database ACL connection")??;
    }
    Ok(())
}

pub(super) use plan::RoleAcl;

/// The installed entity maps and their model history tables, independent of ACLs.
async fn executor_history_targets(
    client: &(impl GenericClient + Sync),
) -> anyhow::Result<Vec<(String, String)>> {
    let schemas = client.query(
        "SELECT n.nspname::text FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid WHERE c.relname = 'wamn_entities' AND c.relkind = 'r'", &[],
    ).await.context("read entity map schemas for executor grants")?;
    let mut targets = Vec::new();
    for row in schemas {
        let schema: String = row.get(0);
        targets.push((schema.clone(), "wamn_entities".into()));
        let statement = format!(
            "SELECT DISTINCT c.relname::text FROM {}.wamn_entities m JOIN pg_class c ON c.relnamespace = $1::text::regnamespace AND c.relname = m.table_name || '_history'",
            postgres_protocol::escape::escape_identifier(&schema),
        );
        for row in client
            .query(&statement, &[&schema])
            .await
            .context("read model histories for executor grants")?
        {
            targets.push((schema.clone(), row.get(0)));
        }
    }
    Ok(targets)
}
