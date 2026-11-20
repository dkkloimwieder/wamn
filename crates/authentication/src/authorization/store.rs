//! Positive admission facts shared by the host's application workloads.

use wamn_engine::application_config::ApplicationConfig;
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use moka::sync::Cache;
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_connection::connection_credentials::{
    ConnectionCredentials, CredentialReader, CredentialStore,
};
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionCache, ConnectionFacts};
use wamn_connection::database::ProjectDatabase;
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::secret::PatToken;
use wamn_postgres::WamnPostgres;
use wamn_registry::{ProjectEnvKey, ProjectId};

use super::project_reads::{
    read_environment_inactive, read_platform_grants, read_project_pat, resolve_subject,
};
use super::{AuthorizationStore, HeldOperationGrants, ProjectPat};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fact {
    Environment,
    Grants(PrincipalId),
    Subject([u8; 32]),
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
    Subject(PrincipalId),
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
pub struct Authorization {
    postgres: Arc<WamnPostgres>,
    database: Arc<dyn ProjectDatabase>,
    envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    applications: RwLock<HashMap<String, Weak<ApplicationConfig>>>,
    lifetimes: RwLock<HashMap<ProjectEnvKey, (ProjectId, Duration)>>,
    answers: Cache<Key, Answer>,
    credentials: Option<Arc<CredentialReader>>,
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Authorization")
            .finish_non_exhaustive()
    }
}

impl Authorization {
    /// Share the project's pools and the authorized connection reader.
    pub fn new(postgres: Arc<WamnPostgres>, database: Arc<dyn ProjectDatabase>) -> Self {
        Self {
            postgres,
            database,
            envelope: None,
            lifetimes: RwLock::default(),
            applications: RwLock::default(),
            credentials: None,
            answers: Cache::builder()
                .max_capacity(MAX_AUTHORIZATION_ENTRIES)
                .expire_after(Expiry)
                .build(),
        }
    }

    /// Share one credential store and cache across application workloads.
    pub fn with_credential_store(mut self, store: Box<dyn CredentialStore>) -> Self {
        self.credentials = Some(Arc::new(CredentialReader::new(store)));
        self
    }

    /// Supply the envelope used for OAuth credentials.
    pub fn with_oauth_encryption(
        mut self,
        envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    ) -> Self {
        self.envelope = envelope;
        self
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
        let inactive = read_environment_inactive(&self.postgres, project, project_key).await?;
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
        let grants = read_platform_grants(&self.postgres, project, project_key, principal).await?;
        if grants.present && grants.active && !grants.roles.is_empty() {
            self.answers.insert(key, Answer::Grants(grants.clone()));
        }
        Ok(grants)
    }

    async fn resolve_subject(
        &self,
        project: &ProjectId,
        key: &ProjectEnvKey,
        issuer: &str,
        subject: &str,
    ) -> anyhow::Result<Option<PrincipalId>> {
        let mut digest = wamn_digest::Hasher::new();
        digest.update(issuer.len().to_be_bytes());
        digest.update(issuer);
        digest.update(subject);
        let key = self.key(project, key, Fact::Subject(digest.finalize()))?;
        if let Some(Answer::Subject(principal)) = self.answers.get(&key) {
            return Ok(Some(principal));
        }
        let principal = resolve_subject(&self.postgres, project, issuer, subject).await?;
        if let Some(principal) = principal {
            self.answers.insert(key, Answer::Subject(principal));
        }
        Ok(principal)
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
        Ok(super::held_grants(&grants))
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
        let principal = read_project_pat(&self.postgres, project, token).await?;
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
        let key = wamn_registry::name::project_env_key(
            &application.org,
            &application.project,
            &application.environment,
        );
        Ok(Arc::new(
            ConnectionFacts::new(
                Arc::default(),
                Arc::new(ConnectionCredentials::for_org(
                    &application.org,
                    self.credentials.as_ref().map(Arc::clone),
                )),
                &self.database,
                &key,
                &application.project,
                &application.environment,
            )
            .with_oauth_encryption(self.envelope.as_ref().map(Arc::clone))
            .with_cache(self, application),
        ))
    }
}
