//! Live tests of standard queue leases, leases, and exhausted-run reaping.
//!
//! The fixture includes an effect attempt to show that leases and reaping
//! ignore it. FIFO order, lease recovery, crash accounting,
//! and release pins retain their standard behavior.

use sqlx::{Connection as _, Row as _, Statement as _};
use std::collections::BTreeSet;

use serde_json::{Value, json};
use wamn_run_state::queue::select_production_lease_sql;
use wamn_runtime::plugins::wamn_postgres::{
    ProductionLeaseErrorType, ProductionLeaseResult, ProductionReapResult,
};

mod common;

use common::{
    COMPONENT, EMPTY_HASH, ENVIRONMENT, PACKAGE_ID, POD_MANIFEST_DIGEST, PROJECT_ENV_KEY,
    ROLLED_COMPONENT, SCHEMA, WIRING_ID, WIRING_VERSION, assert_terminal_status_dequeued, connect,
    expire_effect_run, install_fixture, queue_attempts, quote_literal, ready_run, release_record,
    seed_exhausted_run, seed_run, teardown,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_lease_live() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::project();
    let url = database.url();
    let mut fixture = install_fixture(url).await?;
    let admin = &mut fixture.admin;
    let plugin = &fixture.plugin;
    let release_package_ids = [PACKAGE_ID.to_owned(), "cat_overlay".to_owned()];

    // Run id breaks equal
    // timestamps across package boundaries. An earlier foreign-package row does
    // not enter the mounted release's one SQL turn.
    seed_run(admin, "fifo-foreign", "cat_foreign").await?;
    for (run_id, package_id) in [
        ("fifo-c", PACKAGE_ID),
        ("fifo-b", "cat_overlay"),
        ("fifo-a", PACKAGE_ID),
    ] {
        seed_run(admin, run_id, package_id).await?;
    }
    let mut fifo = Vec::new();
    for _ in 0..3 {
        let result = plugin
            .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?;
        let ProductionLeaseResult::Ready {
            run_id, package_id, ..
        } = result
        else {
            panic!("expected a release-package FIFO row, got {result:?}");
        };
        fifo.push((run_id, package_id));
    }
    assert_eq!(
        fifo,
        [
            ("fifo-a".into(), PACKAGE_ID.into()),
            ("fifo-b".into(), "cat_overlay".into()),
            ("fifo-c".into(), PACKAGE_ID.into()),
        ]
    );

    // A second leaseholder skips the exact FIFO head while the first leaseholder holds
    // its production row locks, then the rolled-back head remains leasable.
    seed_run(admin, "double-a", "cat_main").await?;
    seed_run(admin, "double-b", "cat_main").await?;
    let mut first_leaseholder = connect(url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "BEGIN; SET LOCAL search_path TO {SCHEMA}, pg_catalog"
    )))
    .execute(&mut first_leaseholder)
    .await
    .map(drop)?;
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(&PROJECT_ENV_KEY)
        .execute(&mut first_leaseholder)
        .await
        .map(|result| result.rows_affected())?;
    let locked = sqlx::query(sqlx::AssertSqlSafe(
        (&select_production_lease_sql()).to_string(),
    ))
    .bind(&release_package_ids.as_slice())
    .bind(&ENVIRONMENT)
    .bind(&Some(POD_MANIFEST_DIGEST))
    .fetch_one(&mut first_leaseholder)
    .await?;
    assert_eq!(locked.get::<String, _>(0), "double-a");
    let skipped = ready_run(
        plugin
            .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
    );
    assert_eq!(skipped, "double-b");
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut first_leaseholder)
        .await
        .map(drop)?;
    let released = ready_run(
        plugin
            .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
    );
    let acquired = BTreeSet::from([skipped, released]);
    assert_eq!(
        acquired,
        BTreeSet::from(["double-a".into(), "double-b".into()])
    );

    // Exhaustion records a failure and removes the queue row.
    seed_exhausted_run(admin, "janitor").await?;
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "janitor".into()
        }
    );
    assert_terminal_status_dequeued(admin, "janitor", "infrastructure-failure").await?;

    // ---- a refused grant must not starve its own janitor (wamn-0h0g.15.69) --
    //
    // `attempts` used to ride the same statement that grants the lease, so any
    // refusal of that statement rolled the increment back with it: the run
    // could never reach `max_attempts`, the janitor could never reap it, and
    // nothing locked it after rollback — one run head-of-line-blocked the
    // project forever. The advance now runs before the grant's subtransaction,
    // so a refusal still counts as crash evidence. A probe trigger stands in
    // for any database guard that can refuse the grant.
    seed_run(admin, "grant-refused", "cat_main").await?;
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=2, max_attempts=3 \
                  WHERE project_env_key=$1 AND run_id='grant-refused'"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE FUNCTION {SCHEMA}.refuse_probed_grant() \
               RETURNS trigger LANGUAGE plpgsql AS $probe$ \
               BEGIN \
                 IF NEW.run_id = 'grant-refused' THEN \
                   RAISE EXCEPTION USING ERRCODE = '55000', \
                     MESSAGE = 'probe-grant-refused'; \
                 END IF; \
                 RETURN NEW; \
               END $probe$; \
             CREATE TRIGGER refuse_probed_grant BEFORE UPDATE OF status \
               ON {SCHEMA}.runs FOR EACH ROW \
               EXECUTE FUNCTION {SCHEMA}.refuse_probed_grant();"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    let starved = plugin
        .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
        .await
        .expect_err("the probed grant refuses");
    assert_eq!(starved.error_type(), ProductionLeaseErrorType::Storage);
    assert_eq!(starved.operation(), "grant production lease");
    assert_eq!(
        queue_attempts(admin, "grant-refused").await?,
        3,
        "the refused lease rolled its own crash evidence back"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP TRIGGER refuse_probed_grant ON {SCHEMA}.runs; \
             DROP FUNCTION {SCHEMA}.refuse_probed_grant();"
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "grant-refused".into()
        },
        "the budget the refusal spent must reach the janitor"
    );
    assert_terminal_status_dequeued(admin, "grant-refused", "infrastructure-failure").await?;

    // ---- the admission pin (docs/architecture/overview.md#release-identity) ------------------------
    //
    // Admission pins the release by its manifest digest. A matching pod leases
    // the run and never writes the pin.
    seed_run(admin, "release-record", PACKAGE_ID).await?;
    let recorded = Some(POD_MANIFEST_DIGEST.to_string());
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    assert_eq!(
        ready_run(
            plugin
                .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // SAME-RELEASE RE-LEASE. The classifier's pre-effect recover keeps the pin.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        ready_run(
            plugin
                .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // A release is an admission pin, not a lease-time rollout slot. A pod
    // carrying another release does not select this run at all (R20): the
    // candidate select filters by the pod's release, and the grant keeps the
    // test again.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        plugin
            .lease_next(ROLLED_COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionLeaseResult::Empty,
        "a different release must not select an admitted run"
    );
    assert!(
        plugin
            .pinned_backlog(COMPONENT, &release_package_ids, ENVIRONMENT)
            .await?,
        "the expired run is the pinned backlog of its own release"
    );
    assert!(
        !plugin
            .pinned_backlog(ROLLED_COMPONENT, &release_package_ids, ENVIRONMENT)
            .await?,
        "another release has no pinned backlog"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    assert_eq!(
        ready_run(
            plugin
                .lease_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // No path rewrites or erases the pin.
    for assignment in [quote_literal(EMPTY_HASH), "NULL".to_owned()] {
        let rewritten = sqlx::query(sqlx::AssertSqlSafe(
            (&format!(
                "UPDATE {SCHEMA}.runs SET manifest_digest={assignment} \
                      WHERE project_env_key=$1 AND run_id='release-record'"
            ))
                .to_string(),
        ))
        .bind(&PROJECT_ENV_KEY)
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())
        .expect_err("no path may rewrite an admitted release in place");
        let db = rewritten
            .as_database_error()
            .expect("guard refusal is a db error");
        assert_eq!(db.code().as_deref().unwrap(), "55000");
        assert_eq!(db.message(), "run-admission-pin-immutable");
    }
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "DELETE FROM {SCHEMA}.run_queue \
                  WHERE project_env_key=$1 AND run_id='release-record'"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;

    teardown(fixture).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_writers_stamp_existing_executor_leases() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::project();
    let mut fixture = install_fixture(database.url()).await?;
    let admin = &mut fixture.admin;
    let plugin = &fixture.plugin;
    // Admission already binds the executor in its existing transaction.

    let actor = wamn_identity_protocol::PlatformComponent::Executor
        .principal_id()
        .to_string();
    let admitted = plugin
        .admit_manual_run(
            "default",
            PROJECT_ENV_KEY,
            &wamn_runtime::plugins::wamn_postgres::ManualRunAdmission {
                package_id: PACKAGE_ID,
                manifest_digest: POD_MANIFEST_DIGEST,
                environment: ENVIRONMENT,
                workflow_id: WIRING_ID,
                workflow_version: WIRING_VERSION,
                wiring_hash: EMPTY_HASH,
                execution_principal_id: &actor,
                execution_role: "workflow",
                invoked_by_user_id: &actor,
                node_digests: &json!({}),
                idempotency_key: "history-admission",
                input: &json!({}),
            },
        )
        .await?;
    let wamn_runtime::plugins::wamn_postgres::ManualRunAdmitted::Queued { run_id } = admitted
    else {
        panic!("new admission did not queue its run");
    };
    wamn_test_infrastructure::record_history::assert_stamp(
        admin,
        "wamn_run.run_queue",
        &format!("run_id='{run_id}'"),
        &actor,
        &actor,
    )
    .await;
    sqlx::query("DELETE FROM wamn_run.run_queue WHERE run_id=$1")
        .bind(&run_id)
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())?;

    seed_run(admin, "history-lease", PACKAGE_ID).await?;
    let acquired = ready_run(
        plugin
            .lease_next(COMPONENT, &[PACKAGE_ID.to_owned()], ENVIRONMENT, 30_000)
            .await?,
    );
    assert_eq!(acquired, "history-lease");
    wamn_test_infrastructure::record_history::assert_stamp(
        admin,
        "wamn_run.run_queue",
        "run_id='history-lease'",
        &wamn_identity_protocol::PlatformComponent::Provisioning
            .principal_id()
            .to_string(),
        &actor,
    )
    .await;
    teardown(fixture).await
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_retries_end_with_the_last_cause() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::project();
    let mut fixture = install_fixture(database.url()).await?;
    let admin = &mut fixture.admin;
    let plugin = &fixture.plugin;
    let packages = [PACKAGE_ID.to_owned()];
    for last_release in [true, false] {
        let run = if last_release {
            "last-release"
        } else {
            "last-expiry"
        };
        seed_run(admin, run, PACKAGE_ID).await?;
        sqlx::query("UPDATE wamn_run.run_queue SET max_attempts=3 WHERE run_id=$1")
            .bind(&run)
            .execute(&mut *admin)
            .await
            .map(|result| result.rows_affected())?;
        let lease = plugin
            .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionLeaseResult::Ready {
            lease_generation, ..
        } = lease
        else {
            panic!("first lease missing")
        };
        assert!(
            plugin
                .release_lease(COMPONENT, run, lease_generation)
                .await?
        );
        assert_eq!(queue_attempts(admin, run).await?, 1);
        sqlx::query("UPDATE wamn_run.run_queue SET available_at='2000-01-01' WHERE run_id=$1")
            .bind(&run)
            .execute(&mut *admin)
            .await
            .map(|result| result.rows_affected())?;
        let lease = plugin
            .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        assert_eq!(ready_run(lease), run);
        assert_eq!(
            queue_attempts(admin, run).await?,
            1,
            "a released lease must not count twice"
        );
        expire_effect_run(admin, run).await?;
        let lease = plugin
            .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionLeaseResult::Ready {
            lease_generation, ..
        } = lease
        else {
            panic!("expired lease missing")
        };
        assert_eq!(
            queue_attempts(admin, run).await?,
            2,
            "expiry shares the release count"
        );
        if last_release {
            assert!(
                plugin
                    .release_lease(COMPONENT, run, lease_generation)
                    .await?
            );
            assert_terminal_status_dequeued(admin, run, "failed").await?;
            let row = sqlx::query("SELECT fail_type,result_json::text,terminal_reason FROM wamn_run.runs WHERE run_id=$1").bind(&run).fetch_one(&mut *admin).await?;
            assert_eq!(row.get::<String, _>(0), "retry-exhausted");
            assert_eq!(row.get::<String, _>(2), "retry-exhausted");
            let result: Value = serde_json::from_str(&row.get::<String, _>(1))?;
            assert_eq!(result["error"]["code"], "retry-exhausted");
        } else {
            expire_effect_run(admin, run).await?;
            assert_eq!(
                ready_run(
                    plugin
                        .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
                        .await?
                ),
                run
            );
            assert_eq!(queue_attempts(admin, run).await?, 3);
            expire_effect_run(admin, run).await?;
            assert!(matches!(
                plugin
                    .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
                    .await?,
                ProductionLeaseResult::Empty
            ));
            assert_eq!(
                plugin
                    .reap_exhausted(COMPONENT, &packages, ENVIRONMENT, 0)
                    .await?,
                ProductionReapResult::Reaped {
                    run_id: run.to_owned()
                }
            );
            assert_terminal_status_dequeued(admin, run, "infrastructure-failure").await?;
            let body: String = sqlx::query("SELECT result_json::text FROM wamn_run.runs WHERE run_id=$1 AND terminal_reason='infrastructure-failure'").bind(&run).fetch_one(&mut *admin)
                .await?
                .get(0);
            assert_eq!(
                serde_json::from_str::<Value>(&body)?["error"]["code"],
                "infrastructure-failure"
            );
        }
        assert!(
            !plugin
                .release_lease(COMPONENT, run, lease_generation)
                .await?,
            "a terminal run cannot be released again"
        );
    }
    teardown(fixture).await
}

#[tokio::test(flavor = "multi_thread")]
async fn lease_release_fences_and_bounds_backoff() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::project();
    let mut fixture = install_fixture(database.url()).await?;
    let admin = &mut fixture.admin;
    let plugin = &fixture.plugin;
    let packages = [PACKAGE_ID.to_owned()];
    for attempts in [0_i32, 1, 2, 8, 9, 18] {
        let run = format!("backoff-{attempts}");
        seed_run(admin, &run, PACKAGE_ID).await?;
        let lease = plugin
            .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionLeaseResult::Ready {
            lease_generation, ..
        } = lease
        else {
            panic!("lease missing")
        };
        sqlx::query("UPDATE wamn_run.run_queue SET attempts=$2 WHERE run_id=$1")
            .bind(&run)
            .bind(&attempts)
            .execute(&mut *admin)
            .await
            .map(|result| result.rows_affected())?;
        assert!(
            !plugin
                .release_lease(COMPONENT, &run, lease_generation + 1)
                .await?
        );
        assert!(
            !plugin
                .release_lease(ROLLED_COMPONENT, &run, lease_generation)
                .await?
        );
        assert_eq!(queue_attempts(admin, &run).await?, attempts);
        let before: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
            sqlx::query("SELECT clock_timestamp()")
                .fetch_one(&mut *admin)
                .await?
                .get(0);
        assert!(
            plugin
                .release_lease(COMPONENT, &run, lease_generation)
                .await?
        );
        let base = (1000_i64 << attempts).min(300_000);
        let upper = (base * 12 / 10).min(300_000);
        let row = sqlx::query(
            "SELECT attempts,lease_owner IS NULL AND lease_expires_at IS NULL, \
             available_at >= $2::timestamptz + $3::bigint * interval '1 millisecond' \
             AND available_at <= clock_timestamp() + $4::bigint * interval '1 millisecond' \
             FROM wamn_run.run_queue WHERE run_id=$1",
        )
        .bind(&run)
        .bind(&before)
        .bind(&base)
        .bind(&upper)
        .fetch_one(&mut *admin)
        .await?;
        assert_eq!(row.get::<i32, _>(0), attempts + 1);
        assert!(row.get::<bool, _>(1));
        assert!(
            row.get::<bool, _>(2),
            "attempt {attempts} delay outside jitter bounds"
        );
        assert!(
            !plugin
                .release_lease(COMPONENT, &run, lease_generation)
                .await?
        );
        sqlx::query("UPDATE wamn_run.run_queue SET available_at = 'infinity' WHERE run_id=$1")
            .bind(&run)
            .execute(&mut *admin)
            .await
            .map(|result| result.rows_affected())?;
    }
    teardown(fixture).await
}
