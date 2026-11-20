//! Org, environment, and project coordinates.

use serde::{Deserialize, Serialize};

use crate::{OrgId, ProjectId};

/// The control-plane identity triple `(org, env, project)` — the key every
/// subsystem speaks (registry rows, provisioning, subdomain routing, dispatcher
/// registration, promotion tooling). Tooling keys off the triple rather than
/// parsing a provisioned name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Triple {
    pub org: OrgId,
    pub env: crate::EnvName,
    pub project: ProjectId,
}

impl Triple {
    pub fn new(org: OrgId, env: crate::EnvName, project: ProjectId) -> Self {
        Self { org, env, project }
    }
}

impl std::fmt::Display for Triple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.org, self.env, self.project)
    }
}

/// The coordinate of an environment shared by its project deployments.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub org: OrgId,
    pub env: crate::EnvName,
}

/// The status of a project environment
/// (docs/architecture/execution.md#host-run-routes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentStatus {
    /// Identity offers its audience and its host serves its routes.
    Active,
    /// Identity offers no audience and its host answers
    /// `environment-inactive`. CDC, data and grants stay.
    Inactive,
}

impl EnvironmentStatus {
    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
}

/// The environment row of the system database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub disposable: bool,
    pub instance_suffix: crate::instance_suffix::InstanceSuffix,
}
