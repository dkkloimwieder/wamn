//! Positive admission facts shared by the host's application workloads.

use std::collections::HashMap;
use std::sync::{RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use moka::sync::Cache;
use wamn_authentication::authorization::{self, AuthorizationStore, HeldOperationGrants};
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_catalog::application_config::ApplicationConfig;
use wamn_identity::AuthenticatedPrincipal;
use wamn_identity_protocol::secret::PatToken;
use wamn_identity_protocol::{PrincipalId, RoleName};
use wamn_registry::{ProjectEnvKey, ProjectId};
use wamn_sql::WamnPostgres;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fact {
    Environment,
    Grants(PrincipalId),
    Pat([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    project_env_key: ProjectEnvKey,
    lifetime: Duration,
    fact: Fact,
}

#[derive(Debug, Clone)]
enum Answer {
    ActiveEnvironment,
    Grants(PrincipalGrants),
    Pat(AuthenticatedPrincipal),
}

struct Expiry;

/// Bound the host memory occupied by positive admission facts.
const MAX_AUTHORIZATION_ENTRIES: u64 = 1024;

impl moka::Expiry<Key, Answer> for Expiry {
    fn expire_after_create(&self, key: &Key, _value: &Answer, _now: Instant) -> Option<Duration> {
        Some(key.lifetime)
    }
}

/// One local cache; the project lifetime applies equally to every admitted fact.
#[derive(Debug)]
pub(super) struct Authorization {
    postgres: Weak<WamnPostgres>,
    lifetimes: RwLock<HashMap<ProjectEnvKey, (ProjectId, Duration)>>,
    answers: Cache<Key, Answer>,
}

impl Authorization {
    pub(super) fn new(postgres: Weak<WamnPostgres>) -> Self {
        Self {
            postgres,
            lifetimes: RwLock::default(),
            answers: Cache::builder()
                .max_capacity(MAX_AUTHORIZATION_ENTRIES)
                .expire_after(Expiry)
                .build(),
        }
    }

    fn postgres(&self) -> anyhow::Result<std::sync::Arc<WamnPostgres>> {
        self.postgres
            .upgrade()
            .context("application host has stopped")
    }

    fn key(&self, project: &ProjectId, key: &ProjectEnvKey, fact: Fact) -> anyhow::Result<Key> {
        let lifetimes = self
            .lifetimes
            .read()
            .expect("authorization lifetimes lock poisoned");
        let (configured_project, lifetime) = lifetimes
            .get(key)
            .context("application cache lifetime is not configured")?;
        anyhow::ensure!(
            configured_project == project,
            "project admission coordinates disagree"
        );
        Ok(Key {
            project_env_key: key.clone(),
            lifetime: *lifetime,
            fact,
        })
    }
}

#[async_trait::async_trait]
impl AuthorizationStore for Authorization {
    fn configure_application(&self, application: &ApplicationConfig) -> anyhow::Result<()> {
        let key = wamn_registry::name::project_env_key(
            &application.org,
            &application.project,
            &application.environment,
        );
        self.lifetimes
            .write()
            .expect("authorization lifetimes lock poisoned")
            .insert(
                key,
                (
                    application.project.clone(),
                    Duration::from_secs(u64::from(application.grant_cache_seconds)),
                ),
            );
        Ok(())
    }

    async fn environment_inactive(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
    ) -> anyhow::Result<bool> {
        let key = self.key(project, project_key, Fact::Environment)?;
        if matches!(self.answers.get(&key), Some(Answer::ActiveEnvironment)) {
            return Ok(false);
        }
        let inactive = self
            .postgres()?
            .environment_inactive(project, project_key)
            .await?;
        // Positive means admitted: an inactive environment is read again next time.
        if !inactive {
            self.answers.insert(key, Answer::ActiveEnvironment);
        }
        Ok(inactive)
    }

    async fn principal_grants(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        principal: &PrincipalId,
    ) -> anyhow::Result<PrincipalGrants> {
        let key = self.key(project, project_key, Fact::Grants(*principal))?;
        if let Some(Answer::Grants(grants)) = self.answers.get(&key) {
            return Ok(grants);
        }
        let grants = self
            .postgres()?
            .principal_grants(project, project_key, principal)
            .await?;
        if grants.present && grants.active && !grants.roles.is_empty() {
            self.answers.insert(key, Answer::Grants(grants.clone()));
        }
        Ok(grants)
    }

    async fn session_operation_permissions(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        roles: &[RoleName],
        principal: &PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let key = self.key(project, project_key, Fact::Grants(*principal))?;
        if let Some(Answer::Grants(grants)) = self.answers.get(&key)
            && let Ok(permissions) = authorization::session_grants(&grants, roles)
        {
            return Ok(permissions);
        }
        // A cached grant set can admit a session, but a missing signed role
        // must read again: refused session admissions are never retained.
        let grants = self
            .postgres()?
            .principal_grants(project, project_key, principal)
            .await?;
        let permissions = authorization::session_grants(&grants, roles)?;
        self.answers.insert(key, Answer::Grants(grants));
        Ok(permissions)
    }

    async fn user_operation_permissions(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        principal: &PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let grants = self
            .principal_grants(project, project_key, principal)
            .await?;
        Ok(authorization::held_grants(&grants, None))
    }

    async fn route_pat(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        reader: &sqlx::PgPool,
        token: &str,
    ) -> anyhow::Result<Option<AuthenticatedPrincipal>> {
        let Ok(parsed) = token.parse::<PatToken>() else {
            return Ok(None);
        };
        let key = self.key(project, project_key, Fact::Pat(*parsed.digest().as_bytes()))?;
        if let Some(Answer::Pat(principal)) = self.answers.get(&key) {
            return Ok(principal
                .pat_deadline()
                .filter(|deadline| SystemTime::now() < *deadline)
                .map(|_| principal));
        }
        let principal = authorization::route_pat(reader, token).await?;
        if let Some(principal) = &principal {
            self.answers.insert(key, Answer::Pat(principal.clone()));
        }
        Ok(principal)
    }
}
