//! The auth facts a committed write changed, and their publication to the
//! memory of every host (docs/architecture/execution.md#auth-memory).
//!
//! Every function of this crate that writes a fact a host keeps takes an
//! [`AuthDrops`] and records the fact in it. Its caller publishes the list
//! with [`crate::Publisher::publish`] after the commit, which deletes the keys of
//! the facts from the NATS KV bucket [`BUCKET`]. Every host watches the
//! bucket and keeps the values as its memory. The facts are the users
//! rows, roles, permissions and user roles of an environment, its
//! status, and the PATs, logins, memberships and status of a principal.

use std::collections::BTreeSet;

use crate::kv_memory::Deletion;

/// One auth fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthChange {
    /// The user row or the user roles of `principal` in `project_env_key`.
    Principal {
        project_env_key: String,
        principal: String,
    },
    /// A role or a permission of `project_env_key`, which every principal holding the
    /// role reads, or the environment status of `project_env_key`.
    ProjectEnvKey(String),
    /// A PAT, a login, a membership or the status of `principal`. These are
    /// facts of the organization, so they change the principal in every
    /// project.
    Identity(String),
}

/// The changes of one transaction, published after its commit.
#[must_use = "publish the drops with Publisher::publish after the commit"]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthDrops(BTreeSet<AuthChange>);

impl AuthDrops {
    pub fn principal(&mut self, project_env_key: &str, principal: &str) {
        self.0.insert(AuthChange::Principal {
            project_env_key: project_env_key.to_owned(),
            principal: principal.to_owned(),
        });
    }

    pub fn project_env_key(&mut self, project_env_key: &str) {
        self.0
            .insert(AuthChange::ProjectEnvKey(project_env_key.to_owned()));
    }

    pub fn identity(&mut self, principal: &str) {
        self.0.insert(AuthChange::Identity(principal.to_owned()));
    }

    /// Add the changes of `other`, for a caller that runs several writers in
    /// one transaction.
    pub fn extend(&mut self, other: AuthDrops) {
        self.0.extend(other.0);
    }

    pub fn changes(&self) -> impl Iterator<Item = &AuthChange> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The NATS KV bucket of the auth memory, on the scheduler NATS.
pub const BUCKET: &str = "wamn-auth";

/// The key of the grants of `principal` in the environment named `project_env_key`.
/// This shared cache key stays targeted after the table project columns leave.
pub fn principal_key(project_env_key: &str, principal: &str) -> String {
    wamn_registry::name::nats_subject(&[project_env_key, principal])
}

/// The key of the environment status of `project_env_key`. A principal id is a UUID,
/// so it is never `environment`.
pub fn project_env_key(project_env_key: &str) -> String {
    wamn_registry::name::nats_subject(&[project_env_key, "environment"])
}

/// The first token of the key of an identity fact. A project name holds only
/// `[A-Za-z0-9_-]`, so this token never meets a project key.
pub const IDENTITY_TOKEN: &str = "=identity";

/// The key a change of an identity fact of `principal` deletes.
pub fn identity_key(principal: &str) -> String {
    wamn_registry::name::nats_subject(&[IDENTITY_TOKEN, principal])
}

/// Open the bucket. Identity makes it ([`crate::kv_memory::create_buckets`]).
pub async fn auth_bucket(
    client: &async_nats::Client,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    crate::kv_memory::open_bucket(client, BUCKET).await
}

impl crate::Changes for AuthDrops {
    fn into_deletions(self) -> (&'static str, Vec<Deletion>) {
        (
            BUCKET,
            self.0
                .into_iter()
                .map(|change| match change {
                    AuthChange::Principal {
                        project_env_key,
                        principal,
                    } => Deletion::Key(principal_key(&project_env_key, &principal)),
                    AuthChange::Identity(principal) => Deletion::Key(identity_key(&principal)),
                    AuthChange::ProjectEnvKey(project_env_key) => Deletion::Under(project_env_key),
                })
                .collect(),
        )
    }
}
