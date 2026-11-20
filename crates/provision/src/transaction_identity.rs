//! Platform component identities bound to the transaction that writes.

use sqlx::PgConnection;
use wamn_identity_protocol::PlatformComponent;

/// Bind a component as the caller and operation in the open transaction.
pub async fn bind_platform_principal(
    connection: &mut PgConnection,
    component: PlatformComponent,
) -> Result<(), sqlx::Error> {
    wamn_postgres::bind_transaction_identity(
        connection,
        &component.principal_id(),
        "",
        "",
        Some(&component.principal_name()),
        None,
    )
    .await
}

/// Render a component binding for a transaction batch.
pub fn bind_platform_principal_sql(component: PlatformComponent) -> String {
    let mut statement = wamn_postgres::transaction_identity_sql(
        &component.principal_id(),
        "",
        "",
        Some(&component.principal_name()),
        None,
    );
    statement.push_str(";\n");
    statement
}
