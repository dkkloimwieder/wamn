//! Resolving a control-plane identity to its placement.
//!
//! [`Registry::resolve`] is the reason the registry exists: given a [`Triple`],
//! answer *where does this database live and how is it credentialed* — tier +
//! cluster + Secret reference — without any tooling parsing a provisioned name.

use crate::types::{ClusterRef, Org, Project, ProjectEnv, Registry, SecretRef, Tier, Triple};

/// What a [`Triple`] resolves to: its tier, the CNPG cluster that physically
/// holds the database, and a **reference** to its credential Secret (never the
/// credential — R8b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub tier: Tier,
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

    /// Resolve a control-plane identity to its placement: tier + cluster + Secret
    /// reference. The cluster is chosen by the env's recovery-domain
    /// [`side`](crate::Env::side) (the T2 prod/dev split); the Secret is the
    /// reference recorded on the provisioned project-env.
    ///
    /// Fails if the org is not registered ([`RegistryError::UnknownOrg`]), the
    /// project is not registered under it ([`RegistryError::UnknownProject`]), or
    /// the exact `(org, project, env)` has not been provisioned
    /// ([`RegistryError::UnknownProjectEnv`]).
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
        Ok(Resolution {
            tier: org.tier,
            cluster: org.cluster(triple.env.side()).clone(),
            secret: pe.db_secret.clone(),
        })
    }
}
