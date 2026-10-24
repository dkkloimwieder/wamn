//! One row of each platform configuration table that the control host writes
//! (docs/plan/platform-config.md, Tables; issue `wamn-aqd0m.3`), and the read
//! of those rows back.
//!
//! `run_plane_live` and `environment_apply_kind` [`write`] the rows of
//! [`configuration`] with the environment's `wamn_administration` login, as
//! the control host writes the application rows of an environment, and
//! compare [`read_back`], read as a generation of `wamn_app`, with
//! [`expected`]. [`history`] reads the history entries of the writes.

use serde_json::{Value, json};
use wamn_platform_identity::configuration::{
    Connection, ExternalSystem, IntegrationAllow, NumberSequence, PlatformConfiguration,
    ProjectConnection, Setting, write_configuration,
};

const EXTERNAL_SYSTEM_ID: &str = "a0000000-0000-4000-8000-000000000001";
const CONNECTION_ID: &str = "a0000000-0000-4000-8000-000000000002";
const NUMBER_SEQUENCE_ID: &str = "a0000000-0000-4000-8000-000000000003";

/// The caller that the writes bind, as a control host route binds its
/// caller. It stamps every row.
pub const CALLER: &str = "a0000000-0000-4000-8000-0000000000c1";

/// The operation that the writes bind, as a control host route binds its
/// sealed operation id.
pub const OPERATION: &str = "admin:platform-configuration";

/// The six tables, in the order of [`expected`].
pub const TABLES: [&str; 6] = [
    "external_system",
    "connection",
    "project_connection",
    "integration_allow",
    "number_sequence",
    "setting",
];

/// Write `configuration` with the `wamn_administration` login `client`, in
/// one transaction that binds [`CALLER`] and [`OPERATION`]. It returns the
/// rows inserted or changed.
pub async fn write(
    client: &mut tokio_postgres::Client,
    configuration: &PlatformConfiguration,
) -> anyhow::Result<u64> {
    let transaction = client.transaction().await?;
    transaction
        .execute(
            "SELECT pg_catalog.set_config('app.user_id', $1, true), \
             pg_catalog.set_config('app.operation', $2, true)",
            &[&CALLER, &OPERATION],
        )
        .await?;
    let written = write_configuration(&transaction, configuration).await?;
    transaction.commit().await?;
    Ok(written)
}

/// One row of each table. The connection reaches the external system, and the
/// project enables the connection.
pub fn configuration(project: &str) -> PlatformConfiguration {
    PlatformConfiguration {
        external_systems: vec![ExternalSystem {
            id: EXTERNAL_SYSTEM_ID.to_owned(),
            integration: "netsuite".to_owned(),
            name: "netsuite-main".to_owned(),
            parameters: json!({"account_id": "1234567"}),
            status: "active".to_owned(),
        }],
        connections: vec![Connection {
            id: CONNECTION_ID.to_owned(),
            integration: "netsuite".to_owned(),
            external_system_id: Some(EXTERNAL_SYSTEM_ID.to_owned()),
            name: "netsuite-orders".to_owned(),
            transport: "http".to_owned(),
            exchange: "request".to_owned(),
            parameters: json!({"endpoint": "https://example.invalid/orders"}),
            status: "active".to_owned(),
        }],
        project_connections: vec![ProjectConnection {
            project_id: project.to_owned(),
            connection_id: CONNECTION_ID.to_owned(),
        }],
        integration_allows: vec![IntegrationAllow {
            scope_type: "project".to_owned(),
            scope_id: project.to_owned(),
            integration: "netsuite".to_owned(),
        }],
        number_sequences: vec![NumberSequence {
            id: NUMBER_SEQUENCE_ID.to_owned(),
            application: "receiving".to_owned(),
            name: "receipt".to_owned(),
            pattern: "RC-{yyyy}-{seq:5}".to_owned(),
        }],
        settings: vec![Setting {
            scope_type: "project".to_owned(),
            scope_id: project.to_owned(),
            key: "date_format".to_owned(),
            value: json!("yyyy-MM-dd"),
            locked: false,
        }],
    }
}

/// The rows [`read_back`] answers after [`configuration`] is written by
/// [`write`], each stamped by [`CALLER`].
pub fn expected(project: &str) -> Vec<Value> {
    let stamped = |table: &str, mut row: Value| {
        row["created_by"] = json!(CALLER);
        row["updated_by"] = json!(CALLER);
        json!({"table": table, "row": row})
    };
    vec![
        stamped(
            "external_system",
            json!({"id": EXTERNAL_SYSTEM_ID, "integration": "netsuite", "name": "netsuite-main",
                   "parameters": {"account_id": "1234567"}, "status": "active"}),
        ),
        stamped(
            "connection",
            json!({"id": CONNECTION_ID, "integration": "netsuite",
                   "external_system_id": EXTERNAL_SYSTEM_ID, "name": "netsuite-orders",
                   "transport": "http", "exchange": "request",
                   "parameters": {"endpoint": "https://example.invalid/orders"},
                   "status": "active"}),
        ),
        stamped(
            "project_connection",
            json!({"project_id": project, "connection_id": CONNECTION_ID}),
        ),
        stamped(
            "integration_allow",
            json!({"scope_type": "project", "scope_id": project, "integration": "netsuite"}),
        ),
        stamped(
            "number_sequence",
            json!({"id": NUMBER_SEQUENCE_ID, "application": "receiving", "name": "receipt",
                   "pattern": "RC-{yyyy}-{seq:5}"}),
        ),
        stamped(
            "setting",
            json!({"scope_type": "project", "scope_id": project, "key": "date_format",
                   "value": "yyyy-MM-dd", "locked": false}),
        ),
    ]
}

/// Every row of the six tables that `client` can read, without the two
/// timestamps, in the order of [`expected`].
pub async fn read_back(client: &tokio_postgres::Client) -> anyhow::Result<Vec<Value>> {
    let mut rows = Vec::new();
    for table in TABLES {
        for row in client
            .query(
                &format!(
                    "SELECT (to_jsonb(t) - 'created_at' - 'updated_at')::text \
                       FROM platform.{table} AS t ORDER BY 1"
                ),
                &[],
            )
            .await?
        {
            let row: Value = serde_json::from_str(&row.get::<_, String>(0))?;
            rows.push(json!({"table": table, "row": row}));
        }
    }
    Ok(rows)
}

/// The history entries of the six tables that `client` can read, each as
/// `<table> <type> <operation> <changed_by>`, in the order of [`TABLES`] and
/// then of position.
pub async fn history(client: &tokio_postgres::Client) -> anyhow::Result<Vec<String>> {
    let mut entries = Vec::new();
    for table in TABLES {
        for row in client
            .query(
                &format!(
                    "SELECT type || ' ' || operation || ' ' || changed_by::text \
                       FROM platform.{table}_history ORDER BY position"
                ),
                &[],
            )
            .await?
        {
            entries.push(format!("{table} {}", row.get::<_, String>(0)));
        }
    }
    Ok(entries)
}
