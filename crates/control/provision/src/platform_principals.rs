//! The platform principal rows that provisioning writes into a tenant database.
//!
//! Every platform component that writes in a tenant has an `app_system.users`
//! row of type `platform`. The row carries the tenant id, the derived id, the
//! `wamn:<component>` name, and the email `<component>@<platform-domain>`.
//! `deploy/sql/app-schema.sql` pins each name and id pair. Static SQL cannot
//! read deployment configuration, so this module checks the platform domain
//! and writes the email.
//!
//! A platform component that writes binds its principal id as `app.user_id`
//! and its `wamn:<component>` name as `app.operation`, so the record-history
//! triggers record that component.

use std::fmt::Write as _;

use std::fmt;

use wamn_pg_core::quote_literal;
use wamn_project_state::{OPERATION_CLAIM, PlatformComponent, USER_ID_CLAIM, USERS, UserType};

/// The longest domain name in text form, in bytes.
const MAX_DOMAIN_LEN: usize = 253;
/// The longest domain label, in bytes.
const MAX_LABEL_LEN: usize = 63;

/// Refusal of a platform domain that is not a valid domain name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformDomainError {
    domain: Box<str>,
}

impl PlatformDomainError {
    /// The refused platform domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

impl fmt::Display for PlatformDomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "platform domain {:?} is not a valid domain name: expected dot-separated labels of \
             lowercase letters, digits, and inner hyphens",
            self.domain
        )
    }
}

impl std::error::Error for PlatformDomainError {}

/// Render the statement that binds `component` as the actor and the operation
/// of the current transaction.
///
/// The statement binds the principal id as `app.user_id` and the
/// `wamn:<component>` name as `app.operation`. The binding is
/// transaction-local, so the caller runs the statement inside the transaction
/// that writes.
pub fn bind_platform_principal_sql(component: PlatformComponent) -> String {
    format!(
        "SELECT pg_catalog.set_config({}, {}, true), pg_catalog.set_config({}, {}, true);\n",
        quote_literal(USER_ID_CLAIM),
        quote_literal(&component.principal_id().to_string()),
        quote_literal(OPERATION_CLAIM),
        quote_literal(component.principal_name()),
    )
}

/// Render the SQL that creates the platform rows of one tenant.
///
/// The SQL first binds `wamn:provisioning` as the actor and the operation. The
/// `wamn:provisioning` row comes next, because it is the first row in a tenant
/// database, and it stamps itself. The other components follow. The SQL
/// carries no transaction control, so the caller runs it inside one
/// transaction. The binding ends with that transaction.
///
/// # Errors
///
/// Returns [`PlatformDomainError`] when `platform_domain` is not a valid
/// domain name.
pub fn platform_principals_sql(
    tenant: &str,
    platform_domain: &str,
) -> Result<String, PlatformDomainError> {
    validate_platform_domain(platform_domain)?;
    let tenant = quote_literal(tenant);
    let components = std::iter::once(PlatformComponent::Provisioning).chain(
        PlatformComponent::ALL
            .into_iter()
            .filter(|component| *component != PlatformComponent::Provisioning),
    );
    let mut sql = bind_platform_principal_sql(PlatformComponent::Provisioning);
    for component in components {
        writeln!(
            sql,
            "INSERT INTO {} (tenant_id, id, type, email, display_name) VALUES ({tenant}, {}, {}, {}, {});",
            USERS.qualified(),
            quote_literal(&component.principal_id().to_string()),
            quote_literal(UserType::Platform.as_str()),
            quote_literal(&format!("{}@{platform_domain}", component.as_str())),
            quote_literal(component.principal_name()),
        )
        .expect("writing to a String cannot fail");
    }
    Ok(sql)
}

/// Refuse a platform domain that is not a valid domain name.
///
/// A valid name has dot-separated labels of lowercase ASCII letters, digits,
/// and inner hyphens, each label at most 63 bytes and the name at most 253.
///
/// # Errors
///
/// Returns [`PlatformDomainError`] for any other value.
pub fn validate_platform_domain(domain: &str) -> Result<(), PlatformDomainError> {
    let valid = domain.len() <= MAX_DOMAIN_LEN
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= MAX_LABEL_LEN
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(PlatformDomainError {
            domain: domain.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_binding_names_the_actor_and_the_operation() {
        for component in PlatformComponent::ALL {
            assert_eq!(
                bind_platform_principal_sql(component),
                format!(
                    "SELECT pg_catalog.set_config('app.user_id', '{}', true), \
                     pg_catalog.set_config('app.operation', '{}', true);\n",
                    component.principal_id(),
                    component.principal_name(),
                )
            );
        }
        assert!(
            platform_principals_sql("t1", "example.invalid")
                .expect("a domain name renders")
                .starts_with(&bind_platform_principal_sql(
                    PlatformComponent::Provisioning
                )),
            "the platform rows are written as wamn:provisioning"
        );
    }

    #[test]
    fn a_domain_name_renders() {
        for domain in ["example.invalid", "localhost", "a-1.b2.example"] {
            assert!(
                platform_principals_sql("t1", domain).is_ok(),
                "{domain} is a valid domain name"
            );
        }
    }

    #[test]
    fn a_platform_domain_that_is_not_a_domain_name_is_refused() {
        let long_label = "a".repeat(MAX_LABEL_LEN + 1);
        let long_domain = ["a"; 128].join(".");
        for domain in [
            "",
            ".",
            "example.",
            ".example",
            "exa..mple",
            "-example.invalid",
            "example-.invalid",
            "Example.invalid",
            "example.invalid/path",
            "user@example.invalid",
            "exa mple.invalid",
            "example.invalid'",
            long_label.as_str(),
            long_domain.as_str(),
        ] {
            let error = platform_principals_sql("t1", domain).unwrap_err();
            assert_eq!(error.domain(), domain);
        }
    }
}
