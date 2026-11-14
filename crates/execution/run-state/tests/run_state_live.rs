//! Live contracts for run admission, lease fencing, recovery and pruning.
use sqlx::{Connection as _, Row as _};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_run_state::admission::{self, RunAdmission, RunAdmitted, RunTrigger};
use wamn_run_state::persistence::{self, LeaseTurn, ProductionLeaseResult};
use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseRenewal, ProductionReapResult,
};

use std::io::Write as _;
use std::process::{Command, Output, Stdio};
fn psql(url: &str, script: &str) -> Output {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let script =
        format!("SET app.user_id='{actor}'; SET app.operation='admin:run-state-fixture'; {script}");
    let mut child = Command::new("psql")
        .args(["-X", "-v", "ON_ERROR_STOP=1", "-Atq", "-1", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start psql");
    let mut stdin = child.stdin.take().expect("open psql stdin");
    if let Err(error) = stdin.write_all(script.as_bytes()) {
        eprintln!("psql closed stdin before the full script was written: {error}");
    }
    drop(stdin);
    child.wait_with_output().expect("run psql")
}

fn success(url: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("psql stdout is utf-8")
}

const EXECUTOR_LOGIN: &str = "wamn_transitions_executor_login";

#[sqlx::test(migrations = false)]
async fn run_state_live(pool: sqlx::PgPool) -> anyhow::Result<()> {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog = wamn_catalog::CATALOG_SCHEMA_SQL;
    let app_schema = wamn_control_provision::APP_SCHEMA_SQL;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .expect("read run-state DDL");
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .expect("read run-queue DDL");

    // The class-grant leg below shows the DDL's guest ACL through a prepared
    // App generation. Its production builder also converges the stable
    // `wamn_app` carrier to NOLOGIN/NOINHERIT/passwordless, so a drifted cluster
    // role cannot make the leg pass through ambient authority.
    //
    // `wamn_executor_platform` block mirrors `ensure_workload_acl_role_sql` in
    // crates/control/provision/src/sql.rs. Both carry the production attributes exactly:
    // NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS.
    // ANY CHANGE TO EITHER PRODUCTION ATTRIBUTE SET UPDATES THIS BLOCK IN THE SAME COMMIT.
    // The mirrored surface is the attribute list and nothing else. The `pg_roles`
    // assertion after this bootstrap is what holds the mirror to its word.
    //
    // REOPEN TRIGGER: a SECOND consumer needing this attribute set. Minting a shared
    // cross-workspace home crate for one reader was rejected as infrastructure built for
    // a single caller; a second reader is when that crate earns its existence.
    //
    // THE EXECUTOR'S RUN-PLANE GRANTS ARE THE PRODUCTION BUILDER'S NOW (`wamn-0h0g.22.31`).
    // The union grant this block carried was test-only because the cutover was unbuilt:
    // deploy/sql/run-state.sql withdrew `wamn_app`'s write surface at `5a8645d3` and
    // deferred the replacement to "their owning cutovers". `wamn-0h0g.22.37` built it, so
    // the standing instruction here — WHEN THE EXECUTOR CUTOVER LANDS, THIS BLOCK IS
    // REPLACED BY THAT BUILDER — is discharged: every fenced transition below now runs on
    // exactly `sql::grant_executor_platform_surface_sql`, and a leg that reaches its
    // semantics reaches them on what provisioning emits. What is NOT builder-owned is the
    // membership: `runs_platform` is the only policy an executor session matches, and it
    // is `TO wamn_platform`.
    //
    // `wamn_authority.current_project_key()` WENT WITH THE UNION AND IS NOT RESTORED. The
    // surface grants `project_key(text)` — load bearing, because `runs_tkey` is an
    // expression index over it and every status write maintains that index — and withholds
    // `current_project_key()`, because the base policy that calls it is narrowed
    // `TO wamn_app` and this family only ever matches the permissive `TO wamn_platform`
    // arm. If a leg below ever needs it, that is a real widening to argue, not a fixture
    // gap to patch.
    //
    // BOTH GENERATIONS are dropped before they are minted. Roles are
    // CLUSTER-wide, so a login left behind by an earlier run could let a
    // mutated builder pass on stale state. `DROP OWNED BY` first, because a role
    // holding any grant cannot be dropped.
    let database = success(&url, "SELECT current_database();")
        .trim()
        .to_string();
    let app_login = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: "t1",
            database: &database,
        },
    )
    .expect("derive the transitions App generation");
    let app_generation = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        &database,
        &app_login,
        "transitions-app-test-password",
    );
    let executor_generation = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        &database,
        EXECUTOR_LOGIN,
        "transitions-test-password",
    );
    success(
        &url,
        &format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{app_login}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{app_login}'); \
                 EXECUTE format('DROP ROLE %I', '{app_login}'); \
               END IF; \
                \
               IF NOT EXISTS \
                 (SELECT FROM pg_roles WHERE rolname = 'wamn_executor_platform') THEN \
                 CREATE ROLE wamn_executor_platform NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{EXECUTOR_LOGIN}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{EXECUTOR_LOGIN}'); \
                 EXECUTE format('DROP ROLE %I', '{EXECUTOR_LOGIN}'); \
               END IF; \
             END $$; \
             {app_generation} \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             {catalog} {run_state} {run_queue} {app_schema} \
             INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( 't1','cat','1.0.0', \
               'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); \
             INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) \
             VALUES ('t1','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a','{{}}'); \
             GRANT wamn_platform TO wamn_executor_platform \
               WITH INHERIT TRUE, SET FALSE, ADMIN FALSE; \
             {executor_generation}"
        ),
    );

    let mut admin = wamn_postgres::connect(&url).await?;
    let mut worker = wamn_postgres::connect(&url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE {EXECUTOR_LOGIN}; SET search_path TO wamn_run"
    )))
    .execute(&mut worker)
    .await?;
    let actor = wamn_identity_protocol::PlatformComponent::Executor
        .principal_id()
        .to_string();
    for client in [&mut admin, &mut worker] {
        sqlx::query("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:run-test', false), set_config('app.project_env_key', 't1', false)")
            .bind(&actor).execute(client).await?;
    }
    let digest = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
    let input = serde_json::json!({"order": 7});
    let nodes = serde_json::json!({"read": digest});
    let run = RunAdmission {
        project_env_key: "t1",
        application: "cat",
        manifest_digest: digest,
        environment: "prod",
        workflow_id: "workflow",
        workflow_version: 1,
        wiring_hash: digest,
        execution_principal_id: &actor,
        execution_role: "admin",
        trigger: RunTrigger::Manual,
        invoked_by: "manual",
        invoked_by_user_id: Some(&actor),
        node_digests: &nodes,
        idempotency_key: "manual:7",
        input: &input,
        source_run: None,
        root_run: None,
        depth: None,
    };
    let mut tx = worker.begin().await?;
    let admitted = admission::admit_run(&mut tx, &run).await?;
    let RunAdmitted::Queued { run_id } = &admitted else {
        panic!("run was not queued");
    };
    assert_eq!(admission::admit_run(&mut tx, &run).await?, admitted);
    let different_input = serde_json::json!({"order": 8});
    let conflict = RunAdmission {
        input: &different_input,
        ..run
    };
    assert_eq!(
        admission::admit_run(&mut tx, &conflict).await?,
        RunAdmitted::Conflict
    );
    tx.commit().await?;
    let applications = ["cat".to_owned()];
    for invalid in [
        Vec::new(),
        vec![String::new()],
        vec![" cat".to_owned()],
        vec!["cat\0".to_owned()],
    ] {
        let mut tx = worker.begin().await?;
        let error =
            persistence::lease_next(&mut tx, "worker", &invalid, "prod", 30000, Some(digest))
                .await
                .unwrap_err();
        assert_eq!(
            error.error_type(),
            wamn_run_state::run_store::ProductionLeaseErrorType::Contract
        );
        tx.rollback().await?;
    }

    // Admission and its queue entry roll back together.
    let mut tx = worker.begin().await?;
    let aborted = RunAdmission {
        idempotency_key: "aborted",
        ..conflict
    };
    let queued = admission::admit_run(&mut tx, &aborted).await?;
    let RunAdmitted::Queued { run_id: aborted_id } = queued else {
        panic!("run was not queued");
    };
    tx.rollback().await?;
    let persisted: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM wamn_run.runs WHERE run_id=$1) OR EXISTS (SELECT 1 FROM wamn_run.run_queue WHERE run_id=$1)")
        .bind(&aborted_id).fetch_one(&mut admin).await?.get(0);
    assert!(!persisted);

    let mut tx = worker.begin().await?;
    assert!(matches!(
        persistence::lease_next(
            &mut tx,
            "worker",
            &applications,
            "prod",
            30000,
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        )
        .await?,
        LeaseTurn::Leased(ProductionLeaseResult::Empty)
    ));
    tx.commit().await?;
    let mut tx = worker.begin().await?;
    let LeaseTurn::Leased(ProductionLeaseResult::Ready {
        lease_generation: first,
        workflow,
        ..
    }) = persistence::lease_next(
        &mut tx,
        "worker",
        &applications,
        "prod",
        30000,
        Some(digest),
    )
    .await?
    else {
        panic!("run was not leased");
    };
    assert_eq!(
        workflow.node_digests.get("read").map(String::as_str),
        Some(digest)
    );
    tx.commit().await?;

    // Expiry clears the abandoned state and advances its generation and attempts.
    sqlx::query("UPDATE wamn_run.runs SET state_json = '{\"partial\":true}' WHERE run_id=$1")
        .bind(run_id)
        .execute(&mut admin)
        .await?;
    sqlx::query("UPDATE wamn_run.run_queue SET lease_expires_at = now() - interval '1 minute' WHERE run_id=$1")
        .bind(run_id).execute(&mut admin).await?;
    let mut tx = worker.begin().await?;
    let LeaseTurn::Leased(ProductionLeaseResult::Ready {
        lease_generation: second,
        ..
    }) = persistence::lease_next(
        &mut tx,
        "next-worker",
        &applications,
        "prod",
        30000,
        Some(digest),
    )
    .await?
    else {
        panic!("expired run was not recovered");
    };
    assert!(second > first);
    // A stale settlement waiting on the new lease must see its new generation.
    let mut stale = wamn_postgres::connect(&url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE {EXECUTOR_LOGIN}; SET search_path TO wamn_run"
    )))
    .execute(&mut stale)
    .await?;
    sqlx::query("SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:run-test',false), set_config('app.project_env_key','t1',false)")
        .bind(&actor).execute(&mut stale).await?;
    let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut stale)
        .await?
        .get(0);
    let stale_run = run_id.to_owned();
    let waiting = tokio::spawn(async move {
        let mut tx = stale.begin().await?;
        let result = persistence::complete(
            &mut tx,
            &stale_run,
            "worker",
            first,
            &ProductionCompletion::completed(serde_json::Value::Null),
        )
        .await?;
        tx.commit().await?;

        Ok::<_, anyhow::Error>(result)
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')")
                .bind(pid).fetch_one(&mut admin).await?.get(0);
            if blocked { return Ok::<_, sqlx::Error>(()); }
            tokio::task::yield_now().await;
        }
    }).await??;

    tx.commit().await?;
    assert_eq!(waiting.await??, ProductionCompletionResult::FenceLost);

    let row = sqlx::query("SELECT r.state_json IS NULL, q.attempts FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (project_env_key,run_id) WHERE r.run_id=$1")
        .bind(run_id).fetch_one(&mut admin).await?;
    assert!(row.get::<bool, _>(0));
    assert_eq!(row.get::<i32, _>(1), 1);
    let mut tx = worker.begin().await?;
    assert_eq!(
        persistence::renew(&mut tx, run_id, "worker", first, 30000).await?,
        ProductionLeaseRenewal::FenceLost
    );
    assert_eq!(
        persistence::complete(
            &mut tx,
            run_id,
            "next-worker",
            second,
            &ProductionCompletion::completed(input.clone())
        )
        .await?,
        ProductionCompletionResult::Terminalized
    );
    tx.commit().await?;
    let queued: bool =
        sqlx::query("SELECT EXISTS (SELECT 1 FROM wamn_run.run_queue WHERE run_id=$1)")
            .bind(run_id)
            .fetch_one(&mut admin)
            .await?
            .get(0);
    assert!(!queued);

    // Use another installed release so a missing pin guard cannot hide behind its foreign key.
    sqlx::raw_sql("INSERT INTO catalog.releases (project_env_key,manifest_digest,canonical_bytes) VALUES ('t1','sha256:'||encode(sha256('[]'::bytea),'hex'),'[]'::bytea)")
        .execute(&mut admin).await?;
    for assignment in [
        "package_id='other'",
        "environment='other'",
        "wiring_id='other'",
        "wiring_version=2",
        "wiring_hash='sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
        "manifest_digest=(SELECT manifest_digest FROM catalog.releases WHERE canonical_bytes='[]'::bytea)",
    ] {
        let mut tx = admin.begin().await?;
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE wamn_run.runs SET {assignment} WHERE run_id=$1"
            )))
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .is_err()
        );
        tx.rollback().await?;
    }

    // The persisted vocabulary accepts each public status and failure class.
    for status in wamn_run_state::RunStatus::ALL {
        let mut tx = admin.begin().await?;
        sqlx::query("UPDATE wamn_run.runs SET status=$2 WHERE run_id=$1")
            .bind(run_id)
            .bind(status.as_sql())
            .execute(&mut *tx)
            .await?;
        tx.rollback().await?;
    }
    for failure in wamn_run_state::FailType::ALL {
        let mut tx = admin.begin().await?;
        sqlx::query("UPDATE wamn_run.runs SET fail_type=$2 WHERE run_id=$1")
            .bind(run_id)
            .bind(failure.as_sql())
            .execute(&mut *tx)
            .await?;
        tx.rollback().await?;
    }
    for (id, version, hash) in [
        ("", 1, digest),
        ("workflow", 0, digest),
        ("workflow", 1, "malformed"),
    ] {
        let invalid = RunAdmission {
            workflow_id: id,
            workflow_version: version,
            wiring_hash: hash,
            idempotency_key: "invalid-workflow",
            ..aborted
        };
        let mut tx = worker.begin().await?;
        assert!(admission::admit_run(&mut tx, &invalid).await.is_err());
        tx.rollback().await?;
    }
    // The schema refuses invalid persisted values and incomplete event ancestry.
    for (statement, value) in [
        (
            "UPDATE wamn_run.runs SET status=$2 WHERE run_id=$1",
            "parked",
        ),
        (
            "UPDATE wamn_run.runs SET fail_type=$2 WHERE run_id=$1",
            "unknown",
        ),
        (
            "UPDATE wamn_run.runs SET invocation_context=$2::text::jsonb WHERE run_id=$1",
            "[]",
        ),
    ] {
        let mut tx = admin.begin().await?;
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
                .bind(run_id)
                .bind(value)
                .execute(&mut *tx)
                .await
                .is_err()
        );
        tx.rollback().await?;
    }
    let too_large = serde_json::json!({"value": "x".repeat(16384)}).to_string();
    let mut tx = admin.begin().await?;
    assert!(
        sqlx::query("UPDATE wamn_run.runs SET invocation_context=$2::text::jsonb WHERE run_id=$1")
            .bind(run_id)
            .bind(too_large)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await?;
    let empty = serde_json::Value::Null;
    for (parent, root, depth) in [
        (None, Some("root"), Some(1)),
        (Some("parent"), None, Some(1)),
        (Some("parent"), Some("root"), None),
        (Some("parent"), Some("root"), Some(17)),
        (Some("parent"), Some("root"), Some(0)),
    ] {
        let invalid = RunAdmission {
            idempotency_key: "invalid-event",
            trigger: RunTrigger::Event,
            invoked_by: "event:7",
            invoked_by_user_id: None,
            input: &empty,
            source_run: parent,
            root_run: root,
            depth,
            ..aborted
        };
        let mut tx = worker.begin().await?;
        assert!(admission::admit_run(&mut tx, &invalid).await.is_err());
        tx.rollback().await?;
    }
    let lineage = RunAdmission {
        idempotency_key: "lineage",
        trigger: RunTrigger::Event,
        invoked_by: "event:7",
        invoked_by_user_id: None,
        input: &empty,
        source_run: Some("parent"),
        root_run: Some("root"),
        depth: Some(1),
        ..aborted
    };
    let mut tx = worker.begin().await?;
    let RunAdmitted::Queued { run_id: lineage_id } =
        admission::admit_run(&mut tx, &lineage).await?
    else {
        panic!("event was not queued");
    };
    tx.commit().await?;
    let mut tx = admin.begin().await?;
    assert!(
        sqlx::query("UPDATE wamn_run.runs SET event_depth=2 WHERE run_id=$1")
            .bind(&lineage_id)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await?;
    let mut tx = admin.begin().await?;
    assert!(
        sqlx::query("DELETE FROM wamn_run.runs WHERE run_id=$1")
            .bind(&lineage_id)
            .execute(&mut *tx)
            .await
            .is_err()
    );
    tx.rollback().await?;
    // Run authority cannot be used to settle another run.
    let mut tx = worker.begin().await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(
        wamn_run_state::transitions::terminalize_sql(),
    ))
    .bind(run_id)
    .bind(&lineage_id)
    .bind("worker")
    .bind(1_i64)
    .bind(wamn_run_state::RunStatus::Completed.as_sql())
    .bind("router-completed")
    .bind("{}")
    .bind(None::<String>)
    .bind(chrono::Utc::now())
    .fetch_one(&mut *tx)
    .await?;
    let code: String = row.get(0);
    let status: Option<String> = row.get(1);
    assert_eq!(
        wamn_run_state::transitions::TerminalizeResult::from_parts(
            &code,
            status.as_deref().unwrap_or_default()
        ),
        Some(wamn_run_state::transitions::TerminalizeResult::CrossRunAuthority)
    );
    tx.rollback().await?;

    let input = serde_json::Value::Null;
    let exhausted = RunAdmission {
        idempotency_key: "exhausted",
        input: &input,
        ..aborted
    };
    let mut tx = worker.begin().await?;
    let RunAdmitted::Queued {
        run_id: exhausted_id,
    } = admission::admit_run(&mut tx, &exhausted).await?
    else {
        panic!("run was not queued");
    };
    tx.commit().await?;
    sqlx::query("UPDATE wamn_run.run_queue SET lease_owner='dead', lease_expires_at=now()-interval '1 minute', attempts=max_attempts WHERE run_id=$1")
        .bind(&exhausted_id).execute(&mut admin).await?;
    let mut tx = worker.begin().await?;
    assert_eq!(
        persistence::reap_exhausted(&mut tx, &applications, "prod", 0).await?,
        ProductionReapResult::Reaped {
            run_id: exhausted_id
        }
    );
    tx.commit().await?;

    sqlx::query("UPDATE wamn_run.runs SET created_at = created_at - interval '31 days'")
        .execute(&mut admin)
        .await?;
    assert_eq!(
        persistence::prune_terminal_runs(&mut admin, "t1", 30).await?,
        2
    );
    let retained: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM wamn_run.runs WHERE run_id=$1)")
        .bind(&lineage_id)
        .fetch_one(&mut admin)
        .await?
        .get(0);
    assert!(retained);
    Ok(())
}
