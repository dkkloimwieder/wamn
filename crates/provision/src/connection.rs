//! CloudNativePG managed role declarations.

use serde::{Deserialize, Serialize};

/// One role managed by the PostgreSQL controller in the organization's namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the CloudNativePG role schema defines these independent privilege booleans"
)]
pub struct ManagedRole {
    pub name: wamn_identity_protocol::LoginName,
    pub ensure: String,
    pub login: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_limit: Option<u32>,
    pub superuser: bool,
    pub createdb: bool,
    pub createrole: bool,
    pub inherit: bool,
    pub replication: bool,
    pub bypassrls: bool,
    pub in_roles: Vec<String>,
    pub password_secret: PasswordSecret,
}

/// The basic-auth Secret that supplies one role's password.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordSecret {
    pub name: String,
}

/// Declare a restricted login that inherits its one function's privileges.
pub fn managed_role(
    name: wamn_identity_protocol::LoginName,
    acl_role: &str,
    secret: String,
) -> ManagedRole {
    ManagedRole {
        name,
        ensure: "present".into(),
        login: true,
        connection_limit: None,
        superuser: false,
        createdb: false,
        createrole: false,
        inherit: true,
        replication: false,
        bypassrls: false,
        in_roles: if acl_role.is_empty() {
            Vec::new()
        } else {
            vec![acl_role.into()]
        },
        password_secret: PasswordSecret { name: secret },
    }
}
