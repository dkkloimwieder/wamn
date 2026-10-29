//! Refuse deletion before system authority or projected rows change.

use serde_json::Value;
use tokio_postgres::Transaction;

use super::{ConnectionError, refuse};

async fn exists(tx: &Transaction<'_>, relation: &str) -> Result<bool, ConnectionError> {
    Ok(tx
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&relation])
        .await?
        .get(0))
}

/// IS1.5 completes this function with the singular catalog tables and their projections.
pub async fn refuse_environment_references(
    tx: &Transaction<'_>,
    group: &str,
    id: &str,
    name: &str,
) -> Result<(), ConnectionError> {
    let projection = match group {
        "connection" => "platform.connection",
        "integration" => "platform.integration",
        "external_system" => "platform.external_system",
        _ => return Err(refuse("invalid_input", "operation")),
    };
    if exists(tx, projection).await? {
        // Foreign-key inserts wait here until deletion commits or rolls back.
        tx.query(
            &format!("SELECT id FROM {projection} WHERE id=$1::text::uuid FOR UPDATE"),
            &[&id],
        )
        .await?;
    }
    if group == "connection" {
        // Existing releases identify a system connection through its integration name.
        if exists(tx, "catalog.connection_bindings").await?
            && exists(tx, "catalog.connection_instances").await?
        {
            let referenced: bool = tx.query_one(
                "SELECT EXISTS(SELECT FROM catalog.connection_bindings b JOIN catalog.connection_instances i \
                 ON (i.env_key,i.environment,i.instance_id)=(b.env_key,b.environment,b.instance_id) \
                 WHERE i.org_connection_name=$1 OR i.instance_id=$2)", &[&name, &id],
            ).await?.get(0);
            if referenced {
                return Err(refuse("referenced", "binding"));
            }
        }
        for (relation, field, sql) in [
            (
                "catalog.connection_binding",
                "binding",
                "SELECT EXISTS(SELECT FROM catalog.connection_binding WHERE connection_id=$1::text::uuid)",
            ),
            (
                "catalog.release_connection",
                "release_connection",
                "SELECT EXISTS(SELECT FROM catalog.release_connection WHERE connection_id=$1::text::uuid)",
            ),
            (
                "platform.file",
                "file",
                "SELECT EXISTS(SELECT FROM platform.file WHERE connection_id=$1::text::uuid)",
            ),
        ] {
            if exists(tx, relation).await? && tx.query_one(sql, &[&id]).await?.get::<_, bool>(0) {
                return Err(refuse("referenced", field));
            }
        }
    } else if group == "external_system" && exists(tx, "platform.external_system").await? {
        // A foreign key identifies every record source, including application tables.
        for row in tx.query(
            "SELECT quote_ident(n.nspname)||'.'||quote_ident(t.relname), quote_ident(a.attname) \
             FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid \
             JOIN pg_namespace n ON n.oid=t.relnamespace \
             JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=c.conkey[1] \
             WHERE c.contype='f' AND c.confrelid='platform.external_system'::regclass \
               AND cardinality(c.conkey)=1 AND NOT (n.nspname='platform' AND t.relname='integration')",
            &[],
        ).await? {
            let relation: String = row.get(0);
            let column: String = row.get(1);
            if tx.query_one(&format!("SELECT EXISTS(SELECT FROM {relation} WHERE {column}=$1::text::uuid)"), &[&id]).await?.get::<_, bool>(0) {
                return Err(refuse("referenced", "record"));
            }
        }
    }
    Ok(())
}

/// Remove only the deleted identity from each database after the system commit.
pub async fn remove_projection(
    tx: &Transaction<'_>,
    group: &str,
    id: &str,
) -> Result<(), ConnectionError> {
    let relation = match group {
        "connection" => "platform.connection",
        "integration" => "platform.integration",
        "external_system" => "platform.external_system",
        _ => return Err(refuse("invalid_input", "operation")),
    };
    if exists(tx, relation).await? {
        if group == "connection" && exists(tx, "platform.project_connection").await? {
            tx.execute(
                "DELETE FROM platform.project_connection WHERE connection_id=$1::text::uuid",
                &[&id],
            )
            .await?;
        }
        tx.execute(
            &format!("DELETE FROM {relation} WHERE id=$1::text::uuid"),
            &[&id],
        )
        .await?;
    }
    Ok(())
}

/// IS1.5 fills the new projection through the project-state writer.
pub fn project_administration(
    _system: &Transaction<'_>,
    _environment: &Transaction<'_>,
    _org: &str,
    _project: &str,
    _env: &str,
) {
}

/// Destroy every private object after the connection and projected rows disappear.
pub async fn destroy_credentials(credentials: &Value) -> Result<(), ConnectionError> {
    for credential in credentials.as_array().into_iter().flatten() {
        let store =
            wamn_credential_store::open(credential["store"].as_str().expect("database store"))
                .map_err(|source| ConnectionError {
                    code: "store_error",
                    field: "credential",
                    source: Some(Box::new(source)),
                })?;
        store
            .destroy(
                credential["ref"].as_str().expect("database reference"),
                credential["version"].as_str().expect("database version"),
            )
            .await
            .map_err(|source| ConnectionError {
                code: "store_error",
                field: "credential",
                source: Some(Box::new(source)),
            })?;
    }
    Ok(())
}
