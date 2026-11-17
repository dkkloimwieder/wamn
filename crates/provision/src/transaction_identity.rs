//! Platform component identities bound to the transaction that writes.

use postgres_protocol::escape::escape_literal;
use wamn_identity_protocol::PlatformComponent;

/// Render the statement that binds `component` as the actor and the operation
/// of the current transaction.
///
/// The statement binds the principal id in `platform.tx_identity.principal_id` and the
/// `wamn:<component>` name in `platform.tx_identity.operation`. The binding is
/// transaction-local, so the caller runs the statement inside the transaction
/// that writes.
pub fn bind_platform_principal_sql(component: PlatformComponent) -> String {
    format!(
        "INSERT INTO platform.tx_identity (xid, principal_id, role, application, operation) \
         VALUES (pg_current_xact_id(), {}::uuid, '', '', {});\n",
        escape_literal(&component.principal_id().to_string()),
        escape_literal(&component.principal_name()),
    )
}
