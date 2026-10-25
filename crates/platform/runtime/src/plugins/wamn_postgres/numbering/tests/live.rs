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
    CounterSet, CounterSetError, CounterSetRefusal, DATE_SQL, NO_NUMBER_ISSUED, NumberCall,
    render_number,
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

/// The keys of one call: the values the pattern names.
type Keys<'a> = &'a [(&'a str, Option<&'a str>)];
/// The keys of a bare pattern, which takes none.
const NO_KEYS: Keys<'static> = &[];

fn owned(keys: Keys<'_>) -> Vec<(String, Option<String>)> {
    keys.iter()
        .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
        .collect()
}

/// How a call leaves its operation transaction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// Commit, also after a refusal, as a guest that handles it does.
    Commit,
    Rollback,
    /// No operation transaction is open.
    Outside,
}

struct Live {
    database: wamn_test_postgres::Database,
    administration: tokio_postgres::Client,
    plugin: WamnPostgres,
    scopes: AtomicU32,
    _lock: wamn_test_postgres::ProcessLock,
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
        let database = wamn_project_state::test_database::tenant_platform();
        database
            .execute(&["CREATE SCHEMA wamn_run;"])
            .expect("create the run-plane schema that administration revokes in");
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
        keys: Keys<'_>,
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
            return render_number(&self.plugin, &scope, name.to_owned(), owned(keys), call).await;
        }
        let transaction = self
            .plugin
            .begin_operation_transaction(&scope)
            .await
            .expect("the operation transaction begins");
        self.plugin
            .bind_operation_transaction(&scope, &transaction)
            .expect("the transaction binds");
        let result = render_number(&self.plugin, &scope, name.to_owned(), owned(keys), call).await;
        self.plugin.revoke_operation_transaction(&scope);
        if finish == Finish::Commit {
            transaction.commit().await.expect("the transaction commits");
        } else {
            transaction
                .rollback()
                .await
                .expect("the transaction rolls back");
        }
        result
    }

    async fn next(&self, name: &str, keys: Keys<'_>) -> String {
        self.call(APPLICATION, name, keys, NumberCall::Next, Finish::Commit)
            .await
            .unwrap_or_else(|error| panic!("next-number {name} {keys:?}: {error:?}"))
    }

    async fn peek(&self, name: &str, keys: Keys<'_>) -> Result<String, StatementError> {
        self.call(APPLICATION, name, keys, NumberCall::Peek, Finish::Commit)
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
        sqlstate(&live.peek("purchase_order", NO_KEYS).await.unwrap_err()),
        NO_NUMBER_ISSUED,
        "peek before any number"
    );
    let mut numbers =
        futures_util::future::join_all((0..8).map(|_| live.next("purchase_order", NO_KEYS))).await;
    numbers.sort();
    let expected = (1..=8).map(|n| format!("P-{n:04}")).collect::<Vec<_>>();
    assert_eq!(numbers, expected);
    assert_eq!(
        live.peek("purchase_order", NO_KEYS).await.unwrap(),
        "P-0008"
    );
}

/// A number fetched in a transaction that rolls back is never consumed.
#[tokio::test]
async fn live_a_rollback_gives_the_number_back() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po,width=4}")
        .await;
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-0001");
    let rolled_back = live
        .call(
            APPLICATION,
            "purchase_order",
            NO_KEYS,
            NumberCall::Next,
            Finish::Rollback,
        )
        .await
        .unwrap();
    assert_eq!(rolled_back, "P-0002");
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-0002");
    assert_eq!(
        live.peek("purchase_order", NO_KEYS).await.unwrap(),
        "P-0002"
    );
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
    let day = &[("facility", Some("DAY"))];
    let cin = &[("facility", Some("CIN"))];
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
            &[("facility", None)],
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
    let abc = &[("batch_id", Some("abc"))];
    let mut labels =
        futures_util::future::join_all((0..6).map(|_| live.next("pick_label", abc))).await;
    labels.sort();
    let expected = (1..=6).map(|n| format!("B0001-{n:03}")).collect::<Vec<_>>();
    assert_eq!(labels, expected);
    assert_eq!(
        live.next("pick_label", &[("batch_id", Some("def"))]).await,
        "B0002-001"
    );
    assert_eq!(live.next("pick_label", abc).await, "B0001-007");
    assert_eq!(live.peek("pick_label", abc).await.unwrap(), "B0001-007");
    let unseen = live
        .peek("pick_label", &[("batch_id", Some("ghi"))])
        .await
        .unwrap_err();
    assert_eq!(
        sqlstate(&unseen),
        NO_NUMBER_ISSUED,
        "peek of a key with no number"
    );
}

/// A sequence is not tied to a record: a caller with no record takes a
/// number of a bare pattern with no keys, and a number of a keyed pattern
/// with a key map of the values the pattern names. A key the pattern does not
/// name is not read, and a key given twice is refused.
#[tokio::test]
async fn live_a_caller_numbers_with_no_record_and_with_a_key_map() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "label", "L-{seq:n}").await;
    live.sequence(
        APPLICATION,
        "dock_label",
        "{field:dock}-{seq:n,reset=dock,width=2}",
    )
    .await;
    assert_eq!(live.next("label", NO_KEYS).await, "L-1");
    assert_eq!(live.next("label", NO_KEYS).await, "L-2");
    assert_eq!(live.peek("label", NO_KEYS).await.unwrap(), "L-2");
    assert_eq!(
        live.next("dock_label", &[("dock", Some("D1"))]).await,
        "D1-01"
    );
    assert_eq!(
        live.next("dock_label", &[("dock", Some("D1")), ("shift", Some("A"))])
            .await,
        "D1-02",
        "a key the pattern does not name is not read"
    );
    assert_eq!(
        live.next("dock_label", &[("dock", Some("D2"))]).await,
        "D2-01"
    );
    for keys in [NO_KEYS, &[("dock", Some("D1")), ("dock", Some("D2"))]] {
        let refused = live
            .call(
                APPLICATION,
                "dock_label",
                keys,
                NumberCall::Next,
                Finish::Rollback,
            )
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&refused), "22023", "{keys:?}");
    }
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
            &[("batch_id", Some("abc"))],
            NumberCall::Next,
            Finish::Rollback,
        )
        .await
        .unwrap();
    assert_eq!(rolled_back, "B1");
    assert_eq!(live.next("batch", &[("batch_id", Some("def"))]).await, "B1");
    assert_eq!(live.next("batch", &[("batch_id", Some("abc"))]).await, "B2");
}

/// A sequence of another application is refused by the application in host
/// state, never by the name: the same call under the owning application
/// renders.
#[tokio::test]
async fn live_a_sequence_of_another_application_is_refused() {
    let live = Live::new().await;
    live.sequence(OTHER_APPLICATION, "shipment", "S-{seq:n}")
        .await;
    for call in [NumberCall::Next, NumberCall::Peek] {
        let refused = live
            .call(APPLICATION, "shipment", NO_KEYS, call, Finish::Rollback)
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&refused), "42704", "{call:?}");
    }
    let owned = live
        .call(
            OTHER_APPLICATION,
            "shipment",
            NO_KEYS,
            NumberCall::Next,
            Finish::Commit,
        )
        .await
        .unwrap();
    assert_eq!(owned, "S-1");
}

/// The plugin reads the application from host state, not from the
/// `app.application` claim: a transaction whose claim is rewritten to another
/// application still renders its own sequences and reaches none of the other
/// application's (wamn-9e4ks).
#[tokio::test]
async fn live_a_rewritten_application_claim_reaches_no_other_sequence() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    live.sequence(OTHER_APPLICATION, "shipment", "S-{seq:n}")
        .await;
    let scope = "numbering-rewritten-claim";
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
    let transaction = live
        .plugin
        .begin_operation_transaction(scope)
        .await
        .expect("the operation transaction begins");
    live.plugin
        .bind_operation_transaction(scope, &transaction)
        .expect("the transaction binds");
    transaction
        .with_host_connection(|connection| async move {
            let result = connection
                .connection()
                .batch_execute(&format!(
                    "SELECT set_config('app.application', '{OTHER_APPLICATION}', true)"
                ))
                .await;
            (connection, result)
        })
        .await
        .expect("the claim is rewritten");
    let render = |name: &str| {
        render_number(
            &live.plugin,
            scope,
            name.to_owned(),
            Vec::new(),
            NumberCall::Next,
        )
    };
    let refused = render("shipment").await.unwrap_err();
    assert_eq!(sqlstate(&refused), "42704", "{refused:?}");
    assert_eq!(render("purchase_order").await.unwrap(), "P-1");
    live.plugin.revoke_operation_transaction(scope);
    transaction
        .rollback()
        .await
        .expect("the transaction rolls back");
}

/// An operation transaction whose host state names no application renders
/// nothing: both calls are refused and no counter moves.
#[tokio::test]
async fn live_a_call_with_no_application_in_host_state_is_refused() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    let scope = "numbering-no-application";
    live.plugin
        .bind_session_claims(
            scope,
            &SessionClaims {
                tenant: TENANT.to_owned(),
                user_id: Some(LIVE_PRINCIPAL.to_owned()),
                ..SessionClaims::default()
            },
        )
        .await
        .expect("the claims bind");
    let transaction = live
        .plugin
        .begin_operation_transaction(scope)
        .await
        .expect("the operation transaction begins");
    live.plugin
        .bind_operation_transaction(scope, &transaction)
        .expect("the transaction binds");
    for call in [NumberCall::Next, NumberCall::Peek] {
        let refused = render_number(
            &live.plugin,
            scope,
            "purchase_order".to_owned(),
            Vec::new(),
            call,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(refused, StatementError::Postgres(PgError::PermissionDenied)),
            "{call:?}: {refused:?}"
        );
    }
    live.plugin.revoke_operation_transaction(scope);
    transaction
        .rollback()
        .await
        .expect("the transaction rolls back");
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-1");
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
            .call(
                APPLICATION,
                "purchase_order",
                NO_KEYS,
                call,
                Finish::Outside,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(refused, StatementError::Postgres(PgError::PermissionDenied)),
            "{call:?}: {refused:?}"
        );
    }
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-1");
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
            .call(
                application,
                "stamp",
                NO_KEYS,
                NumberCall::Next,
                Finish::Commit,
            )
            .await
            .unwrap();
        let after = date(offset);
        assert!(
            number == format!("{before}-1") || number == format!("{after}-1"),
            "{application}: {number} is not {before}-1 or {after}-1"
        );
    }
}

/// Every counter row of the database, in key order.
async fn counter_rows(client: &tokio_postgres::Client) -> Vec<(String, String, i64)> {
    client
        .query(
            "SELECT counter, key_value, value FROM platform.number_counter \
             ORDER BY sequence_id, counter, key_value",
            &[],
        )
        .await
        .expect("read the counter rows")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

/// An empty or null key is refused before the first `bump_counter` or
/// `assign_counter`: counter `a` comes before the keyed counters in lock
/// order, and a refused call that the guest commits leaves every counter
/// row unchanged.
#[tokio::test]
async fn live_a_refused_key_moves_no_counter() {
    let live = Live::new().await;
    live.sequence(
        APPLICATION,
        "pick_label",
        "{seq:a}-{seq:b,per=batch_id}-{field:dock}{seq:c,reset=dock}",
    )
    .await;
    let superuser = connect(live.database.url()).await;
    let good = &[("batch_id", Some("x")), ("dock", Some("D"))];
    assert_eq!(live.next("pick_label", good).await, "1-1-D1");
    let before = counter_rows(&superuser).await;
    for (keys, code) in [
        (&[("batch_id", Some("")), ("dock", Some("D"))], "22023"),
        (&[("batch_id", Some("y")), ("dock", Some(""))], "22023"),
        (&[("batch_id", None), ("dock", Some("D"))], "22004"),
        (&[("batch_id", Some("y")), ("dock", None)], "22004"),
    ] {
        let refused = live
            .call(
                APPLICATION,
                "pick_label",
                keys,
                NumberCall::Next,
                Finish::Commit,
            )
            .await
            .unwrap_err();
        assert_eq!(sqlstate(&refused), code, "{keys:?}");
        assert_eq!(counter_rows(&superuser).await, before, "{keys:?}");
    }
    assert_eq!(live.next("pick_label", good).await, "2-1-D2");
}

/// A `timezone` setting that is not a time zone is a refusal with SQLSTATE
/// `22023` for a pattern that reads the date. The refusal leaves the
/// operation transaction usable, and a pattern with no date still renders.
#[tokio::test]
async fn live_an_invalid_timezone_setting_is_refused() {
    let live = Live::new().await;
    for (application, value) in [(APPLICATION, "\"Mars/Olympus\""), (OTHER_APPLICATION, "5")] {
        live.sequence(application, "stamp", "{date:YY}-{seq:n,reset=date:YY}")
            .await;
        live.sequence(application, "label", "L-{seq:n}").await;
        live.administer(&format!(
            "INSERT INTO platform.setting (scope_type, scope_id, key, value) \
             VALUES ('application', '{application}', 'timezone', '{value}');"
        ))
        .await;
        let scope = format!("numbering-timezone-{application}");
        live.plugin
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
        let transaction = live
            .plugin
            .begin_operation_transaction(&scope)
            .await
            .expect("the operation transaction begins");
        live.plugin
            .bind_operation_transaction(&scope, &transaction)
            .expect("the transaction binds");
        let render = |name: &str| {
            render_number(
                &live.plugin,
                &scope,
                name.to_owned(),
                Vec::new(),
                NumberCall::Next,
            )
        };
        let refused = render("stamp").await.unwrap_err();
        assert_eq!(sqlstate(&refused), "22023", "{application}: {refused:?}");
        assert_eq!(render("label").await.unwrap(), "L-1", "{application}");
        live.plugin.revoke_operation_transaction(&scope);
        transaction.commit().await.expect("the transaction commits");
        let next = live
            .call(
                application,
                "label",
                NO_KEYS,
                NumberCall::Next,
                Finish::Commit,
            )
            .await
            .unwrap();
        assert_eq!(next, "L-2", "{application}: the first call committed");
    }
}

/// When a pattern holds `WW`, `YYYY` and `YY` are the ISO year, the year
/// that holds the ISO week: 2027-01-01 is in week 53 of 2026. A
/// `reset=date:YY` key in such a pattern keys on the ISO year too.
#[tokio::test]
async fn live_a_week_pattern_renders_the_iso_year() {
    let live = Live::new().await;
    let superuser = connect(live.database.url()).await;
    let boundary = DATE_SQL.replace(
        "transaction_timestamp()",
        "timestamptz '2027-01-01 12:00:00+00'",
    );
    for (iso_year, dates) in [(true, "2026|26|01|01|53|1"), (false, "2027|27|01|01|53|1")] {
        let text: String = superuser
            .query_one(&boundary, &[&"UTC", &iso_year])
            .await
            .expect("render the boundary date")
            .get(0);
        assert_eq!(text, dates, "iso year {iso_year}");
    }
    live.sequence(
        APPLICATION,
        "weekly",
        "{date:YYYY}-{date:WW}-{seq:n,reset=date:YY}",
    )
    .await;
    let now = chrono::Utc::now();
    let iso = chrono::Datelike::iso_week(&now);
    let number = live.next("weekly", NO_KEYS).await;
    assert_eq!(
        number,
        format!("{}-{:02}-1", iso.year(), iso.week()),
        "rendered at {now}"
    );
    let key: String = superuser
        .query_one(
            "SELECT key_value FROM platform.number_counter WHERE counter = 'n'",
            &[],
        )
        .await
        .expect("the reset counter row")
        .get(0);
    assert_eq!(key, format!("{:02}", iso.year() % 100));
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
    ) -> Result<i64, CounterSetError> {
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
            Err(error) if error.refusal().is_some() => error.refusal().unwrap().clone(),
            other => panic!("the set of {counter} to {value} is refused: {other:?}"),
        }
    }
}

/// An advance renders the set value next, and a set lower than the counter
/// is allowed. The set reads no record: a set that gives a number a record
/// already carries is caught by the model's `UQ` on the insert.
#[tokio::test]
async fn live_set_counter_advances_resets_and_the_model_uq_catches_a_live_number() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "PO-{seq:po,width=6}")
        .await;
    let superuser = live.records().await;
    let record = async |number: &str| {
        superuser
            .execute(
                "INSERT INTO numbered.purchase_order (number) VALUES ($1)",
                &[&number],
            )
            .await
    };
    for _ in 0..3 {
        let number = live.next("purchase_order", NO_KEYS).await;
        record(&number).await.expect("record the number");
    }
    assert_eq!(
        live.set("advance", "purchase_order", "po", "", 100)
            .await
            .unwrap(),
        100
    );
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "PO-000100");
    assert_eq!(
        live.set("reset", "purchase_order", "po", "", 2)
            .await
            .unwrap(),
        2,
        "a set below a live number is not refused"
    );
    let live_number = live.next("purchase_order", NO_KEYS).await;
    assert_eq!(live_number, "PO-000002");
    let collision = record(&live_number)
        .await
        .expect_err("the model's UQ refuses the live number");
    assert_eq!(
        collision.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
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
    let abc = &[("batch_id", Some("abc"))];
    assert_eq!(live.next("pick_label", abc).await, "B0001-001");
    assert_eq!(
        live.refused("pick_label", "batch", "abc", 7).await,
        CounterSetRefusal::AssignedPerKey
    );
    assert_eq!(
        live.set("line", "pick_label", "line", "0001", 5)
            .await
            .unwrap(),
        5
    );
    assert_eq!(live.next("pick_label", abc).await, "B0001-005");
    assert_eq!(
        live.next("pick_label", &[("batch_id", Some("def"))]).await,
        "B0002-001",
        "the per= allocator did not move"
    );
    assert_eq!(live.set("period", "yearly", "n", "99", 1).await.unwrap(), 1);
}

#[tokio::test]
async fn live_set_errors_keep_the_request_and_source() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "broken", "{seq:").await;
    let error = live
        .set("bad-pattern", "broken", "n", "", 23)
        .await
        .unwrap_err();
    assert_eq!(error.refusal(), Some(&CounterSetRefusal::CounterNotFound));
    assert_eq!(error.request_id(), "bad-pattern");
    assert_eq!(
        error.set(),
        &CounterSet {
            application: APPLICATION.to_owned(),
            sequence: "broken".to_owned(),
            counter: "n".to_owned(),
            key: String::new(),
            value: 23,
        }
    );
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<wamn_number_pattern::PatternError>()
    );

    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    live.database.execute(&[
        "ALTER TABLE platform.number_counter ADD CONSTRAINT error_source_probe CHECK (value <> 23)",
    ]).unwrap();
    let error = live
        .set("bad-write", "purchase_order", "po", "", 23)
        .await
        .unwrap_err();
    assert_eq!(error.refusal(), None);
    assert_eq!(error.request_id(), "bad-write");
    assert_eq!(error.set().sequence, "purchase_order");
    assert!(matches!(
        std::error::Error::source(&error).unwrap().downcast_ref::<PgError>(),
        Some(PgError::CheckViolation(constraint)) if constraint == "error_source_probe"
    ));
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-1");
}

/// A set waits for a concurrent next number that holds the counter row, and
/// the next number after both renders the set value.
#[tokio::test]
async fn live_set_counter_serializes_with_a_concurrent_next_number() {
    let live = Live::new().await;
    live.sequence(APPLICATION, "purchase_order", "P-{seq:po}")
        .await;
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-1");
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
        Vec::new(),
        NumberCall::Next,
    )
    .await
    .expect("the held call renders");
    assert_eq!(number, "P-2");
    let peek = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        live.peek("purchase_order", NO_KEYS),
    )
    .await
    .expect("peek returns while the other transaction still holds the counter row")
    .expect("peek reads the committed counter");
    assert_eq!(peek, "P-1", "peek reads no uncommitted number");
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
    assert_eq!(set.await.unwrap(), 50);
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-50");
}

/// A set is one write log row under the route's operation and the request
/// id, with the set value as its result; a retry answers it and changes
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
        10
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
        Some(r#"{"value":10}"#)
    );
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-10");
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 10)
            .await
            .unwrap(),
        10,
        "a retry answers the stored result"
    );
    assert_eq!(live.next("purchase_order", NO_KEYS).await, "P-11");
    assert_eq!(
        live.set("logged", "purchase_order", "po", "", 30)
            .await
            .unwrap_err()
            .refusal(),
        Some(&CounterSetRefusal::IdempotencyConflict)
    );
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
