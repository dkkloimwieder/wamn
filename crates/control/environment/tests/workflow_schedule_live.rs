//! Schedule workflows from rows, live
//! (docs/architecture/execution.md#workflows): a schedule trigger authored with
//! its cron, the enable rule of a schedule, the host-owned admission of a due
//! moment, and the host's schedule task, which reads the enabled rows at each
//! tick.
//!
//! The fixture release carries one package, `orders`, with one read
//! operation (a WAT component that answers fixed rows and writes nothing). A
//! one-node workflow over the read is authored through the code path of
//! `wamn-ctl workflow author --schedule`, enabled, and admitted with
//! `wamn_workflow::admission::admit_schedule_run` as the host's schedule task will admit
//! it. The real queue service of the release drains it through the router,
//! under the targeted executor and application logins.

use sqlx::{Connection as _, Row as _};
#[path = "support/schedule_fixture.rs"]
pub mod support;
use support::*;

/// An invalid cron refuses at author with the stable schedule refusal, and no
/// row is written.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_cron_refuses_at_author() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    for cron in ["* * * *", "* * * * * *", "61 * * * *", "every minute"] {
        let refusal = fixture
            .author(
                "nightly",
                WorkflowTrigger::Schedule(cron),
                WorkflowExecution::Role("reader"),
            )
            .await
            .expect_err("an invalid cron refuses");
        let refusal = refusal
            .downcast_ref::<AuthorWorkflowError>()
            .expect("an authorship refusal");
        assert_eq!(
            refusal.error_type(),
            AuthorWorkflowErrorType::Schedule,
            "{cron}"
        );
    }
    let rows: i64 = sqlx::query("SELECT count(*) FROM catalog.workflows")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);

    // The same version under another trigger or cron is another definition.
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("a valid cron is authored");
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("the same definition converges");
    for trigger in [
        WorkflowTrigger::Manual,
        WorkflowTrigger::Schedule("0 * * * *"),
    ] {
        let refusal = fixture
            .author("nightly", trigger, WorkflowExecution::Role("reader"))
            .await
            .expect_err("another trigger under an authored version refuses");
        assert_eq!(
            refusal
                .downcast_ref::<AuthorWorkflowError>()
                .expect("an authorship refusal")
                .error_type(),
            AuthorWorkflowErrorType::Conflict,
        );
    }
}

/// An application the environment does not declare refuses at author with
/// the stable application refusal, and no row is written.
#[tokio::test(flavor = "multi_thread")]
async fn an_undeclared_application_refuses_at_author() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let other: wamn_catalog::EnvironmentApplications = [(
        "billing".to_owned(),
        wamn_catalog::ApplicationDefinition {
            connections: std::collections::BTreeMap::new(),
            packages: [APPLICATION.to_owned()].into(),
        },
    )]
    .into();
    for declared in [wamn_catalog::EnvironmentApplications::new(), other] {
        let refusal = fixture
            .author_in(
                "nightly",
                WorkflowTrigger::Manual,
                WorkflowExecution::Role("reader"),
                &declared,
            )
            .await
            .expect_err("an undeclared application refuses");
        assert_eq!(
            refusal
                .downcast_ref::<AuthorWorkflowError>()
                .expect("an authorship refusal")
                .error_type(),
            AuthorWorkflowErrorType::Application,
        );
    }
    let rows: i64 = sqlx::query("SELECT count(*) FROM catalog.workflows")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    assert_eq!(rows, 0);
}

/// A `role` schedule workflow: two admissions of one due moment answer one
/// run of the schedule workflow grain, and the queue executes it as the
/// `workflow` user with the one role. A workflow whose role lacks the read
/// fails at its node, although the `workflow` user holds a role that has it.
#[tokio::test(flavor = "multi_thread")]
async fn a_role_schedule_run_is_admitted_once_per_due_and_runs_as_the_workflow_user() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture.grant_workflow_user("clerk").await;
    for (id, role) in [("nightly", "reader"), ("clerkly", "clerk")] {
        fixture
            .author(
                id,
                WorkflowTrigger::Schedule(CRON),
                WorkflowExecution::Role(role),
            )
            .await
            .expect("author the schedule workflow");
    }
    fixture
        .enable("nightly", READER)
        .await
        .expect("an enabler holding the role enables");
    fixture
        .enable("clerkly", ADMIN)
        .await
        .expect("an admin enables");

    let run = fixture.queued("nightly", "2026-10-20T10:15:00Z").await;
    assert_eq!(
        fixture.queued("nightly", "2026-10-20T10:15:00Z").await,
        run,
        "a second admission of the same due moment answers the first run"
    );
    assert_eq!(fixture.runs().await, 1);
    let denied = fixture.queued("clerkly", "2026-10-20T10:15:00Z").await;

    let row = sqlx::query(
        "SELECT trigger_source, registration_id IS NULL, \
                    wiring_id, wiring_version, wiring_hash, manifest_digest, \
                    node_digests_json::text, execution_principal_id::text, execution_role, \
                    invoked_by, invoked_by_user_id IS NULL, idempotency_key, input_json::text, \
                    package_id \
               FROM wamn_run.runs WHERE run_id = $1",
    )
    .bind(&run)
    .fetch_one(&mut fixture.admin)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>(0), "schedule");
    assert!(row.get::<bool, _>(1), "no registration");
    assert_eq!(
        (row.get::<String, _>(2), row.get::<i32, _>(3)),
        ("nightly".to_owned(), 1)
    );
    assert_eq!(
        row.get::<String, _>(4),
        workflow("nightly").wiring_hash().as_str()
    );
    assert_eq!(row.get::<String, _>(5), fixture.digest);
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<String, _>(6)).unwrap(),
        json!({"read": fixture.read.component_digest})
    );
    assert_eq!(row.get::<String, _>(7), workflow_user_id().to_string());
    assert_eq!(row.get::<String, _>(8), "reader");
    assert_eq!(row.get::<String, _>(9), "schedule");
    assert!(row.get::<bool, _>(10), "no invoking user");
    assert_eq!(
        row.get::<String, _>(11),
        "schedule:erp/nightly:2026-10-20T10:15:00Z"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<String, _>(12)).unwrap(),
        json!({"due": "2026-10-20T10:15:00Z"})
    );
    assert_eq!(row.get::<String, _>(13), APP);

    fixture.drain().await;
    let (status, result) = fixture.outcome(&run).await;
    assert_eq!(status, "completed", "{result:?}");
    assert_eq!(result, Some(serde_json::from_str(ROWS).unwrap()));
    let (status, result) = fixture.outcome(&denied).await;
    assert_eq!(status, "failed");
    let error = &result.expect("a failed run records its refusal")["error"];
    assert_eq!(error["code"], "permission_denied");
    assert_eq!(error["operation"], READ);
}

/// An `invoker` schedule workflow assigns `admin`: a non-admin enabler
/// refuses, an admin refuses while the `workflow` user lacks `admin`, and is
/// allowed once it holds it. The run executes as the `workflow` user with
/// `admin` active.
#[tokio::test(flavor = "multi_thread")]
async fn an_invoker_schedule_needs_admin_to_enable() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Invoker,
        )
        .await
        .expect("author the schedule workflow");

    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", READER).await),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler
    );
    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", ADMIN).await),
        WorkflowLifecycleErrorType::RoleNotHeldByWorkflowUser
    );
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled
    );
    fixture.grant_workflow_user("admin").await;
    assert_eq!(
        lifecycle_refusal(fixture.enable("nightly", READER).await),
        WorkflowLifecycleErrorType::RoleNotHeldByEnabler,
        "a non-admin still refuses"
    );
    fixture
        .enable("nightly", ADMIN)
        .await
        .expect("an admin enables while the workflow user holds admin");

    let run = fixture.queued("nightly", "2026-10-20T10:20:00Z").await;
    let row = sqlx::query(
        "SELECT execution_principal_id::text, execution_role, invoked_by \
               FROM wamn_run.runs WHERE run_id = $1",
    )
    .bind(&run)
    .fetch_one(&mut fixture.admin)
    .await
    .unwrap();
    assert_eq!(
        (
            row.get::<String, _>(0),
            row.get::<String, _>(1),
            row.get::<String, _>(2)
        ),
        (
            workflow_user_id().to_string(),
            "admin".to_owned(),
            "schedule".to_owned()
        )
    );
    fixture.drain().await;
    assert_eq!(fixture.outcome(&run).await.0, "completed");
}

/// No enabled schedule version refuses with no row: a workflow never
/// enabled, a manual workflow, and a schedule workflow after its disable.
#[tokio::test(flavor = "multi_thread")]
async fn no_enabled_schedule_version_refuses_with_no_row() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled
    );
    fixture
        .author(
            "manual",
            WorkflowTrigger::Manual,
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author a manual workflow");
    fixture
        .enable("manual", READER)
        .await
        .expect("enable the manual workflow");
    assert_eq!(
        fixture.admit("manual", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "a manual workflow has no schedule run"
    );
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author the schedule workflow");
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:15:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "an authored version is disabled"
    );
    fixture
        .enable("nightly", READER)
        .await
        .expect("enable the schedule workflow");
    wamn_workflow::workflow_lifecycle::disable_workflow(
        &fixture.url,
        &Triple::new("acme", ENVIRONMENT, APPLICATION),
        &mut fixture.admin,
        &WorkflowVersion {
            org: "schedule-live",
            project: "default",
            environment: ENVIRONMENT,
            application: APP,
            workflow_id: "nightly",
            version: 1,
        },
    )
    .await
    .expect("disable the schedule workflow");
    assert_eq!(
        fixture.admit("nightly", "2026-10-20T10:20:00Z").await,
        ScheduleRunAdmitted::NotEnabled,
        "a disabled version has no schedule run"
    );
    assert_eq!(fixture.runs().await, 0);
}

/// The running schedule service observes enable and disable through the runtime pool.
#[tokio::test(flavor = "multi_thread")]
async fn the_schedule_task_fires_a_row_enabled_after_start_and_stops_after_disable() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "minutely",
            WorkflowTrigger::Schedule("* * * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .unwrap();
    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        Arc::clone(&fixture.postgres),
        PROJECT,
        Arc::clone(&fixture.release),
    );
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let task =
        tokio::spawn(async move { wamn_workflow::schedule::serve(&schedules, stopping).await });
    fixture.enable("minutely", READER).await.unwrap();
    tokio::time::timeout(Duration::from_secs(70), async {
        while fixture.dues("minutely").await.is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the next minute admits the enabled workflow");
    fixture.disable("minutely").await.unwrap();
    let admitted = fixture.dues("minutely").await;
    tokio::time::sleep(Duration::from_secs(65)).await;
    assert_eq!(fixture.dues("minutely").await, admitted);
    assert!(!task.is_finished());
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    fixture.drain().await;
    let run: String = sqlx::query("SELECT run_id FROM wamn_run.runs WHERE wiring_id='minutely'")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        wamn_run_state::RunStatus::from_sql(&fixture.outcome(&run).await.0),
        Some(wamn_run_state::RunStatus::Completed)
    );
}

/// A slow database read admits its own due moment and skips missed minutes.
#[tokio::test(flavor = "multi_thread")]
async fn schedules_run_from_rows_and_a_missed_tick_is_skipped() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "minutely",
            WorkflowTrigger::Schedule("* * * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .unwrap();
    fixture.enable("minutely", READER).await.unwrap();
    let mut locker = connect(&fixture.url).await;
    let mut hold = locker.begin().await.unwrap();
    sqlx::raw_sql("LOCK TABLE catalog.workflows IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *hold)
        .await
        .unwrap();
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(
                ClassCredentials::default()
                    .with_class(AuthorityClass::ExecutorPlatform, &fixture.executor_url),
            ),
            statement_timeout_ms: 120_000,
            ..WamnPostgresConfig::from_env()
        })
        .unwrap(),
    );
    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        postgres,
        PROJECT,
        Arc::clone(&fixture.release),
    );
    let executor = url::Url::parse(&fixture.executor_url)
        .unwrap()
        .username()
        .to_owned();
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let task =
        tokio::spawn(async move { wamn_workflow::schedule::serve(&schedules, stopping).await });
    tokio::time::timeout(Duration::from_secs(70), async {
        loop {
            let blocked: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename=$1 AND wait_event_type='Lock')")
                .bind(&executor).fetch_one(&mut fixture.admin).await.unwrap().get(0);
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("the schedule read waits for the database lock");
    tokio::time::sleep(Duration::from_secs(65)).await;
    assert!(fixture.dues("minutely").await.is_empty());
    hold.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(70), async {
        while fixture.dues("minutely").await.len() < 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the current and next visible ticks admit");
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    let admitted = fixture.dues("minutely").await;
    assert!(admitted[1] - admitted[0] >= chrono::Duration::minutes(2));
}

#[path = "workflow_schedule_live/event_batch.rs"]
mod event_batch;

/// A replacement changes only A's schedule pins. B keeps admitting under its loaded release.
#[tokio::test(flavor = "multi_thread")]
async fn applications_schedule_independently_across_upgrade_and_rollback() {
    use wamn_workflow::schedule::PostgresSchedules;
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "nightly",
            WorkflowTrigger::Schedule(CRON),
            WorkflowExecution::Role("reader"),
        )
        .await
        .unwrap();
    fixture.enable("nightly", READER).await.unwrap();
    sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, schedule, execution_type, execution_role, enabled, created_by) \
         SELECT project_env_key, environment, 'billing', workflow_id, version, graph_json, wiring_hash, trigger, schedule, execution_type, execution_role, enabled, created_by FROM catalog.workflows WHERE application=$1").bind(&APP).execute(&mut fixture.admin).await.map(|result| result.rows_affected()).unwrap();
    let upgraded = Arc::new(release_variant(&mut fixture, APP, b"1251").await);
    let billing = Arc::new(release_variant(&mut fixture, "billing", b"1252").await);
    let b = PostgresSchedules::new(Arc::clone(&fixture.postgres), PROJECT, Arc::clone(&billing));
    let rows_tick = due("2026-10-20T10:15:00Z");
    let b_rows = b.enabled(rows_tick).await.unwrap();
    assert!(b_rows.iter().all(|row| row.application == "billing"));
    assert!(!b_rows.is_empty());
    for (minute, release) in [
        (15, &fixture.release),
        (20, &upgraded),
        (25, &fixture.release),
    ] {
        let a = PostgresSchedules::new(Arc::clone(&fixture.postgres), PROJECT, Arc::clone(release));
        let a_rows = a.enabled(rows_tick).await.unwrap();
        assert!(a_rows.iter().all(|row| row.application == APP));
        assert!(!a_rows.is_empty());
        let tick = due(&format!("2026-10-20T10:{minute}:00Z"));
        for (schedules, rows, loaded) in [(&b, &b_rows, &billing), (&a, &a_rows, release)] {
            for row in rows {
                let ScheduleRunAdmitted::Queued { run_id } =
                    schedules.admit(row, tick).await.unwrap()
                else {
                    panic!("schedule was not admitted");
                };
                assert_eq!(
                    schedules.admit(row, tick).await.unwrap(),
                    ScheduleRunAdmitted::Queued {
                        run_id: run_id.clone()
                    }
                );
                let stored = sqlx::query(
                    "SELECT package_id, manifest_digest FROM wamn_run.runs WHERE run_id=$1",
                )
                .bind(&run_id)
                .fetch_one(&mut fixture.admin)
                .await
                .unwrap();
                assert_eq!(stored.get::<String, _>(0), row.application);
                assert_eq!(
                    stored.get::<String, _>(1),
                    loaded.release().manifest_digest.as_str()
                );
            }
        }
        assert!(a.admit(&b_rows[0], tick).await.is_err());
        assert_eq!(b.enabled(rows_tick).await.unwrap(), b_rows);
    }
}

/// The project time zone supplies local calendar minutes across daylight saving changes.
#[tokio::test]
async fn schedule_ticks_use_the_environment_time_zone() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "daily",
            WorkflowTrigger::Schedule("0 6 * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .unwrap();
    fixture.enable("daily", READER).await.unwrap();
    sqlx::query("INSERT INTO platform.setting (target_type,target_id,key,value) VALUES ('project',$1,'timezone',$2::text::jsonb)")
        .bind(PROJECT).bind(serde_json::json!("America/New_York").to_string())
        .execute(&mut fixture.admin).await.unwrap();
    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        Arc::clone(&fixture.postgres),
        PROJECT,
        Arc::clone(&fixture.release),
    );
    for tick in [
        "2026-03-07T11:00:00Z",
        "2026-03-08T10:00:00Z",
        "2026-10-31T10:00:00Z",
        "2026-11-01T11:00:00Z",
    ] {
        let rows = schedules.enabled(due(tick)).await.unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            assert!(wamn_workflow::schedule::is_due(&row.schedule, row.local_tick).unwrap());
        }
    }
    sqlx::query("UPDATE platform.setting SET value='7'::jsonb WHERE target_type='project' AND target_id=$1 AND key='timezone'")
        .bind(PROJECT).execute(&mut fixture.admin).await.unwrap();
    assert!(schedules.enabled(Utc::now()).await.is_err());
}
