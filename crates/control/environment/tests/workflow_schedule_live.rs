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
//! `WamnPostgres::admit_schedule_run` as the host's schedule task will admit
//! it. The real queue service of the release drains it through the router,
//! under the scoped executor and application logins.

#[path = "support/schedule_fixture.rs"]
pub mod support;
use support::*;

/// An invalid cron refuses at author with the stable schedule refusal, and no
/// row is written.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_cron_refuses_at_author() {
    let _lock = wamn_test_postgres::lock();
    let fixture = fixture().await;
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
    let rows: i64 = fixture
        .admin
        .query_one("SELECT count(*) FROM catalog.workflows", &[])
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
    let fixture = fixture().await;
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
    let rows: i64 = fixture
        .admin
        .query_one("SELECT count(*) FROM catalog.workflows", &[])
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

    let row = fixture
        .admin
        .query_one(
            "SELECT trigger_source, service_principal_id IS NULL, registration_id IS NULL, \
                    wiring_id, wiring_version, wiring_hash, manifest_digest, \
                    node_digests_json::text, execution_principal_id::text, execution_role, \
                    invoked_by, invoked_by_user_id IS NULL, idempotency_key, input_json::text, \
                    package_id \
               FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "schedule");
    assert!(row.get::<_, bool>(1), "no service principal");
    assert!(row.get::<_, bool>(2), "no registration");
    assert_eq!(
        (row.get::<_, String>(3), row.get::<_, i32>(4)),
        ("nightly".to_owned(), 1)
    );
    assert_eq!(
        row.get::<_, String>(5),
        workflow("nightly").wiring_hash().as_str()
    );
    assert_eq!(row.get::<_, String>(6), fixture.digest);
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<_, String>(7)).unwrap(),
        json!({"read": fixture.read.component_digest})
    );
    assert_eq!(row.get::<_, String>(8), workflow_user_id().to_string());
    assert_eq!(row.get::<_, String>(9), "reader");
    assert_eq!(row.get::<_, String>(10), "schedule");
    assert!(row.get::<_, bool>(11), "no invoking user");
    assert_eq!(
        row.get::<_, String>(12),
        "schedule:erp/nightly:2026-10-20T10:15:00Z"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&row.get::<_, String>(13)).unwrap(),
        json!({"due": "2026-10-20T10:15:00Z"})
    );
    assert_eq!(row.get::<_, String>(14), APP);

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
    let row = fixture
        .admin
        .query_one(
            "SELECT execution_principal_id::text, execution_role, invoked_by \
               FROM wamn_run.runs WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
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
    wamn_environment_control::workflow_lifecycle::disable_workflow(
        &fixture.url,
        &Triple::new("acme", APPLICATION, ENVIRONMENT),
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

#[derive(Clone)]
struct ScheduleClock {
    now: tokio::sync::watch::Sender<DateTime<Utc>>,
    waiting: tokio::sync::mpsc::UnboundedSender<DateTime<Utc>>,
}

impl ScheduleClock {
    fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<DateTime<Utc>>) {
        let (now, _) = tokio::sync::watch::channel(due("2026-10-20T10:15:23Z"));
        let (waiting, ticks) = tokio::sync::mpsc::unbounded_channel();
        (Self { now, waiting }, ticks)
    }

    async fn wait_until(&self, tick: DateTime<Utc>) {
        let mut clock = self.now.subscribe();
        self.waiting.send(tick).unwrap();
        clock.wait_for(|now| *now >= tick).await.unwrap();
    }
}

async fn next_tick(
    ticks: &mut tokio::sync::mpsc::UnboundedReceiver<DateTime<Utc>>,
) -> DateTime<Utc> {
    tokio::time::timeout(Duration::from_secs(10), ticks.recv())
        .await
        .expect("the schedule task reaches its next wait")
        .expect("the schedule task keeps serving")
}

/// The host's schedule task reads the enabled rows at each tick: a row
/// enabled after the task started fires at its next tick, and a row disabled
/// fires at no later tick. The test controls each minute boundary.
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
        .expect("author the schedule workflow");

    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        connect(&fixture.executor_url).await,
        Arc::clone(&fixture.postgres),
        PROJECT,
        Arc::clone(&fixture.release),
    );
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let (clock, mut ticks) = ScheduleClock::new();
    let serving_clock = clock.clone();
    let task = tokio::spawn(async move {
        wamn_workflow::schedule::serve_with_clock(
            &schedules,
            stopping,
            || *serving_clock.now.borrow(),
            |tick| serving_clock.wait_until(tick),
        )
        .await
    });

    let first = next_tick(&mut ticks).await;
    assert_eq!(first, due("2026-10-20T10:16:00Z"));
    fixture
        .enable("minutely", READER)
        .await
        .expect("an enabler holding the role enables");
    clock.now.send_replace(first);
    let second = next_tick(&mut ticks).await;
    assert_eq!(fixture.dues("minutely").await, [first]);

    fixture
        .disable("minutely")
        .await
        .expect("the enabled version disables");
    clock.now.send_replace(second);
    next_tick(&mut ticks).await;
    assert_eq!(fixture.dues("minutely").await, [first]);
    assert!(!task.is_finished(), "the schedule task keeps serving");

    stop.send(true).unwrap();
    task.await
        .unwrap()
        .expect("the schedule task stops at shutdown");
    fixture.drain().await;
    let run: String = fixture
        .admin
        .query_one(
            "SELECT run_id FROM wamn_run.runs WHERE wiring_id = 'minutely' ORDER BY created_at LIMIT 1",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(fixture.outcome(&run).await.0, "completed");
}

/// Schedules run from rows (docs/architecture/execution.md#workflows): the
/// serving release declares no schedule, an authored and enabled row fires, and
/// a tick missed while the task waits on the database is skipped, not caught
/// up. The enable after start and the disable are the case above. A missed tick
/// during a slow admission is the schedule task's unit test
/// `ticks_missed_during_a_slow_admission_are_skipped`. Here the task's read of
/// the rows waits on a database lock while the test advances the clock.
#[tokio::test(flavor = "multi_thread")]
async fn schedules_run_from_rows_and_a_missed_tick_is_skipped() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    let stored: Vec<u8> = fixture
        .admin
        .query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE manifest_digest = $1",
            &[&fixture.digest],
        )
        .await
        .unwrap()
        .get(0);
    let stored: Value = serde_json::from_slice(&stored).unwrap();
    assert!(
        stored.pointer("/workflow/schedules").is_none(),
        "the serving release declares no schedule: {stored}"
    );
    fixture.grant_workflow_user("reader").await;
    fixture
        .author(
            "minutely",
            WorkflowTrigger::Schedule("* * * * *"),
            WorkflowExecution::Role("reader"),
        )
        .await
        .expect("author the schedule workflow");
    fixture
        .enable("minutely", READER)
        .await
        .expect("an enabler holding the role enables");

    // Hold the rows from before the first tick until after the second.
    let mut locker = connect(&fixture.url).await;
    let hold = locker.transaction().await.unwrap();
    hold.batch_execute("LOCK TABLE catalog.workflows IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let connection = connect(&fixture.executor_url).await;
    let reader_pid: i32 = connection
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let schedules = wamn_workflow::schedule::PostgresSchedules::new(
        connection,
        Arc::clone(&fixture.postgres),
        PROJECT,
        Arc::clone(&fixture.release),
    );
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let (clock, mut ticks) = ScheduleClock::new();
    let serving_clock = clock.clone();
    let task = tokio::spawn(async move {
        wamn_workflow::schedule::serve_with_clock(
            &schedules,
            stopping,
            || *serving_clock.now.borrow(),
            |tick| serving_clock.wait_until(tick),
        )
        .await
    });
    let first = next_tick(&mut ticks).await;
    assert_eq!(first, due("2026-10-20T10:16:00Z"));
    clock.now.send_replace(first);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = fixture.admin.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock')",
                &[&reader_pid],
            ).await.unwrap().get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("the schedule read waits on the held database lock");
    clock
        .now
        .send_replace(first + chrono::Duration::seconds(65));
    assert!(
        fixture.dues("minutely").await.is_empty(),
        "no run is admitted while the rows are locked"
    );
    hold.commit().await.unwrap();
    let third = next_tick(&mut ticks).await;
    assert_eq!(third, first + chrono::Duration::minutes(2));
    clock.now.send_replace(third);
    next_tick(&mut ticks).await;
    stop.send(true).unwrap();
    task.await
        .unwrap()
        .expect("the schedule task stops at shutdown");
    assert_eq!(
        fixture.dues("minutely").await,
        [first, third],
        "the late first tick admits its own due, the missed second tick is skipped, \
         and the third fires"
    );
}

#[path = "workflow_schedule_live/event_batch.rs"]
mod event_batch;

/// A replacement changes only A's schedule pins. B keeps admitting under its loaded release.
#[tokio::test(flavor = "multi_thread")]
async fn applications_schedule_independently_across_upgrade_and_rollback() {
    use wamn_workflow::schedule::{PostgresSchedules, ScheduleRows as _};
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
    fixture.admin.execute(
        "INSERT INTO catalog.workflows (env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, schedule, execution_type, execution_role, enabled, created_by) \
         SELECT env_key, environment, 'billing', workflow_id, version, graph_json, wiring_hash, trigger, schedule, execution_type, execution_role, enabled, created_by FROM catalog.workflows WHERE application=$1",
        &[&APP],
    ).await.unwrap();
    let upgraded = Arc::new(release_variant(&fixture, APP, b"1251").await);
    let billing = Arc::new(release_variant(&fixture, "billing", b"1252").await);
    let b = PostgresSchedules::new(
        connect(&fixture.executor_url).await,
        Arc::clone(&fixture.postgres),
        PROJECT,
        Arc::clone(&billing),
    );
    let b_rows = b.enabled().await.unwrap();
    assert!(b_rows.iter().all(|row| row.application == "billing"));
    assert!(!b_rows.is_empty());
    for (minute, release) in [
        (15, &fixture.release),
        (20, &upgraded),
        (25, &fixture.release),
    ] {
        let a = PostgresSchedules::new(
            connect(&fixture.executor_url).await,
            Arc::clone(&fixture.postgres),
            PROJECT,
            Arc::clone(release),
        );
        let a_rows = a.enabled().await.unwrap();
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
                let stored = fixture
                    .admin
                    .query_one(
                        "SELECT package_id, manifest_digest FROM wamn_run.runs WHERE run_id=$1",
                        &[&run_id],
                    )
                    .await
                    .unwrap();
                assert_eq!(stored.get::<_, String>(0), row.application);
                assert_eq!(
                    stored.get::<_, String>(1),
                    loaded.release().manifest_digest.as_str()
                );
            }
        }
        assert!(a.admit(&b_rows[0], tick).await.is_err());
        assert_eq!(b.enabled().await.unwrap(), b_rows);
    }
}
