//! Provisioned session audiences bind registry coordinates to one read credential.
//!
//! Documents contain a database credential and belong only in mounted Secrets.
//! HTTP requests select the derived audience, never a database URL or tenant.

use std::fmt;

use serde::{Deserialize, Serialize};
use wamn_control_registry::{Triple, identifiers::valid_tenant};

use crate::name::{project_env_database_name, validate_instance_suffix, validate_project_env};
use crate::session_role_reader::{SessionRoleReaderConnection, parse_session_role_reader_url};

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

/// Apply the existing tenant-claim spelling, without creating another identity format.
pub fn validate_session_tenant_id(tenant: &str) -> Result<(), SessionTargetError> {
    if !valid_tenant(tenant) {
        return Err(refused("session target tenant refused"));
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
    Ok(format!(
        "urn:wamn:project-env:{}:{}:{}:{instance_suffix}",
        triple.org, triple.project, triple.env
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
    tenant_id: String,
    database: String,
    database_url: String,
}

/// One validated target; cloning preserves its exact scope and redacted diagnostics.
#[derive(Clone)]
pub struct SessionTarget {
    document: TargetDocument,
    triple: Triple,
    connection: SessionRoleReaderConnection,
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
    /// Bind a provisioned instance and trusted tenant to its exact reader credential.
    pub fn new(
        triple: &Triple,
        instance_suffix: &str,
        tenant_id: &str,
        database_url: &str,
    ) -> Result<Self, SessionTargetError> {
        let audience = session_audience(triple, instance_suffix)?;
        validate_session_tenant_id(tenant_id)?;
        let database = project_env_database_name(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            instance_suffix,
        );
        let connection = parse_session_role_reader_url(
            database_url,
            &triple.org,
            &triple.project,
            triple.env.as_str(),
            &database,
        )
        .map_err(|_| refused("session target reader credential refused"))?;
        Ok(Self {
            document: TargetDocument {
                audience,
                org: triple.org.clone(),
                project: triple.project.clone(),
                env: triple.env.to_string(),
                instance_suffix: instance_suffix.to_owned(),
                tenant_id: tenant_id.to_owned(),
                database,
                database_url: database_url.to_owned(),
            },
            triple: triple.clone(),
            connection,
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
            &document.tenant_id,
            &document.database_url,
        )?;
        if target.document.audience != document.audience
            || target.document.database != document.database
        {
            return Err(refused("session target scope mismatch"));
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
    /// Borrow the trusted tenant claim used in every role predicate.
    pub fn tenant_id(&self) -> &str {
        &self.document.tenant_id
    }
    /// Borrow the validated reader capability for the database driver only.
    pub fn connection(&self) -> &SessionRoleReaderConnection {
        &self.connection
    }
}
