use super::{
    AUDIT_RETENTION_READ_COLUMNS, BTreeMap, BTreeSet, SystemReader,
    WRITE_LOG_RETENTION_READ_COLUMNS, WorkloadRoleFamily, sql,
};

/// THE GRANT SET, and the one thing that stays per family
/// (`wamn-0h0g.22.16`).
///
/// `None` = this family's stable ACL role holds no direct grants of its own, so
/// there is no denial matrix to assert. The wildcard arm is deliberate: an
/// admitted family reaches every derived flag, action and Secret without an
/// edit anywhere, and acquires an entry HERE only when it acquires authority.
pub(in super::super) fn stable_grant_set(family: WorkloadRoleFamily) -> Option<StableGrantSet> {
    match family {
        WorkloadRoleFamily::ManagementAdmitter => Some(StableGrantSet::ManagementAdmitter),
        WorkloadRoleFamily::RegistryReader => Some(StableGrantSet::RegistryReader),
        WorkloadRoleFamily::IdentityReader => Some(StableGrantSet::IdentityReader),
        WorkloadRoleFamily::ConnectionReader => Some(StableGrantSet::ConnectionReader),
        WorkloadRoleFamily::SessionRoleReader => Some(StableGrantSet::SessionRoleReader),
        WorkloadRoleFamily::Retention => Some(StableGrantSet::Retention),
        // `wamn-0h0g.22.37`: both families acquired authority, so both acquire
        // a denial matrix in the SAME edit. A family with one and not the other
        // is exactly the bug
        // `every_family_derives_a_lifecycle_and_only_a_grant_set_stays_per_family`
        // exists to catch.
        WorkloadRoleFamily::ExecutorPlatform => Some(StableGrantSet::ExecutorPlatform),
        WorkloadRoleFamily::HttpAdmitter => Some(StableGrantSet::HttpAdmitter),
        WorkloadRoleFamily::EventMaterializer => Some(StableGrantSet::EventMaterializer),
        // `wamn-emtx.13`: apply-package converges these grants from the
        // retention source, and the family acquires its denial matrix row with
        // them.
        WorkloadRoleFamily::AuditRetention => Some(StableGrantSet::AuditRetention),
        WorkloadRoleFamily::Administration => Some(StableGrantSet::Administration),
        WorkloadRoleFamily::Control => Some(StableGrantSet::Control),
        _ => None,
    }
}

/// The exact run-retention grant set, measured from the SERVER's ACL catalogs
/// (`wamn-0h0g.12.69`).
///
/// Deliberately a per-schema shape — iterate whatever schemas the role holds
/// anything in and require each to be EXACTLY this set — because retention
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
pub(super) fn verify_retention_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_type.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-retention {} ACL in database {database:?}",
            acl.object_type
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_type.clone(),
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

/// The exact audit retention grant set, measured against `pg_trigger`
/// (`wamn-emtx.13`).
///
/// The role holds schema `USAGE`, `DELETE`, and `SELECT` on the four read
/// columns of each history table whose `wamn_record_history_log` argument is
/// `P<n>D`. With any target, it also holds the pruning watermark grants.
/// `retention_targets` lists those
/// history tables as `(schema, history table)`, from the same query that the
/// retention verb runs. So a grant on an `unlimited` history table, a wider
/// grant, and a missing grant all refuse. The optional `platform.write_log`
/// target grants only deletion and reads of its age and result.
pub(in super::super) fn verify_audit_retention_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
    retention_targets: &[(String, String)],
) -> anyhow::Result<()> {
    let mut expected = BTreeSet::new();
    if retention_targets
        .iter()
        .any(|(schema, history)| schema != "platform" || history != "write_log")
    {
        expected.insert((
            "schema".to_string(),
            "wamn_history".to_string(),
            "wamn_history".to_string(),
            "USAGE".to_string(),
        ));
        for privilege in ["SELECT", "INSERT", "UPDATE"] {
            expected.insert((
                "relation".to_string(),
                "wamn_history".to_string(),
                "pruning_watermarks".to_string(),
                privilege.to_string(),
            ));
        }
    }
    for (schema, history) in retention_targets {
        expected.insert((
            "schema".to_string(),
            schema.clone(),
            schema.clone(),
            "USAGE".to_string(),
        ));
        expected.insert((
            "relation".to_string(),
            schema.clone(),
            history.clone(),
            "DELETE".to_string(),
        ));
        let columns = if schema == "platform" && history == "write_log" {
            WRITE_LOG_RETENTION_READ_COLUMNS.as_slice()
        } else {
            AUDIT_RETENTION_READ_COLUMNS.as_slice()
        };
        for column in columns {
            expected.insert((
                "column".to_string(),
                schema.clone(),
                format!("{history}.{column}"),
                "SELECT".to_string(),
            ));
        }
    }
    let actual = acl_tuples(grants);
    for (kind, schema, object, privilege) in actual.iter().chain(&expected) {
        anyhow::ensure!(
            matches!(kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-retention {kind} ACL in database {database:?}"
        );
        anyhow::ensure!(
            (schema == "platform"
                && matches!(
                    (kind.as_str(), object.as_str(), privilege.as_str()),
                    ("schema", "platform", "USAGE")
                        | ("relation", "write_log", "DELETE")
                        | (
                            "column",
                            "write_log.created_at" | "write_log.result",
                            "SELECT"
                        )
                ))
                || !schema.starts_with("pg_")
                    && !matches!(
                        schema.as_str(),
                        "public"
                            | "information_schema"
                            | "wamn_system"
                            | "catalog"
                            | "app"
                            | "platform"
                    ),
            "stable role {role:?} carries audit-retention ACLs in reserved schema {schema:?} in database {database:?}"
        );
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact audit-retention grant set"
    );
    Ok(())
}

pub(in super::super) fn verify_management_admitter_grants(
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
                acl.object_type.clone(),
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
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact management-admission grant set"
    );
    Ok(())
}

/// The `aclexplode` grants as `(kind, schema, object, privilege)` tuples.
pub(super) fn acl_tuples(grants: &[RoleAcl]) -> BTreeSet<(String, String, String, String)> {
    grants
        .iter()
        .map(|acl| {
            (
                acl.object_type.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect()
}

/// Check the reader's two column-scoped reads and no other direct grants.
pub(in super::super) fn verify_session_role_reader_grants(
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
        "platform".to_string(),
        "platform".to_string(),
        "USAGE".to_string(),
    )]);
    for (relation, columns) in [
        ("user", &["id", "status"][..]),
        ("user_role", &["user_id", "role_name"][..]),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "platform".to_string(),
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
pub(super) fn verify_executor_platform_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
    history_targets: &[(String, String)],
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
        // An event admission reads the durability class (wamn-upl3.6).
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "environment_policies".to_string(),
            "SELECT".to_string(),
        ),
        (
            "routine".to_string(),
            "wamn_authority".to_string(),
            "tenant_key".to_string(),
            "EXECUTE".to_string(),
        ),
    ]);
    expected.insert((
        "schema".to_owned(),
        "platform".to_owned(),
        "platform".to_owned(),
        "USAGE".to_owned(),
    ));
    for column in ["id", "type", "status", "email"] {
        expected.insert((
            "column".to_owned(),
            "platform".to_owned(),
            format!("user.{column}"),
            "SELECT".to_owned(),
        ));
    }
    for relation in sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for relation in ["connection", "integration"] {
        expected.insert((
            "relation".to_string(),
            "platform".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for (relation, privilege, columns) in [
        (
            "runs",
            "UPDATE",
            &sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS[..],
        ),
        (
            "run_queue",
            "UPDATE",
            &sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS[..],
        ),
        // The event run grain the host admits (wamn-upl3.6).
        (
            "runs",
            "INSERT",
            &sql::EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS[..],
        ),
        (
            "run_queue",
            "INSERT",
            &sql::EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS[..],
        ),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "wamn_run".to_string(),
                format!("{relation}.{column}"),
                privilege.to_string(),
            ));
        }
    }
    for (schema, relation) in history_targets {
        expected.insert((
            "schema".into(),
            schema.clone(),
            schema.clone(),
            "USAGE".into(),
        ));
        if relation == "wamn_entities" {
            expected.insert((
                "relation".into(),
                schema.clone(),
                relation.clone(),
                "SELECT".into(),
            ));
        } else {
            for column in [
                "row_key",
                "type",
                "transaction_id",
                "position",
                "changed_by",
            ] {
                expected.insert((
                    "column".into(),
                    schema.clone(),
                    format!("{relation}.{column}"),
                    "SELECT".into(),
                ));
            }
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
/// `USAGE` on the schemas of `sql::HTTP_ADMITTER_SCHEMAS`, a `SELECT` on each
/// relation of `sql::http_admitter_relations`, and NOTHING on the run plane — the
/// disjointness from the executor family is the security property, and it is
/// asserted by equality for the same reason the two T1 readers' is. A `wamn_run`
/// schema `USAGE` alone would fail here, which is what stops this credential
/// being quietly reused for admission work.
pub(in super::super) fn verify_http_admitter_grants(
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
    // The expected set comes from the list that the grant SQL reads, so a
    // new admitter read changes both at once.
    let schemas = sql::HTTP_ADMITTER_SCHEMAS.iter().map(|schema| {
        (
            "schema".to_string(),
            schema.to_string(),
            schema.to_string(),
            "USAGE".to_string(),
        )
    });
    let relations = sql::http_admitter_relations().map(|(schema, relation)| {
        (
            "relation".to_string(),
            schema.to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        )
    });
    let columns = sql::HTTP_ADMITTER_WORKFLOW_COLUMNS.iter().map(|column| {
        (
            "column".to_string(),
            "catalog".to_string(),
            format!("workflows.{column}"),
            "SELECT".to_string(),
        )
    });
    let expected = schemas
        .chain(relations)
        .chain(columns)
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact callable-HTTP admission grant set"
    );
    Ok(())
}

/// Exact surface of the administration family (`wamn-a40n.2`): `USAGE` on
/// `catalog`, `platform` and `wamn_history`, `SELECT`, `INSERT`, `UPDATE`
/// and `DELETE` on each relation of `sql::ADMINISTRATION_RELATIONS`, `INSERT`
/// on the entry columns of its history table (without a tenant column), `SELECT` on `catalog.releases`
/// (`wamn-aemxz.1`), and `EXECUTE` on `row_image`. The grants go to `wamn_administration` alone.
pub(in super::super) fn verify_administration_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no administration ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let tuple = |object_type: &str, schema: &str, object: &str, privilege: &str| {
        (
            object_type.to_string(),
            schema.to_string(),
            object.to_string(),
            privilege.to_string(),
        )
    };
    let mut expected = BTreeSet::from([
        tuple("schema", "catalog", "catalog", "USAGE"),
        tuple("schema", "platform", "platform", "USAGE"),
        tuple("schema", "wamn_history", "wamn_history", "USAGE"),
        tuple("routine", "wamn_history", "row_image", "EXECUTE"),
        tuple("relation", "catalog", "connection_requirement", "SELECT"),
        tuple("relation", "catalog", "release_connection", "SELECT"),
        tuple("relation", "catalog", "connection_binding", "SELECT"),
        tuple("relation", "catalog", "releases", "SELECT"),
    ]);
    for relation in sql::ADMINISTRATION_RELATIONS {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            expected.insert(tuple("relation", "platform", relation, privilege));
        }
        if ["external_system", "integration", "connection"].contains(&relation) {
            continue;
        }
        for column in [
            "row_key",
            "type",
            "operation",
            "changed_by",
            "changed_at",
            "transaction_id",
            "before",
            "after",
        ] {
            expected.insert(tuple(
                "column",
                "platform",
                &format!("{relation}_history.{column}"),
                "INSERT",
            ));
        }
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact administration grant set"
    );
    Ok(())
}

/// Exact two-table catalog read surface of the event materializer.
pub(in super::super) fn verify_event_materializer_grants(
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

/// Require only the login-scoped credential lookup in the control database,
/// and the OAuth token rows that the host's reader login locks and writes.
pub(in super::super) fn verify_connection_reader_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if database != required_database {
        anyhow::ensure!(
            grants.is_empty(),
            "connection reader {role:?} has grants outside the control database"
        );
        return Ok(());
    }
    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_type.as_str(),
                acl.schema_name.as_str(),
                acl.object_name.as_str(),
                acl.privilege.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        ("schema", "registry", "registry", "USAGE"),
        ("schema", "identity", "identity", "USAGE"),
        ("relation", "identity", "oauth_token", "SELECT"),
        ("relation", "identity", "oauth_token", "INSERT"),
        ("relation", "identity", "oauth_token", "UPDATE"),
        ("relation", "identity", "oauth_token", "DELETE"),
        ("relation", "identity", "oauth_authorization", "SELECT"),
        ("relation", "identity", "oauth_authorization", "INSERT"),
        ("relation", "identity", "oauth_authorization", "UPDATE"),
        ("relation", "identity", "oauth_authorization", "DELETE"),
        ("routine", "registry", "lock_oauth_connection", "EXECUTE"),
        ("routine", "registry", "read_file_connection", "EXECUTE"),
        ("routine", "registry", "read_connection_fact", "EXECUTE"),
        (
            "routine",
            "registry",
            "read_connection_generation",
            "EXECUTE",
        ),
        ("routine", "registry", "read_connection_secret", "EXECUTE"),
        ("routine", "registry", "read_env_connections", "EXECUTE"),
        ("routine", "registry", "read_integration_version", "EXECUTE"),
    ]);
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} does not hold the exact credential lookup grants"
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
///
/// `columns` names the column reads in `schema` beside the whole relations.
#[expect(
    clippy::too_many_arguments,
    reason = "the reader's surface and the database it is measured in are separate facts"
)]
pub(in super::super) fn verify_system_reader_grants(
    reader: impl std::fmt::Display,
    schema: &str,
    relations: &[&str],
    columns: &[(&str, &[&str])],
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
                acl.object_type.clone(),
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
        let (schema, relation) = relation.split_once('.').unwrap_or((schema, relation));
        expected.insert((
            "schema".to_string(),
            schema.to_string(),
            schema.to_string(),
            "USAGE".to_string(),
        ));
        expected.insert((
            "relation".to_string(),
            schema.to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for (relation, names) in columns {
        for column in *names {
            expected.insert((
                "column".to_string(),
                schema.to_string(),
                format!("{relation}.{column}"),
                "SELECT".to_string(),
            ));
        }
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact {reader} grant set"
    );
    Ok(())
}

/// The exact control grant set, [`sql::CONTROL_SURFACE`] and
/// [`sql::CONTROL_COLUMN_SURFACE`], held in the control database alone.
pub(in super::super) fn verify_control_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if database != required_database {
        anyhow::ensure!(
            grants.is_empty(),
            "stable role {role:?} carries a control ACL in database {database:?}, \
             which is not the control database"
        );
        return Ok(());
    }
    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_type.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut expected = sql::CONTROL_SCHEMAS
        .iter()
        .map(|schema| {
            (
                "schema".to_string(),
                (*schema).to_string(),
                (*schema).to_string(),
                "USAGE".to_string(),
            )
        })
        .collect::<BTreeSet<_>>();
    for (schema, relation, privileges) in sql::CONTROL_SURFACE {
        for privilege in privileges {
            expected.insert((
                "relation".to_string(),
                schema.to_string(),
                relation.to_string(),
                (*privilege).to_string(),
            ));
        }
    }
    for (schema, relation, privilege, columns) in sql::CONTROL_COLUMN_SURFACE {
        for column in columns {
            expected.insert((
                "column".to_string(),
                schema.to_string(),
                format!("{relation}.{column}"),
                privilege.to_string(),
            ));
        }
    }
    for signature in sql::CONTROL_CONNECTION_FUNCTIONS {
        expected.insert((
            "routine".to_owned(),
            "registry".to_owned(),
            signature
                .split('(')
                .next()
                .expect("function signature")
                .to_owned(),
            "EXECUTE".to_owned(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact control grant set"
    );
    Ok(())
}

/// The per-family denial matrices a stable ACL role is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum StableGrantSet {
    ManagementAdmitter,
    RegistryReader,
    IdentityReader,
    ConnectionReader,
    SessionRoleReader,
    Retention,
    ExecutorPlatform,
    HttpAdmitter,
    EventMaterializer,
    AuditRetention,
    Administration,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum RoleAclExpectation<'a> {
    None,
    Generation {
        database: &'a str,
    },
    StableGrantSet {
        grant_set: StableGrantSet,
        required_database: &'a str,
    },
}

#[derive(Clone)]
pub(in super::super) struct RoleAcl {
    pub(in super::super) object_type: String,
    pub(in super::super) schema_name: String,
    pub(in super::super) object_name: String,
    pub(in super::super) privilege: String,
    pub(in super::super) grantable: bool,
}

/// The only `runs` columns run-history pruning reads: the three its `WHERE`
/// clause names. `run_id` is deliberately absent — the statement never selects
/// it, and the verb reports a COUNT rather than a list.
pub(super) const RETENTION_RUN_READ_COLUMNS: [&str; 3] = ["env_key", "status", "created_at"];

impl StableGrantSet {
    pub(super) fn verify(
        self,
        role: &str,
        database: &str,
        required_database: &str,
        grants: &[RoleAcl],
        dynamic_targets: &[(String, String)],
    ) -> anyhow::Result<()> {
        match self {
            Self::ManagementAdmitter => {
                verify_management_admitter_grants(role, database, required_database, grants)
            }
            Self::RegistryReader => verify_system_reader_grants(
                SystemReader::Registry,
                "registry",
                &sql::REGISTRY_READER_RELATIONS,
                &[],
                role,
                database,
                required_database,
                grants,
            ),
            Self::IdentityReader => verify_system_reader_grants(
                SystemReader::Identity,
                "identity",
                &sql::IDENTITY_READER_RELATIONS,
                &sql::IDENTITY_READER_COLUMNS,
                role,
                database,
                required_database,
                grants,
            ),
            Self::ConnectionReader => {
                verify_connection_reader_grants(role, database, required_database, grants)
            }
            Self::SessionRoleReader => {
                verify_session_role_reader_grants(role, database, required_database, grants)
            }
            Self::Retention => verify_retention_grants(role, database, grants),
            Self::ExecutorPlatform => verify_executor_platform_grants(
                role,
                database,
                required_database,
                grants,
                dynamic_targets,
            ),
            Self::HttpAdmitter => {
                verify_http_admitter_grants(role, database, required_database, grants)
            }
            Self::EventMaterializer => {
                verify_event_materializer_grants(role, database, required_database, grants)
            }
            Self::AuditRetention => {
                verify_audit_retention_grants(role, database, grants, dynamic_targets)
            }
            Self::Administration => {
                verify_administration_grants(role, database, required_database, grants)
            }
            Self::Control => verify_control_grants(role, database, required_database, grants),
        }
    }
}
