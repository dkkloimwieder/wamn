//! Provisioned session audiences bind registry coordinates to one read credential.
//!
//! Documents contain a database credential and belong only in mounted Secrets.
//! HTTP requests select the derived audience, never a database URL or project.

use std::fmt;

use serde::{Deserialize, Serialize};
use wamn_registry::{Triple, identifiers::valid_project_env_key};

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

/// Validate the registry coordinate spelling.
pub fn validate_session_project_env_key(project_env_key: &str) -> Result<(), SessionTargetError> {
    if !valid_project_env_key(project_env_key) {
        return Err(refused("session target project refused"));
    }
    Ok(())
}

/// Derive the exact instance-aware audience from validated provisioning coordinates.
pub fn session_audience(
    triple: &Triple,
    instance_suffix: &str,
) -> Result<String, SessionTargetError> {
    validate_project_env(&triple.org, &triple.project, triple.env.as_str())
        .map_err(|_| refused("session target coordinates refused"))?;
    validate_instance_suffix(instance_suffix)
        .map_err(|_| refused("session target instance refused"))?;
    Ok(wamn_registry::name::project_audience(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
        instance_suffix,
    ))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetDocument {
    audience: String,
    org: String,
    project: String,
    env: String,
    instance_suffix: String,
    project_env_key: String,
    connection: ConnectionConfig,
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
        project_env_key: &str,
        database_url: &str,
    ) -> Result<Self, SessionTargetError> {
        let audience = session_audience(triple, instance_suffix)?;
        validate_session_project_env_key(project_env_key)?;
        if project_env_key
            != wamn_registry::project_env_key(&triple.org, &triple.project, triple.env.as_str())
        {
            return Err(refused("session target coordinate mismatch"));
        }
        let database = project_env_database_name(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            instance_suffix,
        );
        let connection: ConnectionConfig = serde_json::from_str(database_url)
            .map_err(|_| refused("session connection configuration refused"))?;
        let role = crate::workload_login(
            crate::WorkloadRoleFamily::SessionRoleReader,
            crate::WorkloadRoleIdentity::ProjectEnvironment {
                org: &triple.org,
                project: &triple.project,
                environment: triple.env.as_str(),
                database: &database,
            },
        )
        .map_err(|_| refused("session reader identity refused"))?;
        if connection.database != database || connection.username != role {
            return Err(refused("session reader configuration names another target"));
        }
        Ok(Self {
            document: TargetDocument {
                audience,
                org: triple.org.clone(),
                project: triple.project.clone(),
                env: triple.env.to_string(),
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
            env: document.env.as_str().into(),
        };
        let target = Self::new(
            &triple,
            &document.instance_suffix,
            &document.project_env_key,
            &document
                .connection
                .to_json()
                .map_err(|_| refused("session connection refused"))?,
        )?;
        if target.document.audience != document.audience
            || target.document.connection.database != document.connection.database
        {
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
    pub fn project_env_key(&self) -> &str {
        &self.document.project_env_key
    }
    /// Borrow the validated reader capability for the database driver only.
    pub fn connection(&self) -> &ConnectionConfig {
        &self.document.connection
    }
}
