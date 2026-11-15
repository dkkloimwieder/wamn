//! Org, environment, and project coordinates.

use serde::{Deserialize, Serialize};

/// An organization id — a lowercase slug. It embeds into cluster / Secret /
/// subdomain names, so it follows the platform slug discipline (see
/// the provisioning boundary, mirroring `wamn-control-provision` / wi4 / 66x).
pub type OrgId = String;

/// A project id — a lowercase slug, unique within its org.
pub type ProjectId = String;

/// A validated environment slug — the D18 generic env model. `env` is **data,
/// not a closed type**: the default set is `dev` / `prod` (rows in
/// an environment policy), with `canary` and any others addable as policies. A slug both
/// names a project-env in the [`Triple`] and resolves its an environment policy (by
/// name). A serde-transparent `String` newtype (the `OrgId`-style discipline);
/// well-formedness is validated in the provisioning boundary, not the type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Env(String);

impl Env {
    /// Wrap a slug. Validation (lowercase slug, names a known policy) is
    /// the provisioning boundary's job — this is a plain constructor.
    pub fn new(s: impl Into<String>) -> Self {
        Env(s.into())
    }

    /// The slug string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Env {
    fn from(s: &str) -> Self {
        Env(s.to_string())
    }
}

impl From<String> for Env {
    fn from(s: String) -> Self {
        Env(s)
    }
}

impl std::ops::Deref for Env {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Env {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Env {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Env {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// The control-plane identity triple `(org, env, project)` — the key every
/// subsystem speaks (registry rows, provisioning, subdomain routing, dispatcher
/// registration, promotion tooling). Tooling keys off the triple rather than
/// parsing a provisioned name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Triple {
    pub org: OrgId,
    pub env: Env,
    pub project: ProjectId,
}

impl Triple {
    pub fn new(org: impl Into<String>, env: impl Into<Env>, project: impl Into<String>) -> Self {
        Self {
            org: org.into(),
            project: project.into(),
            env: env.into(),
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
    pub env: Env,
}
