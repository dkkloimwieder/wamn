//! Capture and reproduce effective application privileges on qualification scratch databases.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use tokio_postgres::Transaction;
use wamn_schema_generator::DATA_ACCESS_ROLE;

use super::{DataAccessReconcileResult, ReconcilePackageDataAccessRequest, quote_identifier};

const SCHEMA_PRIVILEGES: &[&str] = &["CREATE", "USAGE"];
const TABLE_PRIVILEGES: &[&str] = &[
    "DELETE",
    "INSERT",
    "MAINTAIN",
    "REFERENCES",
    "SELECT",
    "TRIGGER",
    "TRUNCATE",
    "UPDATE",
];
const COLUMN_PRIVILEGES: &[&str] = &["INSERT", "REFERENCES", "SELECT", "UPDATE"];

/// Sorted effective privilege facts; table grants remain distinct from column grants.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpgradePrivileges {
    pub(crate) schema: BTreeSet<(String, String)>,
    pub(crate) table: BTreeSet<(String, String, String)>,
    pub(crate) column: BTreeSet<(String, String, String, String)>,
}

/// Read every table-like relation, including undeclared history and control maps.
pub(crate) async fn read_upgrade_privileges(
    tx: &Transaction<'_>,
    schemas: &[String],
) -> anyhow::Result<UpgradePrivileges> {
    let sequence_access: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname = ANY($2::text[]) \
                AND CASE WHEN c.relkind = 'S' \
                  THEN has_sequence_privilege($1, c.oid, 'SELECT,UPDATE,USAGE') \
                  ELSE false END)",
            &[&DATA_ACCESS_ROLE, &schemas],
        )
        .await
        .context("check unsupported predecessor sequence authority")?
        .get(0);
    ensure!(
        !sequence_access,
        "package-upgrade-sequence-authority-refused: application sequence privileges cannot be reproduced by the schema/table/column qualification contract"
    );
    read_effective_privileges(tx, DATA_ACCESS_ROLE, schemas).await
}

/// Shared privilege reader for reconciliation and both sides of upgrade qualification.
pub(super) async fn read_effective_privileges(
    tx: &Transaction<'_>,
    role: &str,
    schemas: &[String],
) -> anyhow::Result<UpgradePrivileges> {
    let mut acl = UpgradePrivileges::default();
    for row in tx
        .query(
            "SELECT n.nspname::text, privilege FROM pg_namespace n \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND has_schema_privilege($1, n.oid, privilege)",
            &[&role, &schemas, &SCHEMA_PRIVILEGES],
        )
        .await
        .context("read effective application schema privileges")?
    {
        acl.schema.insert((row.get(0), row.get(1)));
    }
    for row in tx
        .query(
            "SELECT n.nspname::text, c.relname::text, privilege \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND c.relkind IN ('r','p','v','m','f') \
               AND has_table_privilege($1, c.oid, privilege)",
            &[&role, &schemas, &TABLE_PRIVILEGES],
        )
        .await
        .context("read effective application table privileges")?
    {
        acl.table.insert((row.get(0), row.get(1), row.get(2)));
    }
    for row in tx
        .query(
            "SELECT n.nspname::text, c.relname::text, a.attname::text, privilege \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid \
             CROSS JOIN unnest($3::text[]) AS privilege \
             WHERE n.nspname = ANY($2::text[]) AND c.relkind IN ('r','p','v','m','f') \
               AND a.attnum > 0 AND NOT a.attisdropped \
               AND has_column_privilege($1, c.oid, a.attnum, privilege)",
            &[&role, &schemas, &COLUMN_PRIVILEGES],
        )
        .await
        .context("read effective application column privileges")?
    {
        acl.column
            .insert((row.get(0), row.get(1), row.get(2), row.get(3)));
    }
    Ok(acl)
}

/// Restore captured package privileges only on the qualification-owned database.
///
/// The caller restores ownership and the stable platform role/function floor first.
/// This helper touches only the supplied application schemas and refuses if their
/// effective authority cannot be reproduced, including unexpected inherited grants.
pub(crate) async fn restore_upgrade_privileges(
    tx: &Transaction<'_>,
    schemas: &[String],
    captured: &UpgradePrivileges,
) -> anyhow::Result<()> {
    let role = quote_identifier(DATA_ACCESS_ROLE);
    let mut sql = String::new();
    for schema in schemas {
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON SCHEMA {} FROM PUBLIC, {role};",
            quote_identifier(schema)
        )?;
        writeln!(
            sql,
            "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {} FROM PUBLIC, {role};",
            quote_identifier(schema)
        )?;
    }
    // Table revocation does not remove a pre-existing column grant.
    for row in tx
        .query(
            "SELECT n.nspname::text, c.relname::text, \
                    array_agg(a.attname::text ORDER BY a.attname::text COLLATE \"C\") \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid \
             WHERE n.nspname = ANY($1::text[]) AND c.relkind IN ('r','p','v','m','f') \
               AND a.attnum > 0 AND NOT a.attisdropped \
             GROUP BY n.nspname, c.relname",
            &[&schemas],
        )
        .await
        .context("read scratch columns for privilege restoration")?
    {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let columns: Vec<String> = row.get(2);
        let columns = columns
            .iter()
            .map(|column| quote_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        for privilege in COLUMN_PRIVILEGES {
            writeln!(
                sql,
                "REVOKE {privilege} ({columns}) ON TABLE {}.{} FROM PUBLIC, {role};",
                quote_identifier(&schema),
                quote_identifier(&table)
            )?;
        }
    }
    for (schema, privilege) in &captured.schema {
        validate_fact(schemas, schema, privilege, SCHEMA_PRIVILEGES)?;
        writeln!(
            sql,
            "GRANT {privilege} ON SCHEMA {} TO {role};",
            quote_identifier(schema)
        )?;
    }
    for (schema, table, privilege) in &captured.table {
        validate_fact(schemas, schema, privilege, TABLE_PRIVILEGES)?;
        writeln!(
            sql,
            "GRANT {privilege} ON TABLE {}.{} TO {role};",
            quote_identifier(schema),
            quote_identifier(table)
        )?;
    }
    for (schema, table, column, privilege) in &captured.column {
        validate_fact(schemas, schema, privilege, COLUMN_PRIVILEGES)?;
        // An effective table grant already implies every current column; do
        // not turn that implication into independently retained column grants.
        if captured
            .table
            .contains(&(schema.clone(), table.clone(), privilege.clone()))
        {
            continue;
        }
        writeln!(
            sql,
            "GRANT {privilege} ({}) ON TABLE {}.{} TO {role};",
            quote_identifier(column),
            quote_identifier(schema),
            quote_identifier(table)
        )?;
    }
    tx.batch_execute(&sql)
        .await
        .context("restore captured predecessor application privileges")?;
    ensure!(
        &read_upgrade_privileges(tx, schemas).await? == captured,
        "package-upgrade-predecessor-privilege-restore-mismatch"
    );
    Ok(())
}

fn validate_fact(
    schemas: &[String],
    schema: &str,
    privilege: &str,
    allowed: &[&str],
) -> anyhow::Result<()> {
    ensure!(
        schemas.iter().any(|candidate| candidate == schema) && allowed.contains(&privilege),
        "package-upgrade-privilege-fact-invalid: schema={schema}; privilege={privilege}"
    );
    Ok(())
}

/// Run ordinary reconciliation on the private qualification copy.
pub(crate) async fn reconcile_for_upgrade(
    request: ReconcilePackageDataAccessRequest,
) -> anyhow::Result<DataAccessReconcileResult> {
    let packages = super::read_presented_packages(&request.packages)?;
    super::execute_prepared(
        &request.database_url,
        &request.tenant,
        &packages,
        super::ReconcileMode::Qualification,
        true,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{read_upgrade_privileges, restore_upgrade_privileges};
    use tokio_postgres::{NoTls, error::SqlState};

    #[tokio::test]
    async fn restored_column_grants_refuse_a_new_whole_row_but_preserve_table_grants() {
        let database =
            wamn_test_infrastructure::locked_database::database(wamn_test_postgres::database);
        let (mut client, connection) = tokio_postgres::connect(&database, NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        client
            .batch_execute(
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
                   THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$; \
                 CREATE SCHEMA inventory; \
                 CREATE TABLE inventory.widget (id int, description text); \
                 CREATE TABLE inventory.wamn_entities (id int); \
                 CREATE TABLE inventory.widget_history (id int); \
                 GRANT USAGE ON SCHEMA inventory TO wamn_app; \
                 GRANT SELECT (id, description), INSERT (id) ON inventory.widget TO wamn_app; \
                 GRANT SELECT ON inventory.wamn_entities, inventory.widget_history TO wamn_app;",
            )
            .await
            .unwrap();
        let schemas = vec!["inventory".to_owned()];
        let tx = client.transaction().await.unwrap();
        let captured = read_upgrade_privileges(&tx, &schemas).await.unwrap();
        for relation in ["wamn_entities", "widget_history"] {
            assert!(captured.table.contains(&(
                "inventory".to_owned(),
                relation.to_owned(),
                "SELECT".to_owned()
            )));
        }
        assert!(!captured.table.iter().any(|(_, table, _)| table == "widget"));
        restore_upgrade_privileges(&tx, &schemas, &captured)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        client
            .batch_execute(
                "ALTER TABLE inventory.widget ADD COLUMN note text; \
                 ALTER TABLE inventory.widget_history ADD COLUMN note text; \
                 SET ROLE wamn_app;",
            )
            .await
            .unwrap();
        client
            .batch_execute("EXPLAIN SELECT id, description FROM inventory.widget")
            .await
            .expect("old explicitly named columns remain readable");
        let error = client
            .batch_execute("EXPLAIN SELECT to_jsonb(widget) FROM inventory.widget")
            .await
            .expect_err("restoring predecessor column grants must not cover candidate columns");
        assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
        client
            .batch_execute("EXPLAIN SELECT to_jsonb(widget_history) FROM inventory.widget_history")
            .await
            .expect("a captured table grant still covers future columns");
        client.batch_execute("RESET ROLE").await.unwrap();
        drop(client);
        task.await.unwrap().unwrap();
    }
}
