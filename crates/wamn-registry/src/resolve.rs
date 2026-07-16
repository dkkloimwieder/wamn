//! Resolving a control-plane identity to its placement.
//!
//! [`Registry::resolve`] is the reason the registry exists: given a [`Triple`],
//! answer *where does this database live and how is it credentialed* — the CNPG
//! cluster (derived by [`cluster_of`] from the org's placement + the env's
//! policy) + Secret reference — without any tooling parsing a provisioned name.

use crate::types::{ClusterRef, Org, Project, ProjectEnv, Registry, SecretRef, Triple, cluster_of};

/// What a [`Triple`] resolves to: the CNPG cluster that physically holds the
/// database, and a **reference** to its credential Secret (never the credential —
/// R8b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub cluster: ClusterRef,
    pub secret: SecretRef,
}

/// Why a registry lookup failed. An enum mirroring the failure modes (SR6 rule
/// 2 — never `Error(String)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// No org with this id is registered.
    UnknownOrg(String),
    /// The org exists but has no such project.
    UnknownProject { org: String, project: String },
    /// No provisioned database for this exact `(org, project, env)`.
    UnknownProjectEnv(Triple),
    /// The project-env's `env` slug names no [`EnvPolicy`](crate::EnvPolicy) — the
    /// cluster cannot be derived. A malformed registry (`validate` flags it as
    /// `unknown-env`).
    UnknownEnvPolicy(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::UnknownOrg(org) => write!(f, "unknown org {org:?}"),
            RegistryError::UnknownProject { org, project } => {
                write!(f, "unknown project {project:?} in org {org:?}")
            }
            RegistryError::UnknownProjectEnv(t) => {
                write!(f, "no provisioned database for {t}")
            }
            RegistryError::UnknownEnvPolicy(env) => {
                write!(f, "env {env:?} names no env policy")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

impl Registry {
    /// The org with id `id`, if registered.
    pub fn org(&self, id: &str) -> Option<&Org> {
        self.orgs.iter().find(|o| o.id == id)
    }

    /// The project `(org, id)`, if registered.
    pub fn project(&self, org: &str, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.org == org && p.id == id)
    }

    /// The provisioned project-env for `triple`, if registered.
    pub fn project_env(&self, triple: &Triple) -> Option<&ProjectEnv> {
        self.project_envs.iter().find(|pe| &pe.triple == triple)
    }

    /// Resolve a control-plane identity to its placement: cluster + Secret
    /// reference. The cluster is **derived** by [`cluster_of`] from the org's
    /// [`Placement`](crate::Placement) and the env's [`EnvPolicy`](crate::EnvPolicy)
    /// — a pooled org collapses onto its pool; a dedicated org owns one cluster
    /// per recovery domain. The Secret is the reference recorded on the
    /// provisioned project-env.
    ///
    /// Fails if the org is not registered ([`RegistryError::UnknownOrg`]), the
    /// project is not registered under it ([`RegistryError::UnknownProject`]), the
    /// exact `(org, project, env)` has not been provisioned
    /// ([`RegistryError::UnknownProjectEnv`]), or the env names no policy
    /// ([`RegistryError::UnknownEnvPolicy`]).
    pub fn resolve(&self, triple: &Triple) -> Result<Resolution, RegistryError> {
        let org = self
            .org(&triple.org)
            .ok_or_else(|| RegistryError::UnknownOrg(triple.org.clone()))?;
        if self.project(&triple.org, &triple.project).is_none() {
            return Err(RegistryError::UnknownProject {
                org: triple.org.clone(),
                project: triple.project.clone(),
            });
        }
        let pe = self
            .project_env(triple)
            .ok_or_else(|| RegistryError::UnknownProjectEnv(triple.clone()))?;
        let policy = self
            .env_policy(&triple.env)
            .ok_or_else(|| RegistryError::UnknownEnvPolicy(triple.env.to_string()))?;
        Ok(Resolution {
            cluster: cluster_of(org, policy),
            secret: pe.db_secret.clone(),
        })
    }
}
