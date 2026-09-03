//! Fresh, run-scoped PostgreSQL database for development verification.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use tokio_postgres::{Client, Config as PostgresConfig, NoTls};
use wamn_pg_core::Identifier;

use super::config::{DevConfig, POSTGRES_SYSTEM_DATABASES, VERIFICATION_DATABASE_URL};

const MAINTENANCE_DATABASE: &str = "postgres";

/// Stable category of a verification-database lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationDatabaseErrorKind {
    InvalidConfiguration,
    MaintenanceConnectionFailed,
    LeaseFailed,
    LeaseUnavailable,
    DropFailed,
    CreateFailed,
}

impl VerificationDatabaseErrorKind {
    /// Stable diagnostic code for this failure category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "dev-verification-database-invalid",
            Self::MaintenanceConnectionFailed => {
                "dev-verification-database-maintenance-connection-failed"
            }
            Self::LeaseFailed => "dev-verification-database-lease-failed",
            Self::LeaseUnavailable => "dev-verification-database-in-use",
            Self::DropFailed => "dev-verification-database-drop-failed",
            Self::CreateFailed => "dev-verification-database-create-failed",
        }
    }
}

/// Failure to create or clean up the run-scoped verification database.
pub struct VerificationDatabaseError {
    kind: VerificationDatabaseErrorKind,
    remedy: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl fmt::Debug for VerificationDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationDatabaseError")
            .field("kind", &self.kind)
            .field("key", &VERIFICATION_DATABASE_URL)
            .field("remedy", &self.remedy)
            .field("source", &self.source.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl VerificationDatabaseError {
    fn new(kind: VerificationDatabaseErrorKind, remedy: &'static str) -> Self {
        Self {
            kind,
            remedy,
            source: None,
        }
    }

    fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Stable lifecycle failure category.
    pub const fn kind(&self) -> VerificationDatabaseErrorKind {
        self.kind
    }

    /// Configuration key whose credential or database name must be corrected.
    pub const fn key(&self) -> &'static str {
        VERIFICATION_DATABASE_URL
    }

    /// Credential-free action that resolves this failure category.
    pub const fn remedy(&self) -> &'static str {
        self.remedy
    }
}

impl fmt::Display for VerificationDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at config key {:?}: {}",
            self.kind.as_str(),
            VERIFICATION_DATABASE_URL,
            self.remedy
        )
    }
}

impl Error for VerificationDatabaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

struct DatabaseSpec {
    configured_url: Box<str>,
    maintenance: PostgresConfig,
    database: Identifier,
}

impl DatabaseSpec {
    fn from_config(config: &DevConfig) -> Result<Self, VerificationDatabaseError> {
        Self::from_url(config.verification_database_url())
    }

    fn from_url(configured_url: &str) -> Result<Self, VerificationDatabaseError> {
        let mut maintenance = PostgresConfig::from_str(configured_url).map_err(|source| {
            VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::InvalidConfiguration,
                "set verification_database_url to a valid PostgreSQL URL with an explicit database",
            )
            .with_source(source)
        })?;
        let database = maintenance.get_dbname().ok_or_else(|| {
            VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::InvalidConfiguration,
                "set verification_database_url to a valid PostgreSQL URL with an explicit database",
            )
        })?;
        if POSTGRES_SYSTEM_DATABASES.contains(&database) {
            return Err(VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::InvalidConfiguration,
                "set verification_database_url to a disposable database other than postgres, template0, or template1",
            ));
        }
        let database = Identifier::new(database.to_owned()).map_err(|source| {
            VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::InvalidConfiguration,
                "set verification_database_url to a database name of at most 63 bytes without NUL",
            )
            .with_source(source)
        })?;
        maintenance.dbname(MAINTENANCE_DATABASE);
        Ok(Self {
            configured_url: configured_url.into(),
            maintenance,
            database,
        })
    }
}

trait DatabaseAuthority {
    fn try_acquire_database(
        &self,
        database: &Identifier,
    ) -> impl Future<Output = Result<bool, VerificationDatabaseError>> + Send;

    fn release_database(
        &self,
        database: &Identifier,
    ) -> impl Future<Output = Result<(), VerificationDatabaseError>> + Send;

    fn drop_database(
        &self,
        database: &Identifier,
    ) -> impl Future<Output = Result<(), VerificationDatabaseError>> + Send;

    fn create_database(
        &self,
        database: &Identifier,
    ) -> impl Future<Output = Result<(), VerificationDatabaseError>> + Send;

    fn revoke_public_connect(
        &self,
        database: &Identifier,
    ) -> impl Future<Output = Result<(), VerificationDatabaseError>> + Send;
}

struct PostgresAuthority {
    client: Client,
}

impl PostgresAuthority {
    async fn connect(config: &PostgresConfig) -> Result<Self, VerificationDatabaseError> {
        let (client, connection) = config.connect(NoTls).await.map_err(|source| {
            VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::MaintenanceConnectionFailed,
                "ensure the verification credential can connect to the postgres maintenance database",
            )
            .with_source(source)
        })?;
        tokio::spawn(async move {
            let _connection_result = connection.await;
        });
        Ok(Self { client })
    }
}

impl DatabaseAuthority for PostgresAuthority {
    async fn try_acquire_database(
        &self,
        database: &Identifier,
    ) -> Result<bool, VerificationDatabaseError> {
        self.client
            .query_one(
                "SELECT pg_catalog.pg_try_advisory_lock(pg_catalog.hashtextextended($1, 0))",
                &[&database.as_str()],
            )
            .await
            .map(|row| row.get(0))
            .map_err(|source| {
                VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::LeaseFailed,
                    "ensure the verification credential can acquire session advisory locks on the postgres maintenance database",
                )
                .with_source(source)
            })
    }

    async fn release_database(
        &self,
        database: &Identifier,
    ) -> Result<(), VerificationDatabaseError> {
        let released: bool = self
            .client
            .query_one(
                "SELECT pg_catalog.pg_advisory_unlock(pg_catalog.hashtextextended($1, 0))",
                &[&database.as_str()],
            )
            .await
            .map_err(|source| {
                VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::LeaseFailed,
                    "ensure the verification credential keeps its maintenance session through cleanup",
                )
                .with_source(source)
            })?
            .get(0);
        if !released {
            return Err(VerificationDatabaseError::new(
                VerificationDatabaseErrorKind::LeaseFailed,
                "ensure the verification credential keeps its maintenance session through cleanup",
            ));
        }
        Ok(())
    }

    async fn drop_database(&self, database: &Identifier) -> Result<(), VerificationDatabaseError> {
        self.client
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                database.quoted()
            ))
            .await
            .map_err(|source| {
                VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::DropFailed,
                    "grant the verification credential authority to drop its disposable database",
                )
                .with_source(source)
            })
    }

    async fn create_database(
        &self,
        database: &Identifier,
    ) -> Result<(), VerificationDatabaseError> {
        self.client
            .batch_execute(&format!("CREATE DATABASE {}", database.quoted()))
            .await
            .map_err(|source| {
                VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::CreateFailed,
                    "grant the verification credential CREATEDB authority for its disposable database",
                )
                .with_source(source)
            })
    }

    async fn revoke_public_connect(
        &self,
        database: &Identifier,
    ) -> Result<(), VerificationDatabaseError> {
        self.client
            .batch_execute(&format!(
                "REVOKE CONNECT ON DATABASE {} FROM PUBLIC",
                database.quoted()
            ))
            .await
            .map_err(|source| {
                VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::CreateFailed,
                    "grant the verification credential authority to revoke PUBLIC CONNECT on its disposable database",
                )
                .with_source(source)
            })
    }
}

async fn recreate(
    authority: &impl DatabaseAuthority,
    database: &Identifier,
) -> Result<(), VerificationDatabaseError> {
    authority.drop_database(database).await?;
    if let Err(error) = authority.create_database(database).await {
        let _cleanup = authority.drop_database(database).await;
        return Err(error);
    }
    if let Err(error) = authority.revoke_public_connect(database).await {
        let _cleanup = authority.drop_database(database).await;
        return Err(error);
    }
    Ok(())
}

async fn cleanup(
    authority: &impl DatabaseAuthority,
    database: &Identifier,
) -> Result<(), VerificationDatabaseError> {
    authority.drop_database(database).await
}

async fn run_with_authority<T, E, F, Fut>(
    authority: &impl DatabaseAuthority,
    spec: &DatabaseSpec,
    operation: F,
) -> Result<Result<T, E>, VerificationDatabaseError>
where
    F: FnOnce(Box<str>) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    if !authority.try_acquire_database(&spec.database).await? {
        return Err(VerificationDatabaseError::new(
            VerificationDatabaseErrorKind::LeaseUnavailable,
            "wait for the active wamn dev command using verification_database_url to finish or choose another disposable database",
        ));
    }
    if let Err(error) = recreate(authority, &spec.database).await {
        let _release = authority.release_database(&spec.database).await;
        return Err(error);
    }
    let result = operation(spec.configured_url.clone()).await;
    let cleanup_result = cleanup(authority, &spec.database).await;
    let release_result = authority.release_database(&spec.database).await;
    cleanup_result?;
    release_result?;
    Ok(result)
}

/// Run one operation against a fresh verification database and then remove it.
///
/// The callback receives the configured URL byte-for-byte. The outer result
/// reports lifecycle failures; the inner result preserves the callback's own
/// success or failure after cleanup has completed.
pub async fn run<T, E, F, Fut>(
    config: &DevConfig,
    operation: F,
) -> Result<Result<T, E>, VerificationDatabaseError>
where
    T: Send,
    E: Send,
    F: FnOnce(Box<str>) -> Fut + Send,
    Fut: Future<Output = Result<T, E>> + Send,
{
    let spec = DatabaseSpec::from_config(config)?;
    let authority = PostgresAuthority::connect(&spec.maintenance).await?;
    run_with_authority(&authority, &spec, operation).await
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::super::config::parse_config;
    use super::super::{
        DEV_STAGE_ORDER, DevInvalidation, DevInvalidationSource, DevSourceState, DevStage,
        DevStageRunner, DevWatchObserver, DevWatchOutcome, run_once_stages, run_watch_loop,
    };
    use super::*;

    const EXACT_URL: &str =
        "postgresql://verification:do-not-print@127.0.0.1:5432/wamn%2Dverification";

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event {
        Acquire(Box<str>),
        Release(Box<str>),
        Drop(Box<str>),
        Create(Box<str>),
        RevokePublicConnect(Box<str>),
        Run(Box<str>),
        Stage(DevStage),
        Outcome(DevStage),
    }

    #[derive(Clone)]
    struct RecordingAuthority {
        events: Arc<Mutex<Vec<Event>>>,
        refuse_create: bool,
        refuse_revoke_public_connect: bool,
        lease_available: bool,
    }

    struct SessionRunner {
        events: RecordingAuthority,
    }

    impl DevStageRunner for SessionRunner {
        type Error = Infallible;

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.events.record(Event::Stage(stage));
            Ok(())
        }
    }

    struct SessionInvalidations {
        events: VecDeque<DevInvalidation>,
    }

    impl DevInvalidationSource for SessionInvalidations {
        type Error = Infallible;

        async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.events.pop_front())
        }

        fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(None)
        }
    }

    struct SessionObserver {
        events: RecordingAuthority,
    }

    impl DevWatchObserver for SessionObserver {
        fn completed(&mut self, outcome: DevWatchOutcome) {
            assert!(outcome.result().is_ok());
            self.events.record(Event::Outcome(outcome.from()));
        }
    }

    impl Default for RecordingAuthority {
        fn default() -> Self {
            Self {
                events: Arc::default(),
                refuse_create: false,
                refuse_revoke_public_connect: false,
                lease_available: true,
            }
        }
    }

    impl RecordingAuthority {
        fn refusing_create() -> Self {
            Self {
                events: Arc::default(),
                refuse_create: true,
                refuse_revoke_public_connect: false,
                lease_available: true,
            }
        }

        fn refusing_revoke_public_connect() -> Self {
            Self {
                events: Arc::default(),
                refuse_create: false,
                refuse_revoke_public_connect: true,
                lease_available: true,
            }
        }

        fn contended() -> Self {
            Self {
                events: Arc::default(),
                refuse_create: false,
                refuse_revoke_public_connect: false,
                lease_available: false,
            }
        }

        fn record(&self, event: Event) {
            self.events.lock().expect("recording lock").push(event);
        }

        fn events(&self) -> Vec<Event> {
            self.events.lock().expect("recording lock").clone()
        }
    }

    impl DatabaseAuthority for RecordingAuthority {
        async fn try_acquire_database(
            &self,
            database: &Identifier,
        ) -> Result<bool, VerificationDatabaseError> {
            self.record(Event::Acquire(database.as_str().into()));
            Ok(self.lease_available)
        }

        async fn release_database(
            &self,
            database: &Identifier,
        ) -> Result<(), VerificationDatabaseError> {
            self.record(Event::Release(database.as_str().into()));
            Ok(())
        }

        async fn drop_database(
            &self,
            database: &Identifier,
        ) -> Result<(), VerificationDatabaseError> {
            self.record(Event::Drop(database.as_str().into()));
            Ok(())
        }

        async fn create_database(
            &self,
            database: &Identifier,
        ) -> Result<(), VerificationDatabaseError> {
            self.record(Event::Create(database.as_str().into()));
            if self.refuse_create {
                Err(VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::CreateFailed,
                    "grant the verification credential CREATEDB authority for its disposable database",
                ))
            } else {
                Ok(())
            }
        }

        async fn revoke_public_connect(
            &self,
            database: &Identifier,
        ) -> Result<(), VerificationDatabaseError> {
            self.record(Event::RevokePublicConnect(database.as_str().into()));
            if self.refuse_revoke_public_connect {
                Err(VerificationDatabaseError::new(
                    VerificationDatabaseErrorKind::CreateFailed,
                    "grant the verification credential authority to revoke PUBLIC CONNECT on its disposable database",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn fresh_database_surrounds_success_and_failure_in_exact_order() {
        for operation_result in [Ok("completed"), Err("stage failed")] {
            let authority = RecordingAuthority::default();
            let operation_events = authority.clone();
            let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");

            let result = run_with_authority(&authority, &spec, move |url| async move {
                operation_events.record(Event::Run(url));
                operation_result
            })
            .await
            .expect("database lifecycle succeeds");

            assert_eq!(result, operation_result);
            assert_eq!(
                authority.events(),
                vec![
                    Event::Acquire("wamn-verification".into()),
                    Event::Drop("wamn-verification".into()),
                    Event::Create("wamn-verification".into()),
                    Event::RevokePublicConnect("wamn-verification".into()),
                    Event::Run(EXACT_URL.into()),
                    Event::Drop("wamn-verification".into()),
                    Event::Release("wamn-verification".into()),
                ]
            );
        }
    }

    #[tokio::test]
    async fn one_shot_stage_loop_runs_inside_one_database_lifecycle() {
        let authority = RecordingAuthority::default();
        let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");
        let operation_events = authority.clone();

        let result = run_with_authority(&authority, &spec, move |url| async move {
            operation_events.record(Event::Run(url));
            let mut runner = SessionRunner {
                events: operation_events,
            };
            run_once_stages(DevSourceState::Clean, &mut runner).await
        })
        .await
        .expect("database lifecycle succeeds")
        .expect("one-shot stage loop succeeds");

        assert_eq!(result.completed(), DEV_STAGE_ORDER);
        let mut expected = vec![
            Event::Acquire("wamn-verification".into()),
            Event::Drop("wamn-verification".into()),
            Event::Create("wamn-verification".into()),
            Event::RevokePublicConnect("wamn-verification".into()),
            Event::Run(EXACT_URL.into()),
        ];
        expected.extend(DEV_STAGE_ORDER.map(Event::Stage));
        expected.extend([
            Event::Drop("wamn-verification".into()),
            Event::Release("wamn-verification".into()),
        ]);
        assert_eq!(authority.events(), expected);
    }

    #[tokio::test]
    async fn watch_suffixes_share_one_database_lifecycle() {
        let authority = RecordingAuthority::default();
        let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");
        let operation_events = authority.clone();

        run_with_authority(&authority, &spec, move |url| async move {
            operation_events.record(Event::Run(url));
            let mut runner = SessionRunner {
                events: operation_events.clone(),
            };
            let mut source = SessionInvalidations {
                events: VecDeque::from([
                    DevInvalidation::Rerun {
                        from: DevStage::Build,
                        source_state: DevSourceState::Clean,
                    },
                    DevInvalidation::Rerun {
                        from: DevStage::Gate,
                        source_state: DevSourceState::Clean,
                    },
                ]),
            };
            let mut observer = SessionObserver {
                events: operation_events,
            };
            run_watch_loop(&mut runner, &mut source, &mut observer).await
        })
        .await
        .expect("database lifecycle succeeds")
        .expect("watch session succeeds");

        let mut expected = vec![
            Event::Acquire("wamn-verification".into()),
            Event::Drop("wamn-verification".into()),
            Event::Create("wamn-verification".into()),
            Event::RevokePublicConnect("wamn-verification".into()),
            Event::Run(EXACT_URL.into()),
        ];
        expected.extend(DEV_STAGE_ORDER[3..].iter().copied().map(Event::Stage));
        expected.push(Event::Outcome(DevStage::Build));
        expected.extend(DEV_STAGE_ORDER[6..].iter().copied().map(Event::Stage));
        expected.extend([
            Event::Outcome(DevStage::Gate),
            Event::Drop("wamn-verification".into()),
            Event::Release("wamn-verification".into()),
        ]);
        assert_eq!(authority.events(), expected);
    }

    #[tokio::test]
    async fn cleanup_is_idempotent() {
        let authority = RecordingAuthority::default();
        let database = Identifier::new("verification").expect("valid database name");

        cleanup(&authority, &database)
            .await
            .expect("first cleanup succeeds");
        cleanup(&authority, &database)
            .await
            .expect("second cleanup succeeds");

        assert_eq!(
            authority.events(),
            vec![
                Event::Drop("verification".into()),
                Event::Drop("verification".into()),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_maintenance_authority_refuses_before_the_operation_and_cleans_up() {
        let authority = RecordingAuthority::refusing_create();
        let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");

        let error = run_with_authority(&authority, &spec, |_| async { Ok::<(), ()>(()) })
            .await
            .expect_err("missing CREATEDB authority must refuse");

        assert_eq!(error.kind(), VerificationDatabaseErrorKind::CreateFailed);
        assert_eq!(error.key(), "verification_database_url");
        assert!(error.remedy().contains("CREATEDB"));
        assert!(!error.to_string().contains("do-not-print"));
        assert_eq!(
            authority.events(),
            vec![
                Event::Acquire("wamn-verification".into()),
                Event::Drop("wamn-verification".into()),
                Event::Create("wamn-verification".into()),
                Event::Drop("wamn-verification".into()),
                Event::Release("wamn-verification".into()),
            ]
        );
    }

    #[tokio::test]
    async fn public_connect_revoke_failure_refuses_before_the_operation_and_cleans_up() {
        let authority = RecordingAuthority::refusing_revoke_public_connect();
        let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");

        let error = run_with_authority(&authority, &spec, |_| async { Ok::<(), ()>(()) })
            .await
            .expect_err("PUBLIC CONNECT must be revoked before verification begins");

        assert_eq!(error.kind(), VerificationDatabaseErrorKind::CreateFailed);
        assert!(error.remedy().contains("revoke PUBLIC CONNECT"));
        assert_eq!(
            authority.events(),
            vec![
                Event::Acquire("wamn-verification".into()),
                Event::Drop("wamn-verification".into()),
                Event::Create("wamn-verification".into()),
                Event::RevokePublicConnect("wamn-verification".into()),
                Event::Drop("wamn-verification".into()),
                Event::Release("wamn-verification".into()),
            ]
        );
    }

    #[tokio::test]
    async fn contended_database_refuses_before_destructive_work() {
        let authority = RecordingAuthority::contended();
        let spec = DatabaseSpec::from_url(EXACT_URL).expect("valid database spec");

        let error = run_with_authority(&authority, &spec, |_| async { Ok::<(), ()>(()) })
            .await
            .expect_err("held database lease must refuse");

        assert_eq!(
            error.kind(),
            VerificationDatabaseErrorKind::LeaseUnavailable
        );
        assert_eq!(error.key(), "verification_database_url");
        assert!(error.remedy().contains("active wamn dev command"));
        assert_eq!(
            authority.events(),
            vec![Event::Acquire("wamn-verification".into())]
        );
    }

    #[test]
    fn invalid_maintenance_target_names_key_and_secret_free_remedy() {
        let error = match DatabaseSpec::from_url(
            "postgresql://verification:do-not-print@127.0.0.1:5432/postgres",
        ) {
            Ok(_) => panic!("maintenance database cannot be disposable"),
            Err(error) => error,
        };

        assert_eq!(
            error.kind(),
            VerificationDatabaseErrorKind::InvalidConfiguration
        );
        assert_eq!(error.key(), "verification_database_url");
        assert!(error.remedy().contains("set verification_database_url"));
        assert!(!error.to_string().contains("do-not-print"));
        assert!(!format!("{error:?}").contains("do-not-print"));
    }

    async fn live_client(config: PostgresConfig) -> Client {
        let (client, connection) = config.connect(NoTls).await.expect("connect to PostgreSQL");
        tokio::spawn(async move {
            let _connection_result = connection.await;
        });
        client
    }

    async fn database_exists(authority: &PostgresAuthority, database: &Identifier) -> bool {
        authority
            .client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)",
                &[&database.as_str()],
            )
            .await
            .expect("query database inventory")
            .get(0)
    }

    fn database_url(base: &str, database: &str) -> String {
        let mut url = url::Url::parse(base).expect("parse live database URL");
        url.set_path(&format!("/{database}"));
        url.set_query(None);
        url.into()
    }

    fn live_config(verification_url: &str, target_url: &str) -> DevConfig {
        let process = std::process::id();
        let document = json!({
            "verification_database_url": verification_url,
            "target_database_url": target_url,
            "system_database_url": database_url(verification_url, &format!("wamn_dev_system_{process}")),
            "identity_database_url": database_url(verification_url, &format!("wamn_dev_identity_{process}")),
            "guest_database_url": database_url(verification_url, &format!("wamn_dev_guest_{process}")),
            "executor_platform_database_url": database_url(verification_url, &format!("wamn_dev_platform_{process}")),
            "http_admitter_database_url": database_url(verification_url, &format!("wamn_dev_admitter_{process}")),
            "event_materializer_database_url": database_url(verification_url, &format!("wamn_dev_materializer_{process}")),
            "scheduler_nats_url": "nats://127.0.0.1:4222",
            "event_nats_url": "nats://127.0.0.1:4223",
            "component_artifact_base": "127.0.0.1:5000/wamn/components",
            "release_artifact_base": "127.0.0.1:5001/wamn/releases",
            "registry_auth_file": "/run/secrets/registry.json",
            "insecure_registry": true,
            "gate_url": "http://127.0.0.1:8080/authoring",
            "gate_bearer_token": "live-test-token",
            "route_host": "receiving.localhost",
            "flow_http_workload_image": "127.0.0.1:5002/wamn/flow-http:dev",
            "package_sources": [],
            "effective_release_id": 1,
            "tenant": "00000000-0000-0000-0000-000000000001",
            "catalog": "default",
            "environment": "receiving-dev",
            "org": "acme",
            "project": "receiving",
            "schema": "receiving",
            "host_group": "wamn-dev-receiving",
            "host_name": "wamn-dev-receiving-1",
            "runner": "wamn-dev-receiving-1",
            "host_binary": "/opt/wamn/bin/wamn-host",
            "wasmtime_cache_dir": "/tmp/wamn-dev-cache",
        });
        parse_config(&serde_json::to_vec(&document).expect("serialize live config"))
            .expect("parse live config")
    }

    struct LiveStageRunner {
        database_url: Box<str>,
        invoked: Vec<DevStage>,
        inspect_fresh: bool,
        fail_at: Option<DevStage>,
    }

    impl DevStageRunner for LiveStageRunner {
        type Error = io::Error;

        async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
            self.invoked.push(stage);
            if self.inspect_fresh {
                self.inspect_fresh = false;
                let client = live_client(
                    PostgresConfig::from_str(&self.database_url)
                        .expect("parse exact verification URL"),
                )
                .await;
                let stale: Option<String> = client
                    .query_one("SELECT pg_catalog.to_regclass('stale_evidence')::text", &[])
                    .await
                    .expect("inspect fresh verification database")
                    .get(0);
                assert_eq!(stale, None);
                let public_connect: bool = client
                    .query_one(
                        "SELECT EXISTS (\
                           SELECT 1 \
                           FROM pg_catalog.pg_database AS database \
                           CROSS JOIN LATERAL pg_catalog.aclexplode(\
                             COALESCE(database.datacl, pg_catalog.acldefault('d', database.datdba))\
                           ) AS acl \
                           WHERE database.datname = pg_catalog.current_database() \
                             AND acl.grantee = 0 \
                             AND acl.privilege_type = 'CONNECT'\
                         )",
                        &[],
                    )
                    .await
                    .expect("inspect verification database CONNECT floor")
                    .get(0);
                assert!(!public_connect);
                client
                    .batch_execute("CREATE TABLE current_run_evidence (id bigint)")
                    .await
                    .expect("write current-run evidence");
            }
            if self.fail_at == Some(stage) {
                return Err(io::Error::other("intentional stage failure"));
            }
            Ok(())
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL through WAMN_DEV_VERIFICATION_PG_URL"]
    async fn disposable_postgres_proves_freshness_cleanup_and_confinement() {
        let url = std::env::var("WAMN_DEV_VERIFICATION_PG_URL")
            .expect("WAMN_DEV_VERIFICATION_PG_URL must name a disposable database");
        let spec = DatabaseSpec::from_url(&url).expect("valid disposable database URL");
        let authority = PostgresAuthority::connect(&spec.maintenance)
            .await
            .expect("connect with maintenance authority");
        let protected = Identifier::new(format!("wamn_dev_protected_{}", std::process::id()))
            .expect("valid protected database name");
        let protected_url = database_url(&url, protected.as_str());
        let config = live_config(&url, &protected_url);

        cleanup(&authority, &spec.database)
            .await
            .expect("remove prior verification database");
        cleanup(&authority, &protected)
            .await
            .expect("remove prior protected database");
        authority
            .create_database(&spec.database)
            .await
            .expect("create stale verification database");
        authority
            .create_database(&protected)
            .await
            .expect("create protected database");

        let stale = live_client(PostgresConfig::from_str(&url).expect("parse exact URL")).await;
        stale
            .batch_execute("CREATE TABLE stale_evidence (id bigint)")
            .await
            .expect("seed stale evidence");
        drop(stale);

        let protected_client =
            live_client(PostgresConfig::from_str(&protected_url).expect("parse protected URL"))
                .await;
        protected_client
            .batch_execute("CREATE TABLE protected_evidence (id bigint)")
            .await
            .expect("seed protected evidence");

        let mut normal_runner = LiveStageRunner {
            database_url: url.clone().into(),
            invoked: Vec::new(),
            inspect_fresh: true,
            fail_at: None,
        };
        let normal = super::super::run_once(&config, DevSourceState::Clean, &mut normal_runner)
            .await
            .expect("normal lifecycle succeeds")
            .expect("normal stage loop succeeds");
        assert_eq!(normal.completed(), DEV_STAGE_ORDER);
        assert_eq!(normal_runner.invoked, DEV_STAGE_ORDER);
        assert!(!database_exists(&authority, &spec.database).await);
        protected_client
            .query_one("SELECT count(*) FROM protected_evidence", &[])
            .await
            .expect("protected database remains unchanged");

        let mut failing_runner = LiveStageRunner {
            database_url: url.clone().into(),
            invoked: Vec::new(),
            inspect_fresh: false,
            fail_at: Some(DevStage::Gate),
        };
        let failed = super::super::run_once(&config, DevSourceState::Clean, &mut failing_runner)
            .await
            .expect("failing operation still cleans up")
            .expect_err("intentional stage failure surfaces");
        assert_eq!(failed.stage(), DevStage::Gate);
        assert!(!database_exists(&authority, &spec.database).await);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_config = config.clone();
        let first = tokio::spawn(async move {
            run(&first_config, |_| async move {
                started_tx.send(()).expect("signal acquired live lease");
                release_rx.await.expect("release first live run");
                Ok::<_, Infallible>(())
            })
            .await
        });
        started_rx.await.expect("first live run acquired its lease");
        let contending_called = Arc::new(AtomicBool::new(false));
        let called = contending_called.clone();
        let contention = run(&config, move |_| async move {
            called.store(true, Ordering::SeqCst);
            Ok::<_, Infallible>(())
        })
        .await
        .expect_err("concurrent run must refuse before destructive work");
        assert_eq!(
            contention.kind(),
            VerificationDatabaseErrorKind::LeaseUnavailable
        );
        assert!(!contending_called.load(Ordering::SeqCst));
        release_tx.send(()).expect("release first live run");
        first
            .await
            .expect("join first live run")
            .expect("first lifecycle succeeds")
            .expect("first operation succeeds");
        assert!(!database_exists(&authority, &spec.database).await);

        cleanup(&authority, &spec.database)
            .await
            .expect("first repeated cleanup succeeds");
        cleanup(&authority, &spec.database)
            .await
            .expect("second repeated cleanup succeeds");
        drop(protected_client);
        cleanup(&authority, &protected)
            .await
            .expect("remove protected fixture database");
    }
}
