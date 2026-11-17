//! Positive admission facts shared by the host's application workloads.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};
use wamn_connection::connection_facts::{
    ConnectionAnswer, ConnectionCache, ConnectionFacts, Secret,
};

use anyhow::Context as _;
use moka::sync::Cache;
use wamn_authentication::authorization::{self, AuthorizationStore, HeldOperationGrants, ProjectPat};
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_catalog::application_config::ApplicationConfig;
use wamn_identity_protocol::secret::PatToken;
use wamn_identity_protocol::{PrincipalId, RoleName};
use wamn_registry::{ProjectEnvKey, ProjectId};
use wamn_sql::WamnPostgres;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fact {
    Environment,
    Grants(PrincipalId),
    Pat([u8; 32]),
    Connection(uuid::Uuid),
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
    Pat(Arc<ProjectPat>),
    Connection(Arc<ConnectionAnswer>),
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
    envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    applications: RwLock<HashMap<String, Weak<ApplicationConfig>>>,
    lifetimes: RwLock<HashMap<ProjectEnvKey, (ProjectId, Duration)>>,
    answers: Cache<Key, Answer>,
}

impl Authorization {
    pub(super) fn new(postgres: Weak<WamnPostgres>) -> Self {
        Self {
            postgres,
            envelope: None,
            lifetimes: RwLock::default(),
            applications: RwLock::default(),
            answers: Cache::builder()
                .max_capacity(MAX_AUTHORIZATION_ENTRIES)
                .expire_after(Expiry)
                .build(),
        }
    }

    pub(super) fn with_oauth_encryption(
        mut self,
        envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    ) -> Self {
        self.envelope = envelope;
        self
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
        if grants.present && grants.active && !grants.roles.is_empty() {
            self.answers.insert(key, Answer::Grants(grants));
        }
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
        token: &PatToken,
    ) -> anyhow::Result<Option<Arc<ProjectPat>>> {
        let key = self.key(project, project_key, Fact::Pat(*token.digest().as_bytes()))?;
        if let Some(Answer::Pat(principal)) = self.answers.get(&key) {
            return Ok((SystemTime::now() < principal.deadline).then_some(principal));
        }
        let principal = self.postgres()?.read_project_pat(project, token).await?;
        if let Some(principal) = &principal
            && SystemTime::now() < principal.deadline
        {
            self.answers.insert(key, Answer::Pat(Arc::clone(principal)));
        }
        Ok(principal)
    }
}

impl ConnectionCache for Authorization {
    fn bind_application(
        &self,
        workload: &str,
        application: &Arc<ApplicationConfig>,
    ) -> anyhow::Result<()> {
        self.configure_application(application)?;
        let mut applications = self
            .applications
            .write()
            .expect("application facts lock poisoned");
        applications.retain(|_, application| application.strong_count() != 0);
        applications.insert(workload.to_owned(), Arc::downgrade(application));
        Ok(())
    }

    fn get(&self, project: &ProjectEnvKey, id: &uuid::Uuid) -> Option<Arc<ConnectionAnswer>> {
        let (name, _) = self
            .lifetimes
            .read()
            .expect("authorization lifetimes lock poisoned")
            .get(project)?
            .clone();
        let key = self.key(&name, project, Fact::Connection(*id)).ok()?;
        match self.answers.get(&key)? {
            Answer::Connection(answer) => Some(answer),
            _ => None,
        }
    }
    fn insert(&self, project: &ProjectEnvKey, id: uuid::Uuid, answer: Arc<ConnectionAnswer>) {
        let configured = self
            .lifetimes
            .read()
            .expect("authorization lifetimes lock poisoned")
            .get(project)
            .cloned();
        if let Some((name, _)) = configured
            && let Ok(key) = self.key(&name, project, Fact::Connection(id))
        {
            self.answers.insert(key, Answer::Connection(answer));
        }
    }
    fn invalidate(&self, project: &ProjectEnvKey, id: &uuid::Uuid) {
        let configured = self
            .lifetimes
            .read()
            .expect("authorization lifetimes lock poisoned")
            .get(project)
            .cloned();
        if let Some((name, _)) = configured
            && let Ok(key) = self.key(&name, project, Fact::Connection(*id))
        {
            self.answers.invalidate(&key);
        }
    }
    fn credential(&self, project: &ProjectEnvKey, id: &uuid::Uuid) -> Option<Arc<Secret>> {
        self.answers.iter().find_map(|(key, _)| {
            if key.project_env_key != *project {
                return None;
            }
            let Fact::Connection(connection) = key.fact else {
                return None;
            };
            let answer = self.get(project, &connection)?;
            (answer.fact.credential_id == Some(*id))
                .then(|| answer.secret.as_ref().map(Arc::clone))
                .flatten()
        })
    }
    fn integration(
        self: Arc<Self>,
        workload: &str,
        id: &uuid::Uuid,
    ) -> anyhow::Result<Arc<ConnectionFacts>> {
        let id = id.to_string();
        let application = self
            .applications
            .read()
            .expect("application facts lock poisoned")
            .get(workload)
            .and_then(Weak::upgrade)
            .context("integration workload is not configured")?;
        application
            .components
            .iter()
            .flat_map(|component| &component.connections)
            .find(|binding| {
                binding.connection_id == id
                    && binding.runtime_type == wamn_catalog::RuntimeType::Wasm
            })
            .context("integration connection undeclared")?;
        self.facts(application)
    }
    fn project_facts(
        self: Arc<Self>,
        project: &ProjectEnvKey,
    ) -> anyhow::Result<Arc<ConnectionFacts>> {
        let application = self
            .applications
            .read()
            .expect("application facts lock poisoned")
            .values()
            .filter_map(Weak::upgrade)
            .find(|app| {
                &wamn_registry::name::project_env_key(&app.org, &app.project, &app.environment)
                    == project
            })
            .context("project is not configured")?;
        self.facts(application)
    }
    fn facts(
        self: Arc<Self>,
        application: Arc<ApplicationConfig>,
    ) -> anyhow::Result<Arc<ConnectionFacts>> {
        self.configure_application(&application)?;
        let postgres = self.postgres()?;
        let key = wamn_registry::name::project_env_key(
            &application.org,
            &application.project,
            &application.environment,
        );
        Ok(Arc::new(
            ConnectionFacts::new(
                Arc::default(),
                Arc::new(
                    wamn_connection::connection_credentials::ConnectionCredentials::for_org(
                        &application.org,
                    ),
                ),
                &postgres,
                &key,
                &application.project,
                &application.environment,
            )
            .with_oauth_encryption(self.envelope.as_ref().map(Arc::clone))
            .with_cache(self, application),
        ))
    }
}
