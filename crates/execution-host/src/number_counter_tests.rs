//! Counter-set refusals through the host route's transaction API.
use super::{CounterSet, CounterSetError, CounterSetRefusal};
const LIVE_PRINCIPAL: &str = "5c2b7e1a-9d3f-4a6b-8c1e-2f4d6a8b0c3e";
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_sql::{ClassCredentials, WamnPostgres, WamnPostgresConfig};
const PROJECT_ENV_KEY: &str = "test--dev--numbering";
const APPLICATION: &str = "receiving";
const ADMINISTRATION_PASSWORD: &str = "numbering-administration";
const SET_OPERATION: &str = "wamn-control:number-counter/set@0.9.0";
struct Live {
    administration: sqlx::PgConnection,
    observation: sqlx::PgConnection,
    plugin: WamnPostgres,
    _lock: wamn_test_postgres::ProcessLock,
}

async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to the test database");

    client
}

impl Live {
    async fn new(database: url::Url) -> Self {
        let lock = wamn_test_postgres::lock();
        wamn_platform_identity::test_database::project_platform(database.as_str()).await;
        sqlx::raw_sql("CREATE SCHEMA wamn_run;")
            .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
            .await
            .expect("create the run-plane schema that administration revokes in");
        let generation = workload_login(
            WorkloadRoleFamily::Administration,
            WorkloadRoleIdentity::ProjectEnvironment {
                org: "org",
                project: "project",
                environment: &"dev".parse().unwrap(),
                database: database.path().trim_start_matches('/'),
            },
        )
        .expect("the administration family takes a project-environment identity");
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::test_database::login_sql(
                WorkloadRoleFamily::Administration,
                database.path().trim_start_matches('/'),
                &generation,
                ADMINISTRATION_PASSWORD,
            ),
        ))
        .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
        .await
        .expect("mint the administration generation");
        let mut url = url::Url::parse(database.as_str()).expect("parse the test url");
        url.set_username(generation.as_str())
            .expect("set the username");
        url.set_password(Some(ADMINISTRATION_PASSWORD))
            .expect("set the password");
        let administration = connect(url.as_str()).await;
        let guest_url = live_guest_url(database.as_str(), &PROJECT_ENV_KEY.parse().unwrap()).await;
        let mut live = Self {
            observation: connect(&guest_url).await,
            plugin: WamnPostgres::new(WamnPostgresConfig {
                credentials: Some(ClassCredentials::every_class(guest_url)),
                guest_pool_max_size: 8,
                platform_pool_max_size: 1,
                wait_timeout_ms: 5_000,
                statement_timeout_ms: 10_000,
            })
            .expect("the plugin builds from the guest generation's url"),
            _lock: lock,
            administration,
        };
        live.administer(&format!(
            "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ('{LIVE_PRINCIPAL}', 'user', 'caller@example.invalid');"
        ))
        .await;
        live
    }

    /// Write platform rows as the administration generation, in one
    /// transaction that binds a caller and an operation.
    async fn administer(&mut self, statements: &str) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "BEGIN; \
                 INSERT INTO platform.tx_identity (xid, principal_id, role, application, operation) \
                 VALUES (pg_current_xact_id(), '{LIVE_PRINCIPAL}'::uuid, 'admin', '{APPLICATION}', 'admin:numbering'); \
                 {statements} COMMIT;"
            ))
                .to_string(),
        ))
        .execute(&mut self.administration)
        .await
        .map(drop)
        .expect("the administration generation writes the platform rows");
    }

    async fn sequence(
        &mut self,
        application: &wamn_catalog::ApplicationName,
        name: &str,
        pattern: &str,
    ) {
        self.administer(&format!(
            "INSERT INTO platform.number_sequence (application, name, pattern) \
             VALUES ('{application}', '{name}', '{pattern}');"
        ))
        .await;
    }
}
impl Live {
    async fn set(
        &self,
        request_id: &str,
        sequence: &str,
        counter: &str,
        key: &str,
        value: i64,
    ) -> Result<i64, CounterSetError> {
        super::set_number_counter(
            &self.plugin,
            wamn_sql::DEFAULT_PROJECT,
            &PROJECT_ENV_KEY.parse().unwrap(),
            &LIVE_PRINCIPAL
                .parse::<wamn_identity_protocol::PrincipalId>()
                .unwrap(),
            &SET_OPERATION.parse().unwrap(),
            request_id,
            &CounterSet {
                application: APPLICATION.parse().unwrap(),
                sequence: sequence.to_owned(),
                counter: counter.to_owned(),
                key: key.to_owned(),
                value,
            },
        )
        .await
    }

    async fn refused(
        &self,
        sequence: &str,
        counter: &str,
        key: &str,
        value: i64,
    ) -> CounterSetRefusal {
        let request = format!("refused-{}", uuid::Uuid::new_v4());
        match self.set(&request, sequence, counter, key, value).await {
            Err(error) if error.refusal().is_some() => error.refusal().unwrap().clone(),
            other => panic!("the set of {counter} to {value} is refused: {other:?}"),
        }
    }
}

#[sqlx::test(migrations = false)]
async fn counter_set_refuses_invalid_input(pool: sqlx::PgPool) {
    let mut live = Live::new(wamn_test_postgres::url(&pool)).await;
    live.sequence(
        &APPLICATION.parse().expect("valid fixture name"),
        "purchase_order",
        "P-{seq:po}",
    )
    .await;
    assert_eq!(
        live.refused("purchase_order", "po", "", 0).await,
        CounterSetRefusal::InvalidInput { field: "value" }
    );
    assert_eq!(
        live.refused("purchase_order", "po", "facility", 9).await,
        CounterSetRefusal::InvalidInput { field: "key" }
    );
    assert_eq!(
        live.refused("purchase_order", "line", "", 9).await,
        CounterSetRefusal::CounterNotFound
    );
    assert_eq!(
        live.refused("shipment", "po", "", 9).await,
        CounterSetRefusal::SequenceNotFound
    );
    live.sequence(
        &APPLICATION.parse().expect("valid fixture name"),
        "broken",
        "{seq:",
    )
    .await;
    assert_eq!(
        live.refused("broken", "n", "", 23).await,
        CounterSetRefusal::CounterNotFound
    );
    live.sequence(
        &APPLICATION.parse().expect("valid fixture name"),
        "pick_label",
        "{seq:batch,per=batch_id}",
    )
    .await;
    assert_eq!(
        live.refused("pick_label", "batch", "abc", 7).await,
        CounterSetRefusal::AssignedPerKey
    );
}

async fn live_guest_url(admin_url: &str, project_env_key: &wamn_registry::ProjectEnvKey) -> String {
    let mut url = url::Url::parse(admin_url).unwrap();
    let database = url.path().trim_start_matches('/').to_owned();
    let login = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key,
            database: &database,
        },
    )
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            &database,
            &login,
            "counter-test",
        ),
    ))
    .execute(&mut connect(admin_url).await)
    .await
    .unwrap();
    url.set_username(login.as_str()).unwrap();
    url.set_password(Some("counter-test")).unwrap();
    url.to_string()
}

#[sqlx::test(migrations = false)]
async fn live_set_counter_is_logged_by_its_request_id(pool: sqlx::PgPool) {
    let mut live = Live::new(wamn_test_postgres::url(&pool)).await;
    live.sequence(
        &APPLICATION.parse().expect("valid fixture name"),
        "purchase_order",
        "P-{seq:po}",
    )
    .await;
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 10)
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        live.set("later", "purchase_order", "po", "", 12)
            .await
            .unwrap(),
        12
    );
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 10)
            .await
            .unwrap(),
        10
    );
    let mut transaction = wamn_test_postgres::transaction(
        &mut live.observation,
        &LIVE_PRINCIPAL
            .parse::<wamn_identity_protocol::PrincipalId>()
            .unwrap(),
        "admin:counter-observe",
    )
    .await
    .unwrap();
    let current: i64 =
        sqlx::query_scalar("SELECT value FROM platform.number_counter WHERE counter = 'po'")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(current, 12);
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 30)
            .await
            .unwrap_err()
            .refusal(),
        Some(&CounterSetRefusal::IdempotencyConflict)
    );
}
