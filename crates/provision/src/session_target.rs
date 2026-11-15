//! Provisioned session audiences bind registry coordinates to one read credential.
//!
//! Documents contain a database credential and belong only in mounted Secrets.
//! HTTP requests select the derived audience, never a database URL or project.

use std::fmt;

use serde::{Deserialize, Serialize};
use wamn_registry::Triple;

use crate::name::{project_env_database_name, validate_instance_suffix, validate_project_env};
use wamn_postgres::ConnectionConfig;

/// The Secret key mounted as one explicitly configured target file.
pub const SESSION_TARGET_KEY: &str = "target.json";

/// A configuration refusal whose diagnostics never contain a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTargetError {
    context: &'static str,
}

impl fmt::Display for SessionTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl std::error::Error for SessionTargetError {}

fn refused(context: &'static str) -> SessionTargetError {
    SessionTargetError { context }
}

/// Derive the exact instance-aware audience from validated provisioning coordinates.
pub fn session_audience(
    triple: &Triple,
    instance_suffix: &str,
) -> Result<String, SessionTargetError> {
    validate_project_env(&triple.org, &triple.project, &triple.env)
        .map_err(|_| refused("session target coordinates refused"))?;
    validate_instance_suffix(instance_suffix)
        .map_err(|_| refused("session target instance refused"))?;
    Ok(wamn_registry::name::project_audience(
        &triple.org,
        &triple.project,
        &triple.env,
        instance_suffix,
    ))
}

/// A native cluster reference or an explicit operator URL for a local process.
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SessionConnection {
    References(ConnectionConfig),
    Operator(String),
}

impl std::fmt::Debug for SessionConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionConnection([REDACTED])")
    }
}
impl SessionConnection {
    /// Input for the shared SQLx constructor.
    pub fn input(&self) -> anyhow::Result<String> {
        match self {
            Self::References(config) => config.to_json(),
            Self::Operator(input) => Ok(input.clone()),
        }
    }
    pub fn database(&self) -> anyhow::Result<String> {
        Ok(wamn_postgres::identity(&self.input()?)?.1)
    }
    pub fn username(&self) -> anyhow::Result<wamn_identity_protocol::LoginName> {
        Ok(wamn_postgres::identity(&self.input()?)?.0)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetDocument {
    audience: String,
    org: String,
    project: String,
    env: wamn_registry::EnvName,
    instance_suffix: String,
    project_env_key: wamn_registry::ProjectEnvKey,
    connection: SessionConnection,
}

/// One validated target; cloning preserves its exact target and redacted diagnostics.
#[derive(Clone)]
pub struct SessionTarget {
    document: TargetDocument,
    triple: Triple,
}

impl fmt::Debug for SessionTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionTarget")
            .field("audience", &self.document.audience)
            .field("connection", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SessionTarget {
    /// Bind a provisioned instance and trusted project to its exact reader credential.
    pub fn new(
        triple: &Triple,
        instance_suffix: &str,
        project_env_key: &wamn_registry::ProjectEnvKey,
        database_url: &str,
    ) -> Result<Self, SessionTargetError> {
        let audience = session_audience(triple, instance_suffix)?;
        if *project_env_key
            != wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env)
        {
            return Err(refused("session target coordinate mismatch"));
        }
        let database =
            project_env_database_name(&triple.org, &triple.project, &triple.env, instance_suffix);
        let connection = if database_url.trim_start().starts_with('{') {
            SessionConnection::References(
                serde_json::from_str(database_url)
                    .map_err(|_| refused("session connection configuration refused"))?,
            )
        } else {
            SessionConnection::Operator(database_url.to_owned())
        };
        let (username, selected_database) = wamn_postgres::identity(database_url)
            .map_err(|_| refused("session reader configuration refused"))?;
        let role = crate::workload_login(
            crate::WorkloadRoleFamily::SessionRoleReader,
            crate::WorkloadRoleIdentity::ProjectEnvironment {
                org: &triple.org,
                project: &triple.project,
                environment: &triple.env,
                database: &database,
            },
        )
        .map_err(|_| refused("session reader identity refused"))?;
        if selected_database != database || username != role {
            return Err(refused("session reader configuration names another target"));
        }
        Ok(Self {
            document: TargetDocument {
                audience,
                org: triple.org.clone(),
                project: triple.project.clone(),
                env: triple.env.clone(),
                instance_suffix: instance_suffix.to_owned(),
                project_env_key: project_env_key.to_owned(),
                connection,
            },
            triple: triple.clone(),
        })
    }

    /// Parse a mounted document and reject coordinate, audience, or credential mismatches.
    pub fn from_json(bytes: &[u8]) -> Result<Self, SessionTargetError> {
        let document: TargetDocument = serde_json::from_slice(bytes)
            .map_err(|_| refused("session target document refused"))?;
        let triple = Triple {
            org: document.org.clone(),
            project: document.project.clone(),
            env: document.env.clone(),
        };
        let target = Self::new(
            &triple,
            &document.instance_suffix,
            &document.project_env_key,
            &document
                .connection
                .input()
                .map_err(|_| refused("session connection refused"))?,
        )?;
        if target.document.audience != document.audience {
            return Err(refused("session target target mismatch"));
        }
        Ok(target)
    }

    /// Serialize the credential-bearing document for protected Secret output, never logs.
    pub fn to_json(&self) -> Result<String, SessionTargetError> {
        serde_json::to_string(&self.document).map_err(|_| refused("encode session target failed"))
    }

    /// Borrow the exact public audience identifier.
    pub fn audience(&self) -> &str {
        &self.document.audience
    }
    /// Borrow the trusted registry coordinates.
    pub fn triple(&self) -> &Triple {
        &self.triple
    }
    /// Borrow the provisioned instance suffix.
    pub fn instance_suffix(&self) -> &str {
        &self.document.instance_suffix
    }
    /// Borrow the trusted project identity used in every role predicate.
    pub fn project_env_key(&self) -> &wamn_registry::ProjectEnvKey {
        &self.document.project_env_key
    }
    /// Borrow the validated reader capability for the database driver only.
    pub fn connection(&self) -> &SessionConnection {
        &self.document.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_connections_keep_their_target_for_cluster_and_local_inputs() {
        let triple = Triple::new("acme", "dev".parse().unwrap(), "orders");
        let instance = "aaaaaaa1";
        let database =
            project_env_database_name("acme", "orders", &"dev".parse().unwrap(), instance);
        let username = crate::workload_login(
            crate::WorkloadRoleFamily::SessionRoleReader,
            crate::WorkloadRoleIdentity::ProjectEnvironment {
                org: "acme",
                project: "orders",
                environment: &"dev".parse().unwrap(),
                database: &database,
            },
        )
        .unwrap();
        let native = ConnectionConfig {
            host: "database.acme.svc".into(),
            port: 5432,
            database: database.clone(),
            username: username.clone(),
            credentials: wamn_postgres::SecretRef {
                namespace: "acme".into(),
                name: "reader".into(),
            },
            server_ca: wamn_postgres::SecretRef {
                namespace: "acme".into(),
                name: "database-ca".into(),
            },
        };
        let key = wamn_registry::project_env_key("acme", "orders", &"dev".parse().unwrap());
        let explicit = format!("postgres://{username}:password@localhost/{database}");
        for input in [native.to_json().unwrap(), explicit] {
            let target = SessionTarget::new(&triple, instance, &key, &input).unwrap();
            let decoded = SessionTarget::from_json(target.to_json().unwrap().as_bytes()).unwrap();
            assert_eq!(decoded.triple(), target.triple());
            assert_eq!(decoded.project_env_key(), target.project_env_key());
            assert_eq!(decoded.instance_suffix(), target.instance_suffix());
        }
        assert!(
            SessionTarget::new(
                &triple,
                instance,
                &key,
                &format!("postgres://other@localhost/{database}")
            )
            .is_err()
        );
        assert!(
            SessionTarget::new(
                &triple,
                instance,
                &key,
                r#"{"url":"postgres://other@localhost/database"}"#
            )
            .is_err()
        );
    }
}
