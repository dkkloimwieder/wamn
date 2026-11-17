//! Refuse deletion before system authority or projected rows change.

use serde_json::Value;
use sqlx::Row as _;
use wamn_kubernetes::Store as _;

use super::{ConnectionError, refuse};

fn projection_relation(group: &str) -> Result<&'static str, ConnectionError> {
    match group {
        "connection" => Ok("platform.connection"),
        "external_system" => Ok("platform.external_system"),
        _ => Err(refuse("invalid_input", "operation")),
    }
}

/// Refuse deleting a row that this environment database still references.
pub async fn refuse_environment_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group: &str,
    id: &str,
) -> Result<(), ConnectionError> {
    let projection = projection_relation(group)?;
    // Foreign-key inserts wait here until deletion commits or rolls back.
    sqlx::query(sqlx::AssertSqlSafe(
        format!("SELECT id FROM {projection} WHERE id=$1::text::uuid FOR UPDATE").clone(),
    ))
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    if group == "connection" {
        for (field, sql) in [(
            "file",
            "SELECT EXISTS(SELECT FROM platform.file WHERE connection_id=$1::text::uuid)",
        )] {
            if sqlx::query(sqlx::AssertSqlSafe((sql).to_string()))
                .bind(id)
                .fetch_one(&mut **tx)
                .await?
                .get::<bool, _>(0)
            {
                return Err(refuse("referenced", field));
            }
        }
    } else if group == "external_system" {
        // A foreign key identifies every record source, including application tables.
        for row in sqlx::query(
            "SELECT quote_ident(n.nspname)||'.'||quote_ident(t.relname), quote_ident(a.attname) \
             FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid \
             JOIN pg_namespace n ON n.oid=t.relnamespace \
             JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=c.conkey[1] \
             WHERE c.contype='f' AND c.confrelid='platform.external_system'::regclass \
               AND cardinality(c.conkey)=1",
        )
        .fetch_all(&mut **tx)
        .await?
        {
            let relation: String = row.get(0);
            let column: String = row.get(1);
            if sqlx::query(sqlx::AssertSqlSafe(
                format!("SELECT EXISTS(SELECT FROM {relation} WHERE {column}=$1::text::uuid)")
                    .clone(),
            ))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?
            .get::<bool, _>(0)
            {
                return Err(refuse("referenced", "record"));
            }
        }
    }
    Ok(())
}

/// A deployed workload retains authority for every connection in its config.
pub fn refuse_application_references(
    applications: &[wamn_catalog::application_config::ApplicationConfig],
    id: &str,
) -> Result<(), ConnectionError> {
    if applications
        .iter()
        .flat_map(|app| &app.components)
        .flat_map(|component| &component.connections)
        .any(|binding| binding.connection_id == id)
    {
        return Err(refuse("referenced", "binding"));
    }
    Ok(())
}

/// Destroy every private object after the connection and projected rows disappear.
pub async fn destroy_credentials(credentials: &Value) -> Result<(), ConnectionError> {
    for credential in credentials.as_array().into_iter().flatten() {
        let store =
            wamn_kubernetes::kubernetes::KubernetesStore::in_cluster().map_err(|source| {
                ConnectionError {
                    code: "store_error",
                    field: "credential",
                    source: Some(Box::new(source)),
                }
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
