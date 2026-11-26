//! Positive admission facts shared by the host's application workloads.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use super::{HeldOperationGrants, ProjectPat, held_grants};
use anyhow::Context as _;
use moka::sync::Cache;
use wamn_app::app_config::AppConfig;
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_connection::connection_credentials::{ConnectionCredentials, CredentialReader};
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionCache, ConnectionFacts};
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::secret::PatToken;
use wamn_registry::{ProjectEnvKey, ProjectId};

use wamn_postgres::WamnPostgres;

use super::project_reads::{
    read_env_inactive, read_guest_grants, read_platform_grants, read_project_pat, resolve_subject,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fact {
    Env,
    Subject([u8; 32]),
    Pat([u8; 32]),
    Connection(uuid::Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    project_env: ProjectEnvKey,
    lifetime: Duration,
    fact: Fact,
}

#[derive(Debug, Clone)]
enum Answer {
    ActiveEnv,
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
    envelope: Option<Arc<wamn_envelope::encryption::Envelope>>,
    apps: RwLock<HashMap<String, Weak<AppConfig>>>,
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
    pub fn new(postgres: Arc<WamnPostgres>) -> Self {
        Self {
            postgres,
            envelope: None,
            lifetimes: RwLock::default(),
            apps: RwLock::default(),
            credentials: None,
            answers: Cache::builder()
                .max_capacity(MAX_AUTHORIZATION_ENTRIES)
                .expire_after(Expiry)
                .build(),
        }
    }

    /// Share one credential store and cache across application workloads.
    pub fn with_credential_store(mut self, store: CredentialReader) -> Self {
        self.credentials = Some(Arc::new(store));
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

    fn lifetime(&self, project: &ProjectId, key: &ProjectEnvKey) -> anyhow::Result<Duration> {
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
        Ok(*lifetime)
    }

    fn key(&self, project: &ProjectId, key: &ProjectEnvKey, fact: Fact) -> anyhow::Result<Key> {
        Ok(Key {
            lifetime: self.lifetime(project, key)?,
            project_env: key.clone(),
            fact,
        })
    }

    /// Register the cache lifetime for the application workload.
    pub fn configure_app(&self, app: &AppConfig) -> anyhow::Result<()> {
        let key = wamn_registry::name::project_env_key(&app.org, &app.project, &app.env);
        self.lifetimes
            .write()
            .expect("authorization lifetimes lock poisoned")
            .insert(
                key,
                (
                    app.project.clone(),
                    Duration::from_secs(u64::from(app.grant_cache_seconds)),
                ),
            );
        Ok(())
    }

    pub async fn env_inactive(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
    ) -> anyhow::Result<bool> {
        let key = self.key(project, project_key, Fact::Env)?;
        if matches!(self.answers.get(&key), Some(Answer::ActiveEnv)) {
            return Ok(false);
        }
        let inactive = read_env_inactive(&self.postgres, project, project_key).await?;
        // Positive means admitted: read an inactive mirror again next time.
        if !inactive {
            self.answers.insert(key, Answer::ActiveEnv);
        }
        Ok(inactive)
    }

    /// Read active roles and permissions on each call.
    pub async fn principal_grants(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        principal: &PrincipalId,
    ) -> anyhow::Result<PrincipalGrants> {
        self.lifetime(project, project_key)?;
        read_platform_grants(&self.postgres, project, project_key, principal).await
    }

    pub async fn guest_principal_grants(
        &self,
        project: &ProjectId,
        key: &ProjectEnvKey,
        principal: &PrincipalId,
    ) -> anyhow::Result<PrincipalGrants> {
        read_guest_grants(&self.postgres, project, key, principal).await
    }

    pub async fn workflow_role_permissions(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        role: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        self.postgres
            .executor_read(project, async |connection| {
                Ok(sqlx::query_scalar::<_, Vec<String>>(
                    "SELECT ARRAY(SELECT permission FROM platform.permission WHERE role_name = r.name)
                       FROM platform.role AS r
                      WHERE r.name = $1 AND r.active AND EXISTS (
                          SELECT 1 FROM platform.\"user\" WHERE id = $2 AND active
                      )",
                )
                .bind(role)
                .bind(uuid::Uuid::from(*principal))
                .fetch_optional(connection)
                .await?)
            })
            .await
    }

    pub async fn resolve_subject(
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

    pub async fn user_operation_permissions(
        &self,
        project: &ProjectId,
        project_key: &ProjectEnvKey,
        principal: &PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let grants = self
            .principal_grants(project, project_key, principal)
            .await?;
        Ok(held_grants(&grants))
    }

    pub async fn route_pat(
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
    fn bind_app(&self, workload: &str, app: &Arc<AppConfig>) -> anyhow::Result<()> {
        self.configure_app(app)?;
        let mut apps = self.apps.write().expect("application facts lock poisoned");
        apps.retain(|_, app| app.strong_count() != 0);
        apps.insert(workload.to_owned(), Arc::downgrade(app));
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
        let app = self
            .apps
            .read()
            .expect("application facts lock poisoned")
            .get(workload)
            .and_then(Weak::upgrade)
            .context("integration workload is not configured")?;
        app.components
            .iter()
            .flat_map(|component| &component.connections)
            .find(|binding| {
                binding.connection_id == id
                    && binding.runtime_type == wamn_connection_type::RuntimeType::Wasm
            })
            .context("integration connection undeclared")?;
        self.facts(app)
    }
    fn project_facts(
        self: Arc<Self>,
        project: &ProjectEnvKey,
    ) -> anyhow::Result<Arc<ConnectionFacts>> {
        let app = self
            .apps
            .read()
            .expect("application facts lock poisoned")
            .values()
            .filter_map(Weak::upgrade)
            .find(|app| {
                &wamn_registry::name::project_env_key(&app.org, &app.project, &app.env) == project
            })
            .context("project is not configured")?;
        self.facts(app)
    }
    fn facts(self: Arc<Self>, app: Arc<AppConfig>) -> anyhow::Result<Arc<ConnectionFacts>> {
        self.configure_app(&app)?;
        let key = wamn_registry::name::project_env_key(&app.org, &app.project, &app.env);
        Ok(Arc::new(
            ConnectionFacts::new(
                Arc::default(),
                Arc::new(ConnectionCredentials::for_org(
                    &app.org,
                    self.credentials.as_ref().map(Arc::clone),
                )),
                &self.postgres,
                &key,
                &app.project,
                &app.env,
            )
            .with_oauth_encryption(self.envelope.as_ref().map(Arc::clone))
            .with_cache(self, app),
        ))
    }
}
