//! Live checks of the numbering invariants (docs/plan/platform-config.md,
//! Invariants).
//!
//! Each test owns a database of the test PostgreSQL server with the real
//! catalog and application schema. An administration generation, minted by
//! the production builder, writes the sequence, setting and users rows, and
//! the plugin renders as a guest generation of the tenant, a member of
//! `wamn_app`. Each test holds the process lock, because both generations are
//! cluster-wide roles.

use std::sync::atomic::{AtomicU32, Ordering};

use super::super::{
    CounterSet, CounterSetError, CounterSetRefusal, NO_NUMBER_ISSUED, NumberCall, render_number,
};
use crate::plugins::wamn_postgres::claims::tests::{LIVE_PRINCIPAL, live_guest_url};
use crate::plugins::wamn_postgres::{
    ClassCredentials, PgError, SessionClaims, StatementError, WamnPostgres, WamnPostgresConfig,
};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql as provision_sql,
    workload_generation_role,
};

const TENANT: &str = "numbering";
const APPLICATION: &str = "receiving";
const OTHER_APPLICATION: &str = "shipping";
const ADMINISTRATION_PASSWORD: &str = "numbering-administration";
/// The roles the catalog grants to, as provisioning creates them, and the
/// run-plane schema the administration surface revokes in.
const PLATFORM_ROLES_SQL: &str = "DO $roles$ DECLARE role_name text; BEGIN \
       PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
       FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author'] LOOP \
         IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
           EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                           NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
         END IF; \
       END LOOP; \
     END $roles$; \
     CREATE SCHEMA IF NOT EXISTS wamn_run;";

/// How a call leaves its operation transaction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    Commit,
    Rollback,
    /// No operation transaction is open.
    Outside,
}

struct Live {
    _lock: wamn_test_postgres::ProcessLock,
    database: wamn_test_postgres::Database,
    administration: tokio_postgres::Client,
    plugin: WamnPostgres,
    scopes: AtomicU32,
}

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .expect("connect to the test database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

impl Live {
    async fn new() -> Self {
        let lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::database();
        database
            .execute(&[
                PLATFORM_ROLES_SQL,
                wamn_catalog::CATALOG_SCHEMA_SQL,
                wamn_control_provision::APP_SCHEMA_SQL,
            ])
            .expect("install the catalog and the application schema");
        let generation = workload_generation_role(
            WorkloadRoleFamily::Administration,
            WorkloadRoleScope::ProjectEnvironment {
                org: "org",
                project: "project",
                environment: "dev",
                database: database.name(),
            },
            CredentialGeneration::A,
        )
        .expect("the administration family takes a project-environment scope");
        database
            .execute(&[&provision_sql::prepare_workload_generation_sql(
                WorkloadRoleFamily::Administration,
                database.name(),
                &generation,
                ADMINISTRATION_PASSWORD,
                "2100-01-01T00:00:00Z",
            )])
            .expect("mint the administration generation");
        let mut url = url::Url::parse(database.url()).expect("parse the test url");
        url.set_username(&generation).expect("set the username");
        url.set_password(Some(ADMINISTRATION_PASSWORD))
            .expect("set the password");
        let administration = connect(url.as_str()).await;
        let live = Self {
            plugin: WamnPostgres::new(WamnPostgresConfig {
                credentials: Some(ClassCredentials::every_class(
                    live_guest_url(database.url(), TENANT).await,
                )),
                guest_pool_max_size: 8,
                platform_pool_max_size: 1,
                wait_timeout_ms: 5_000,
                statement_timeout_ms: 10_000,
                row_limit: 100,
            })
            .expect("the plugin builds from the guest generation's url"),
            _lock: lock,
            database,
            administration,
            scopes: AtomicU32::new(0),
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
    async fn administer(&self, statements: &str) {
        self.administration
            .batch_execute(&format!(
                "BEGIN; \
                 SELECT pg_catalog.set_config('app.user_id', '{LIVE_PRINCIPAL}', true), \
                        pg_catalog.set_config('app.operation', 'admin:numbering', true); \
                 {statements} COMMIT;"
            ))
            .await
            .expect("the administration generation writes the platform rows");
    }

    async fn sequence(&self, application: &str, name: &str, pattern: &str) {
        self.administer(&format!(
            "INSERT INTO platform.number_sequence (application, name, pattern) \
             VALUES ('{application}', '{name}', '{pattern}');"
        ))
        .await;
    }

    /// One call under `application`, in a fresh operation transaction that
    /// `finish` ends, or outside one.
    async fn call(
        &self,
        application: &str,
        name: &str,
        record: &str,
        call: NumberCall,
        finish: Finish,
    ) -> Result<String, StatementError> {
        let scope = format!("numbering-{}", self.scopes.fetch_add(1, Ordering::Relaxed));
        self.plugin
            .bind_session_claims(
                &scope,
                &SessionClaims {
                    tenant: TENANT.to_owned(),
                    user_id: Some(LIVE_PRINCIPAL.to_owned()),
                    application: Some(application.to_owned()),
                    ..SessionClaims::default()
                },
            )
            .await
            .expect("the claims bind");
        if finish == Finish::Outside {
            return render_number(
                &self.plugin,
                &scope,
                name.to_owned(),
                record.to_owned(),
                call,
            )
            .await;
        }
        let transaction = self
            .plugin
            .begin_operation_transaction(&scope)
            .await
            .expect("the operation transaction begins");
        self.plugin
            .bind_operation_transaction(&scope, &transaction)
            .expect("the transaction binds");
        let result = render_number(
            &self.plugin,
            &scope,
            name.to_owned(),
            record.to_owned(),
            call,
        )
        .await;
        self.plugin.revoke_operation_transaction(&scope);
        if finish == Finish::Commit && result.is_ok() {
            transaction.commit().await.expect("the transaction commits");
        } else {
            transaction
                .rollback()
                .await
                .expect("the transaction rolls back");
        }
        result
    }

    async fn next(&self, name: &str, record: &str) -> String {
        self.call(APPLICATION, name, record, NumberCall::Next, Finish::Commit)
            .await
            .unwrap_or_else(|error| panic!("next-number {name} {record}: {error:?}"))
    }

    async fn peek(&self, name: &str, record: &str) -> Result<String, StatementError> {
        self.call(APPLICATION, name, record, NumberCall::Peek, Finish::Commit)
            .await
    }
}

fn sqlstate(error: &StatementError) -> &str {
    match error {
        StatementError::Postgres(PgError::QueryError((code, _))) => code,
        other => panic!("a query error: {other:?}"),
    }
}

/// N concurrent creates on one counter yield N consecutive values, none
/// repeated, and peek reads the last one.
#[tokio::test]
async fn live_numbers_are_gapless_and_consecutive_under_concurrent_creates() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po,width=4}")
        .await;
    assert_eq!(
        sqlstate(&live.peek("purchase_order", "{}").await.unwrap_err()),
        NO_NUMBER_ISSUED,
        "peek before any number"
    );
    let mut numbers =
        futures_util::future::join_all((0..8).map(|_| live.next("purchase_order", "{}"))).await;
    numbers.sort();
    let expected = (1..=8).map(|n| format!("P-{n:04}")).collect::<Vec<_>>();
    assert_eq!(numbers, expected);
    assert_eq!(live.peek("purchase_order", "{}").await.unwrap(), "P-0008");
}

/// A number fetched in a transaction that rolls back is never consumed.
#[tokio::test]
async fn live_a_rollback_gives_the_number_back() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po,width=4}")
        .await;
    assert_eq!(live.next("purchase_order", "{}").await, "P-0001");
    let rolled_back = live
        .call(
            APPLICATION,
            "purchase_order",
            "{}",
            NumberCall::Next,
            Finish::Rollback,
        )
        .await
        .unwrap();
    assert_eq!(rolled_back, "P-0002");
    assert_eq!(live.next("purchase_order", "{}").await, "P-0002");
    assert_eq!(live.peek("purchase_order", "{}").await.unwrap(), "P-0002");
}

/// `reset=` starts at 1 for each distinct key, and two keys interleaved count
/// on their own. A null key field is refused.
#[tokio::test]
async fn live_reset_starts_at_one_per_key() {
    let live = Live::new().await;
    live.sequence(
        APPLICATION,
        "purchase_order",
        "{field:facility}-PO-{seq:po,reset=facility,width=3}",
    )
    .await;
    let day = r#"{"facility":"DAY"}"#;
    let cin = r#"{"facility":"CIN"}"#;
    assert_eq!(live.next("purchase_order", day).await, "DAY-PO-001");
    assert_eq!(live.next("purchase_order", cin).await, "CIN-PO-001");
    assert_eq!(live.next("purchase_order", day).await, "DAY-PO-002");
    assert_eq!(live.next("purchase_order", cin).await, "CIN-PO-002");
    assert_eq!(
        live.peek("purchase_order", day).await.unwrap(),
        "DAY-PO-002"
    );
    let null = live
        .call(
            APPLICATION,
            "purchase_order",
            r#"{"facility":null}"#,
            NumberCall::Next,
            Finish::Rollback,
        )
        .await
        .unwrap_err();
    assert_eq!(sqlstate(&null), "22004", "a null key field is refused");
}

/// `per=` assigns one value per key once, under concurrency: N concurrent
/// first creates with one batch id all carry one batch number, and their
/// `reset=` lines count 1 to N. A second batch takes the next number.
#[tokio::test]
async fn live_per_assigns_once_per_key_under_concurrency() {
    let live = Live::new().await;
    live.sequence(
        APPLICATION,
        "pick_label",
        "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}",
    )
    .await;
    let abc = r#"{"batch_id":"abc"}"#;
    let mut labels =
        futures_util::future::join_all((0..6).map(|_| live.next("pick_label", abc))).await;
    labels.sort();
    let expected = (1..=6).map(|n| format!("B0001-{n:03}")).collect::<Vec<_>>();
    assert_eq!(labels, expected);
    assert_eq!(
        live.next("pick_label", r#"{"batch_id":"def"}"#).await,
        "B0002-001"
    );
    assert_eq!(live.next("pick_label", abc).await, "B0001-007");
    assert_eq!(live.peek("pick_label", abc).await.unwrap(), "B0001-007");
    let unseen = live
        .peek("pick_label", r#"{"batch_id":"ghi"}"#)
        .await
        .unwrap_err();
    assert_eq!(
        sqlstate(&unseen),
        NO_NUMBER_ISSUED,
        "peek of a key with no number"
    );
}

/// A first `per=` sight that rolls back gives the value back: the next key
/// takes it.
#[tokio::test]
async fn live_a_rolled_back_per_assignment_is_not_consumed() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "batch", "B{seq:batch,per=batch_id}")
        .await;
    let rolled_back = live
        .call(
            APPLICATION,
            "batch",
            r#"{"batch_id":"abc"}"#,
            NumberCall::Next,
            Finish::Rollback,
        )
        .await
        .unwrap();
    assert_eq!(rolled_back, "B1");
    assert_eq!(live.next("batch", r#"{"batch_id":"def"}"#).await, "B1");
    assert_eq!(live.next("batch", r#"{"batch_id":"abc"}"#).await, "B2");
}

/// A sequence of another application is refused by the claim, never by the
/// name: the same call under the owning application renders.
#[tokio::test]
async fn live_a_sequence_of_another_application_is_refused() {
    let live = Live::new().await;
    live.sequence(OTHER_APPLICATION, "shipment", "S-{seq:n}")
        .await;
    for call in [NumberCall::Next, NumberCall::Peek] {
        let refused = live
            .call(APPLICATION, "shipment", "{}", call, Finish::Rollback)
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&refused), "42704", "{call:?}");
    }
    let owned = live
        .call(
            OTHER_APPLICATION,
            "shipment",
            "{}",
            NumberCall::Next,
            Finish::Commit,
        )
        .await
        .unwrap();
    assert_eq!(owned, "S-1");
}

/// Outside an open operation transaction both calls are refused and no
/// counter moves.
#[tokio::test]
async fn live_a_call_outside_an_operation_transaction_is_refused() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    for call in [NumberCall::Next, NumberCall::Peek] {
        let refused = live
            .call(APPLICATION, "purchase_order", "{}", call, Finish::Outside)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, StatementError::Postgres(PgError::PermissionDenied)),
            "{call:?}: {refused:?}"
        );
    }
    assert_eq!(live.next("purchase_order", "{}").await, "P-1");
}

/// `{date:FMT}` renders the transaction date in the `timezone` setting of the
/// application, else in UTC, and `reset=date:` keys on it.
#[tokio::test]
async fn live_the_date_uses_the_application_timezone_else_utc() {
    let live = Live::new().await;
    let pattern = "{date:YYYY}{date:MM}{date:DD}-{seq:n,reset=date:YYYY}";
    live.sequence(APPLICATION, "stamp", pattern).await;
    live.sequence(OTHER_APPLICATION, "stamp", pattern).await;
    // POSIX sign: Etc/GMT-14 is fourteen hours ahead of UTC.
    live.administer(&format!(
        "INSERT INTO platform.setting (scope_type, scope_id, key, value) \
         VALUES ('application', '{APPLICATION}', 'timezone', '\"Etc/GMT-14\"');"
    ))
    .await;
    let date = |offset_hours: i64| {
        (chrono::Utc::now() + chrono::Duration::hours(offset_hours))
            .format("%Y%m%d")
            .to_string()
    };
    for (application, offset) in [(APPLICATION, 14), (OTHER_APPLICATION, 0)] {
        let before = date(offset);
        let number = live
            .call(application, "stamp", "{}", NumberCall::Next, Finish::Commit)
            .await
            .unwrap();
        let after = date(offset);
        assert!(
            number == format!("{before}-1") || number == format!("{after}-1"),
            "{application}: {number} is not {before}-1 or {after}-1"
        );
    }
}

/// The operation id of the `number_counter.set` host route, as the host
/// passes it.
const SET_OPERATION: &str = "wamn-control:number-counter/set@0.9.0";

impl Live {
    /// A table of the application's records whose unique `number` holds
    /// rendered numbers, readable by `wamn_app`.
    async fn records(&self) -> tokio_postgres::Client {
        let superuser = connect(self.database.url()).await;
        superuser
            .batch_execute(
                "CREATE SCHEMA numbered; \
                 CREATE TABLE numbered.purchase_order (id serial PRIMARY KEY, number text UNIQUE); \
                 GRANT USAGE ON SCHEMA numbered TO wamn_app; \
                 GRANT SELECT ON numbered.purchase_order TO wamn_app;",
            )
            .await
            .expect("create the records table");
        superuser
    }

    async fn set(
        &self,
        request_id: &str,
        sequence: &str,
        counter: &str,
        key: &str,
        value: i64,
    ) -> Result<String, CounterSetError> {
        self.plugin
            .set_number_counter(
                crate::plugins::wamn_postgres::DEFAULT_PROJECT,
                TENANT,
                LIVE_PRINCIPAL,
                SET_OPERATION,
                request_id,
                &CounterSet {
                    application: APPLICATION.to_owned(),
                    sequence: sequence.to_owned(),
                    counter: counter.to_owned(),
                    key: key.to_owned(),
                    value,
                },
                &["numbered".to_owned()],
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
        let request = format!("refused-{}", self.scopes.fetch_add(1, Ordering::Relaxed));
        match self.set(&request, sequence, counter, key, value).await {
            Err(CounterSetError::Refused(refusal)) => refusal,
            other => panic!("the set of {counter} to {value} is refused: {other:?}"),
        }
    }
}

/// An advance renders the set value next; a set lower than the counter is
/// allowed when no record carries the number it gives, and refused when one
/// does, which leaves the counter as it was.
#[tokio::test]
async fn live_set_counter_advances_resets_and_refuses_a_live_number() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "PO-{seq:po,width=6}")
        .await;
    let superuser = live.records().await;
    for _ in 0..3 {
        let number = live.next("purchase_order", "{}").await;
        superuser
            .execute(
                "INSERT INTO numbered.purchase_order (number) VALUES ($1)",
                &[&number],
            )
            .await
            .expect("record the number");
    }
    assert_eq!(
        live.set("advance", "purchase_order", "po", "", 100)
            .await
            .unwrap(),
        "PO-000100"
    );
    assert_eq!(live.next("purchase_order", "{}").await, "PO-000100");
    // PO-000100 was issued but no record carries it, and PO-000004 is free.
    assert_eq!(
        live.set("reset", "purchase_order", "po", "", 4)
            .await
            .unwrap(),
        "PO-000004"
    );
    assert_eq!(
        live.refused("purchase_order", "po", "", 2).await,
        CounterSetRefusal::NumberLive {
            number: "PO-000002".to_owned()
        }
    );
    assert_eq!(live.next("purchase_order", "{}").await, "PO-000004");
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
}

/// A `reset=` counter is set for one key, and a set of 1 on a new period key
/// is allowed; a `per=` counter is refused, and no mapping row changes.
#[tokio::test]
async fn live_set_counter_takes_a_reset_key_and_refuses_a_per_counter() {
    let live = Live::new().await;
    live.sequence(
        APPLICATION,
        "pick_label",
        "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}",
    )
    .await;
    live.sequence(APPLICATION, "yearly", "{date:YY}-{seq:n,reset=date:YY}")
        .await;
    let _superuser = live.records().await;
    let abc = r#"{"batch_id":"abc"}"#;
    assert_eq!(live.next("pick_label", abc).await, "B0001-001");
    assert_eq!(
        live.refused("pick_label", "batch", "abc", 7).await,
        CounterSetRefusal::AssignedPerKey
    );
    assert_eq!(
        live.set("line", "pick_label", "line", "0001", 5)
            .await
            .unwrap(),
        "B0001-005"
    );
    assert_eq!(live.next("pick_label", abc).await, "B0001-005");
    assert_eq!(
        live.next("pick_label", r#"{"batch_id":"def"}"#).await,
        "B0002-001",
        "the per= allocator did not move"
    );
    assert_eq!(
        live.set("period", "yearly", "n", "99", 1).await.unwrap(),
        "99-1"
    );
}

/// A set waits for a concurrent next number that holds the counter row, and
/// the next number after both renders the set value.
#[tokio::test]
async fn live_set_counter_serializes_with_a_concurrent_next_number() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    let _superuser = live.records().await;
    assert_eq!(live.next("purchase_order", "{}").await, "P-1");
    let scope = "numbering-held";
    live.plugin
        .bind_session_claims(
            scope,
            &SessionClaims {
                tenant: TENANT.to_owned(),
                user_id: Some(LIVE_PRINCIPAL.to_owned()),
                application: Some(APPLICATION.to_owned()),
                ..SessionClaims::default()
            },
        )
        .await
        .expect("the claims bind");
    let held = live
        .plugin
        .begin_operation_transaction(scope)
        .await
        .expect("the operation transaction begins");
    live.plugin
        .bind_operation_transaction(scope, &held)
        .expect("the transaction binds");
    let number = render_number(
        &live.plugin,
        scope,
        "purchase_order".to_owned(),
        "{}".to_owned(),
        NumberCall::Next,
    )
    .await
    .expect("the held call renders");
    assert_eq!(number, "P-2");
    let set = live.set("serialized", "purchase_order", "po", "", 50);
    tokio::pin!(set);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut set)
            .await
            .is_err(),
        "the set waits for the counter row"
    );
    live.plugin.revoke_operation_transaction(scope);
    held.commit().await.expect("the held call commits");
    assert_eq!(set.await.unwrap(), "P-50");
    assert_eq!(live.next("purchase_order", "{}").await, "P-50");
}

/// A set is one write log row under the route's operation and the request
/// id, with the next number as its result; a retry answers it and changes
/// nothing, a different set under the same id is refused, and the counter
/// row names the caller.
#[tokio::test]
async fn live_set_counter_is_logged_by_its_request_id() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    let superuser = live.records().await;
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 10)
            .await
            .unwrap(),
        "P-10"
    );
    let row = superuser
        .query_one(
            "SELECT result FROM platform.write_log \
             WHERE operation = 'wamn-control:number-counter/set' AND idempotency_key = 'logged'",
            &[],
        )
        .await
        .expect("the set has its write log row");
    assert_eq!(
        row.get::<_, Option<String>>(0).as_deref(),
        Some(r#"{"next_number":"P-10"}"#)
    );
    assert_eq!(live.next("purchase_order", "{}").await, "P-10");
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 10)
            .await
            .unwrap(),
        "P-10",
        "a retry answers the stored result"
    );
    assert_eq!(live.next("purchase_order", "{}").await, "P-11");
    assert!(matches!(
        live.set("logged", "purchase_order", "po", "", 30).await,
        Err(CounterSetError::Refused(
            CounterSetRefusal::IdempotencyConflict
        ))
    ));
    let updated_by: String = superuser
        .query_one(
            "SELECT updated_by::text FROM platform.number_counter WHERE counter = 'po'",
            &[],
        )
        .await
        .expect("the counter row")
        .get(0);
    assert_eq!(updated_by, LIVE_PRINCIPAL);
}
