//! Observed database privileges and exact workload role grant checks.

use anyhow::Context as _;

use super::{
    BTreeMap, BTreeSet, GenericClient, PgConfig, SystemReader, WorkloadRoleFamily, connect_config, sql,
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

/// THE GRANT SET, and the one thing that stays per family
/// (`wamn-0h0g.22.16`).
///
/// `None` = this family's stable ACL role holds no direct grants of its own, so
/// there is no denial matrix to assert. The wildcard arm is deliberate: an
/// admitted family reaches every derived flag, action and Secret without an
/// edit anywhere, and acquires an entry HERE only when it acquires authority.
pub(super) fn stable_grant_set(family: WorkloadRoleFamily) -> Option<StableGrantSet> {
    match family {
        WorkloadRoleFamily::EffectWriter => Some(StableGrantSet::EffectWriter),
        WorkloadRoleFamily::ManagementAdmitter => Some(StableGrantSet::ManagementAdmitter),
        WorkloadRoleFamily::RegistryReader => Some(StableGrantSet::RegistryReader),
        WorkloadRoleFamily::IdentityReader => Some(StableGrantSet::IdentityReader),
        WorkloadRoleFamily::SessionRoleReader => Some(StableGrantSet::SessionRoleReader),
        WorkloadRoleFamily::Retention => Some(StableGrantSet::Retention),
        WorkloadRoleFamily::DispatchReader => Some(StableGrantSet::DispatchReader),
        // `wamn-0h0g.22.37`: both families acquired authority, so both acquire
        // a denial matrix in the SAME edit. A family with one and not the other
        // is exactly the bug
        // `every_family_derives_a_lifecycle_and_only_a_grant_set_stays_per_family`
        // exists to catch.
        WorkloadRoleFamily::ExecutorPlatform => Some(StableGrantSet::ExecutorPlatform),
        WorkloadRoleFamily::HttpAdmitter => Some(StableGrantSet::HttpAdmitter),
        WorkloadRoleFamily::EventMaterializer => Some(StableGrantSet::EventMaterializer),
        _ => None,
    }
}

/// The per-family denial matrices a stable ACL role is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StableGrantSet {
    EffectWriter,
    ManagementAdmitter,
    RegistryReader,
    IdentityReader,
    SessionRoleReader,
    Retention,
    DispatchReader,
    ExecutorPlatform,
    HttpAdmitter,
    EventMaterializer,
}

impl StableGrantSet {
    fn verify(
        self,
        role: &str,
        database: &str,
        required_database: &str,
        grants: &[RoleAcl],
    ) -> anyhow::Result<()> {
        match self {
            Self::EffectWriter => {
                verify_effect_writer_grants(role, database, grants)
            }
            Self::ManagementAdmitter => verify_management_admitter_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::RegistryReader => verify_system_reader_grants(
                SystemReader::Registry,
                "registry",
                &sql::REGISTRY_READER_RELATIONS,
                role,
                database,
                required_database,
                grants,
            ),
            Self::IdentityReader => verify_system_reader_grants(
                SystemReader::Identity,
                "identity",
                &sql::IDENTITY_READER_RELATIONS,
                role,
                database,
                required_database,
                grants,
            ),
            Self::SessionRoleReader => verify_session_role_reader_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::Retention => verify_retention_grants(role, database, grants),
            Self::DispatchReader => {
                verify_dispatch_reader_grants(role, database, grants)
            }
            Self::ExecutorPlatform => verify_executor_platform_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::HttpAdmitter => verify_http_admitter_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::EventMaterializer => verify_event_materializer_grants(
                role,
                database,
                required_database,
                grants,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleAclExpectation<'a> {
    None,
    Generation {
        database: &'a str,
    },
    StableGrantSet {
        grant_set: StableGrantSet,
        required_database: &'a str,
    },
}

pub(super) async fn verify_role_grants(
    admin_config: &PgConfig,
    role: &str,
    expectation: RoleAclExpectation<'_>,
) -> anyhow::Result<()> {
    let (catalog, catalog_task) = connect_config(admin_config, "role grants").await?;
    let databases: Vec<String> = catalog
        .query(sql::non_template_databases_sql(), &[])
        .await
        .context("list databases for role grants")?
        .into_iter()
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
                object_kind: row.get("object_kind"),
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
                acl.object_kind,
                acl.schema_name,
                acl.object_name,
            );
        }
        match expectation {
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database,
            } => {
                grant_set.verify(role, &database, required_database, &grants)?;
            }
            expectation => {
                for acl in &grants {
                    let allowed = match expectation {
                        RoleAclExpectation::None => false,
                        RoleAclExpectation::Generation { database: expected } => {
                            acl.object_kind == "database"
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
                        acl.object_kind,
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

#[derive(Clone)]
pub(super) struct RoleAcl {
    pub(super) object_kind: String,
    pub(super) schema_name: String,
    pub(super) object_name: String,
    pub(super) privilege: String,
    pub(super) grantable: bool,
}

pub(super) fn verify_effect_writer_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-writer {} ACL in database {database:?}",
            acl.object_kind
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_kind.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            ));
    }
    for (schema, actual) in by_schema {
        anyhow::ensure!(
            !schema.starts_with("pg_")
                && !matches!(
                    schema.as_str(),
                    "public" | "information_schema" | "wamn_system" | "catalog" | "app"
                ),
            "stable role {role:?} carries effect-writer ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        for table in [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ] {
            expected.insert((
                "relation".to_string(),
                table.to_string(),
                "SELECT".to_string(),
            ));
            expected.insert((
                "relation".to_string(),
                table.to_string(),
                "INSERT".to_string(),
            ));
        }
        for (table, columns) in [
            ("runs", &["tenant_id", "run_id", "status"][..]),
            (
                "run_queue",
                &[
                    "tenant_id",
                    "run_id",
                    "lease_owner",
                    "lease_expires_at",
                    "lease_generation",
                ][..],
            ),
        ] {
            for column in columns {
                expected.insert((
                    "column".to_string(),
                    format!("{table}.{column}"),
                    "SELECT".to_string(),
                ));
            }
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact effect-writer grant set"
        );
    }
    Ok(())
}

/// The exact run-retention grant set, measured from the SERVER's ACL catalogs
/// (`wamn-0h0g.12.69`).
///
/// Deliberately the effect writer's shape — iterate whatever schemas the role
/// holds anything in and require each to be EXACTLY this set — because retention
/// is likewise a tenant-scoped family whose grants land inside each project-env
/// database's run-plane schema, and a widened grant in a schema nobody thought
/// to name is exactly the drift a per-schema allow-list would miss.
///
/// The `SELECT` is COLUMN-scoped and the assertion has to keep it that way. The
/// role is a `wamn_platform` member, that group's floor arm on `wamn_run.runs`
/// is `USING (true)`, and PostgreSQL grants are relation- and column-shaped
/// rather than row-shaped — so this column list is the only thing standing
/// between a retention credential and every tenant's run payloads. A
/// `("relation", "runs", "SELECT")` entry appearing here is that regression, and
/// it fails as an unexpected member of the exact set.
fn verify_retention_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-retention {} ACL in database {database:?}",
            acl.object_kind
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_kind.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            ));
    }
    for (schema, actual) in by_schema {
        anyhow::ensure!(
            !schema.starts_with("pg_")
                && !matches!(
                    schema.as_str(),
                    "public" | "information_schema" | "wamn_system" | "catalog" | "app"
                ),
            "stable role {role:?} carries retention ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        expected.insert((
            "relation".to_string(),
            "runs".to_string(),
            "DELETE".to_string(),
        ));
        for column in RETENTION_RUN_READ_COLUMNS {
            expected.insert((
                "column".to_string(),
                format!("runs.{column}"),
                "SELECT".to_string(),
            ));
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact run-retention grant set"
        );
    }
    Ok(())
}

/// The exact dispatcher read surface, measured from the SERVER's ACL catalogs
/// (`wamn-0h0g.22.24`).
///
/// The dispatcher's whole database surface is two `SELECT`s over
/// [`sql::DISPATCH_READER_RELATIONS`], so the stable ACL role holds schema
/// `USAGE` plus `SELECT` on exactly those two relations. It is asserted PER
/// SCHEMA and exactly, the effect writer's shape, because a dispatch-reader
/// generation now inherits everything this role holds in every database the
/// role has grants in — and until this bead the family had no denial matrix at
/// all, because it had no generations to guard.
fn verify_dispatch_reader_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-reader {} ACL in database {database:?}",
            acl.object_kind
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_kind.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            ));
    }
    for (schema, actual) in by_schema {
        anyhow::ensure!(
            !schema.starts_with("pg_")
                && !matches!(
                    schema.as_str(),
                    "public" | "information_schema" | "wamn_system" | "catalog" | "app"
                ),
            "stable role {role:?} carries dispatch-reader ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        for relation in sql::DISPATCH_READER_RELATIONS {
            expected.insert((
                "relation".to_string(),
                relation.to_string(),
                "SELECT".to_string(),
            ));
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact dispatch-reader grant set"
        );
    }
    Ok(())
}

/// The only `runs` columns run-history pruning reads: the three its `WHERE`
/// clause names. `run_id` is deliberately absent — the statement never selects
/// it, and the verb reports a COUNT rather than a list.
const RETENTION_RUN_READ_COLUMNS: [&str; 3] = ["tenant_id", "status", "created_at"];

pub(super) fn verify_management_admitter_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no management-admission ACL in required database {database:?}"
        );
        return Ok(());
    }

    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "routine".to_string(),
            "wamn_authority".to_string(),
            "tenant_key".to_string(),
            "EXECUTE".to_string(),
        ),
    ]);
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for column in sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS {
        expected.insert((
            "column".to_string(),
            "catalog".to_string(),
            format!("wirings.{column}"),
            "INSERT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact management-admission grant set"
    );
    Ok(())
}

/// The `aclexplode` grants as `(kind, schema, object, privilege)` tuples.
fn acl_tuples(grants: &[RoleAcl]) -> BTreeSet<(String, String, String, String)> {
    grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect()
}

/// Check the reader's two column-scoped reads and no other direct grants.
pub(super) fn verify_session_role_reader_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no session-role reader ACL in required database {database:?}"
        );
        return Ok(());
    }
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        "app_system".to_string(),
        "app_system".to_string(),
        "USAGE".to_string(),
    )]);
    for (relation, columns) in [
        ("users", ["tenant_id", "id", "status"]),
        ("user_roles", ["tenant_id", "user_id", "role_name"]),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "app_system".to_string(),
                format!("{relation}.{column}"),
                "SELECT".to_string(),
            ));
        }
    }
    anyhow::ensure!(
        grants.iter().all(|acl| !acl.grantable) && acl_tuples(grants) == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact session-role reader grant set"
    );
    Ok(())
}

/// THE EXECUTOR-PLATFORM DENIAL MATRIX (`wamn-0h0g.22.37`).
///
/// EQUALITY against the server's own `aclexplode` answer, never containment: a
/// containment check passes a role that has ALSO acquired `INSERT` on `runs`,
/// and this family's credentials match the permissive `TO wamn_platform` floor
/// arm, so any privilege it holds it holds over EVERY tenant's rows.
///
/// The `routine` rows are part of the matrix, not an exemption. The surface
/// grants two function EXECUTEs — its own authority guard and the tenant-key
/// derivation the `runs_tkey` expression index makes load bearing — and
/// `sql::role_database_grants_sql` reports routine ACLs, so omitting
/// them here would refuse a correctly converged role. (The management-admitter
/// matrix above carries its own `routine` row since `wamn-0h0g.22.38`; the
/// omission this note used to record as a defect is fixed, and a routine row
/// is the convention for both families rather than an exemption for one.)
///
/// An empty grant set is acceptable only OUTSIDE the target database: the
/// project-environment database must carry the grant set, and this role must
/// hold nothing anywhere else on the cluster.
fn verify_executor_platform_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no executor-platform ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "schema".to_string(),
            "wamn_run".to_string(),
            "wamn_run".to_string(),
            "USAGE".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "runs".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "run_queue".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "run_queue".to_string(),
            "DELETE".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "effect_attempts".to_string(),
            "SELECT".to_string(),
        ),
        (
            "routine".to_string(),
            "wamn_authority".to_string(),
            "tenant_key".to_string(),
            "EXECUTE".to_string(),
        ),
    ]);
    for relation in sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for (relation, columns) in [
        ("runs", &sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS[..]),
        (
            "run_queue",
            &sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS[..],
        ),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "wamn_run".to_string(),
                format!("{relation}.{column}"),
                "UPDATE".to_string(),
            ));
        }
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact executor-platform grant set"
    );
    Ok(())
}

/// THE CALLABLE-HTTP ADMITTER DENIAL MATRIX (`wamn-0h0g.22.37`).
///
/// `USAGE` on `catalog` and `app_system`, the exact catalog and fresh permission
/// reads, and NOTHING on the run plane — the
/// disjointness from the executor family is the security property, and it is
/// asserted by equality for the same reason the two T1 readers' is. A `wamn_run`
/// schema `USAGE` alone would fail here, which is what stops this credential
/// being quietly reused for admission work.
pub(super) fn verify_http_admitter_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no callable-HTTP admitter ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "app_system".to_string(),
            "app_system".to_string(),
            "USAGE".to_string(),
        ),
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "permissions".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "users".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "user_roles".to_string(),
            "SELECT".to_string(),
        ),
    ]);
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact callable-HTTP admission grant set"
    );
    Ok(())
}

/// Exact two-table catalog read surface of the event materializer.
pub(super) fn verify_event_materializer_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no event-materializer ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        "catalog".to_string(),
        "catalog".to_string(),
        "USAGE".to_string(),
    )]);
    for relation in sql::EVENT_MATERIALIZER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact event-materializer grant set"
    );
    Ok(())
}

/// THE DISJOINTNESS MATRIX for one T1 control-database reader
/// (`wamn-0h0g.12.116`, `wamn-0h0g.12.67`).
///
/// The server's own `aclexplode` answer, compared for EQUALITY against the
/// derived set — never containment. Containment would pass a role that had
/// acquired the OTHER reader's schema, and that union is the exact failure the
/// two families exist to prevent; an added `INSERT` or `UPDATE` fails here for
/// the same reason.
///
/// An empty grant set is only acceptable in a database that is not the target:
/// the control database MUST carry the grant set, and every other database in
/// the cluster must carry nothing at all.
pub(super) fn verify_system_reader_grants(
    reader: SystemReader,
    schema: &str,
    relations: &[&str],
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if database != required_database {
        anyhow::ensure!(
            grants.is_empty(),
            "stable role {role:?} carries a {reader} ACL in database {database:?}, \
             which is not the control database"
        );
        return Ok(());
    }
    anyhow::ensure!(
        !grants.is_empty(),
        "stable role {role:?} has no {reader} ACL in required database {database:?}"
    );

    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        schema.to_string(),
        schema.to_string(),
        "USAGE".to_string(),
    )]);
    for relation in relations {
        expected.insert((
            "relation".to_string(),
            schema.to_string(),
            (*relation).to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact {reader} grant set"
    );
    Ok(())
}
