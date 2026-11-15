//! The platform principal rows that provisioning writes into a project database.
//!
//! Every platform component that writes in a project has an `platform."user"`
//! row of type `platform`. The row carries the derived id, the
//! `wamn:<component>` name, and the email `<component>@<platform-domain>`.
//! `deploy/sql/app-schema.sql` pins each name and id pair. Static SQL cannot
//! read deployment configuration, so this module tests the platform domain
//! and writes the email.
//!
//! A platform component that writes binds its principal id in `platform.tx_identity.principal_id`
//! and its `wamn:<component>` name in `platform.tx_identity.operation`, so the record-history
//! triggers record that component.

use std::fmt::Write as _;

use postgres_protocol::escape::escape_literal;
use wamn_identity_protocol::{PlatformComponent, WORKFLOW_USER_NAME, workflow_user_id};
use wamn_registry::platform_domain::PlatformDomain;

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

/// Render the SQL that creates the platform rows of one project.
///
/// The SQL first binds `wamn:provisioning` as the actor and the operation. The
/// `wamn:provisioning` row comes next, because it is the first row in a project
/// database, and it stamps itself. The other components follow. The SQL
/// carries no transaction control, so the caller runs it inside one
/// transaction. The binding ends with that transaction.
///
/// The last row is the `workflow` user of type `service`
/// (docs/architecture/execution.md#environment-workflows), with the email
/// `workflow@<platform-domain>`. The platform grants it no role.
///
/// Each insert keeps a row that is already there. Running the SQL twice, or
/// running it against a project that holds part of the list, writes the rest
/// and changes nothing else (`wamn-0h0g.9.15`).
///
pub fn platform_principals_sql(platform_domain: &PlatformDomain) -> String {
    let components = std::iter::once(PlatformComponent::Provisioning).chain(
        PlatformComponent::ALL
            .into_iter()
            .filter(|component| *component != PlatformComponent::Provisioning),
    );
    let mut sql = bind_platform_principal_sql(PlatformComponent::Provisioning);
    for component in components {
        writeln!(
            sql,
            "INSERT INTO platform.\"user\" (id, type, email, display_name) VALUES ({}, {}, {}, {}) ON CONFLICT (id) DO NOTHING;",
            escape_literal(&component.principal_id().to_string()),
            escape_literal("platform"),
            escape_literal(&format!("{}@{platform_domain}", component.as_str())),
            escape_literal(&component.principal_name()),
        )
        .expect("writing to a String cannot fail");
    }
    writeln!(
        sql,
        "INSERT INTO platform.\"user\" (id, type, email, display_name) VALUES ({}, {}, {}, {}) ON CONFLICT (id) DO NOTHING;",
        escape_literal(&workflow_user_id().to_string()),
        escape_literal("service"),
        escape_literal(&format!("{WORKFLOW_USER_NAME}@{platform_domain}")),
        escape_literal(WORKFLOW_USER_NAME),
    )
    .expect("writing to a String cannot fail");
    sql
}
