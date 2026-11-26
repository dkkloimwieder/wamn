//! Records shared by identity callers.

use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::{PrincipalId, pat_prefix::PatPrefix};

/// The user created or reused for an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccount {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// Whether the user has a password credential.
    pub enrolled: bool,
}

/// One member of an org.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OrgUser {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// The user's email.
    pub email: String,
    /// The user's display name.
    pub display_name: String,
    /// The org-local status: `active` or `inactive`.
    pub status: String,
    /// The global principal status: `active` or `disabled`.
    pub principal_status: String,
    /// Whether the user holds `org-admin` in the org.
    pub org_admin: bool,
}

/// An environment and its registry state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvSummary {
    /// The environment name.
    pub name: wamn_registry::EnvName,
    /// The status represented by `registry.env.active`.
    pub state: String,
}

/// One user of a project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectMember {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// The user's email.
    pub email: String,
    /// The user's display name.
    pub display_name: String,
    /// Whether `org-admin` covers the project.
    pub org_admin: bool,
    /// Whether the user holds `project-admin` in the project.
    pub project_admin: bool,
    /// The environments of the project where the user is a member.
    pub environments: Vec<wamn_registry::EnvName>,
}

/// A project and its current state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectSummary {
    /// The project name.
    pub name: wamn_registry::ProjectId,
    /// The state exposed by project.list.
    pub state: String,
}

/// The grants an invitation asks for, beside the org membership.
#[derive(Debug, Default)]
pub struct MemberGrants {
    /// Grant `org-admin` in the org.
    pub org_admin: bool,
    /// Projects of the org where the user gets `project-admin`.
    pub project_admins: Vec<wamn_registry::ProjectId>,
    /// Environments of the org, as `(project, env)`, where the user gets a
    /// membership.
    pub memberships: Vec<(wamn_registry::ProjectId, wamn_registry::EnvName)>,
}

/// Public metadata of a stored token.
#[derive(Debug, Serialize)]
pub struct PatRecord {
    id: Uuid,
    token_prefix: PatPrefix,
    label: String,
    created_at: String,
    expires_at: String,
    revoked_at: Option<String>,
}

impl PatRecord {
    /// Build public metadata from the stored token fields.
    pub fn new(
        id: Uuid,
        token_prefix: PatPrefix,
        label: String,
        created_at: String,
        expires_at: String,
        revoked_at: Option<String>,
    ) -> Self {
        Self {
            id,
            token_prefix,
            label,
            created_at,
            expires_at,
            revoked_at,
        }
    }
}

/// RFC 9068 claims issued to the web client, without authorization or cookie state.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTokenClaims {
    /// Configured identity issuer.
    pub iss: String,
    /// The identity principal UUID.
    pub sub: PrincipalId,
    /// The org audience.
    pub aud: String,
    /// Exclusive expiry in Unix seconds.
    pub exp: i64,
    /// Actual signing time in Unix seconds.
    pub iat: i64,
    /// Unique token identifier.
    pub jti: String,
    /// The client that requested the access token.
    pub client_id: String,
}
