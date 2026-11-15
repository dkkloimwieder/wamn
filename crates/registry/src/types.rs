//! Org, environment, and project coordinates.

use serde::{Deserialize, Serialize};

/// An organization id — a lowercase slug. It embeds into cluster / Secret /
/// subdomain names, so it follows the platform slug discipline (see
/// the provisioning boundary, mirroring `wamn-control-provision` / wi4 / 66x).
pub type OrgId = String;

/// A project id — a lowercase slug, unique within its org.
pub type ProjectId = String;

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
    pub fn new(org: impl Into<String>, env: crate::EnvName, project: impl Into<String>) -> Self {
        Self {
            org: org.into(),
            project: project.into(),
            env,
        }
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
