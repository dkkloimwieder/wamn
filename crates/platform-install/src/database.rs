//! Public database configuration and the credential mount layout.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Root of the database credential mounts in deployed processes.
pub const DATABASE_DIR: &str = "/etc/wamn/db";

/// A named credential source in the organization's namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub namespace: String,
    pub name: String,
}

/// Public connection data. Login and CA bytes come from mounted files.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: wamn_role::LoginName,
    pub credentials: SecretRef,
    pub server_ca: SecretRef,
}

impl ConnectionConfig {
    /// Encode configuration without including credential or CA bytes.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// Resolve one named login directory below the supplied mount root.
pub fn credential_directory(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid database credential name"
    );
    Ok(root.join(name))
}

pub fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: wamn_role::LoginName,
    secret: String,
) -> ConnectionConfig {
    ConnectionConfig {
        host,
        port,
        database,
        username,
        credentials: SecretRef {
            namespace: namespace.into(),
            name: secret,
        },
        server_ca: SecretRef {
            namespace: namespace.into(),
            name: format!("{cluster}-ca"),
        },
    }
}
