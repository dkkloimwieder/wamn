//! Live tests of standard queue claims, leases, and exhausted-run reaping.
//!
//! The fixture includes an effect attempt to show that claims and reaping
//! ignore it. FIFO order, lease recovery, crash accounting, caller outcomes,
//! and release pins retain their standard behavior.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use wamn_run_state::{RunStore as _, queue::select_production_claim_sql};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimErrorType, ProductionClaimResult, ProductionReapResult,
};

mod common;

use common::{
    COMPONENT, EMPTY_HASH, ENV_KEY, ENVIRONMENT, PACKAGE_ID, POD_MANIFEST_DIGEST, ROLLED_COMPONENT,
    SCHEMA, WIRING_ID, WIRING_VERSION, assert_callerless_terminal, assert_prior_winner_terminal,
    assert_terminal_status_dequeued, connect, expire_effect_run, install_fixture,
    install_prior_caller_winner, make_callerless, queue_attempts, quote_literal, ready_run,
    release_record, seed_exhausted_run, seed_run, teardown,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_claim_live() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let url = database.url();
    let fixture = install_fixture(url).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let release_package_ids = [PACKAGE_ID.to_owned(), "cat_overlay".to_owned()];

    // Exact release-wide global FIFO: stream sequence, then run id, breaks equal
    // timestamps across package boundaries. An earlier foreign-package row does
    // not enter the mounted release's one SQL turn.
    seed_run(admin, "fifo-foreign", "cat_foreign", 0).await?;
    for (run_id, package_id, stream_seq) in [
        ("fifo-c", PACKAGE_ID, 2),
        ("fifo-b", "cat_overlay", 1),
        ("fifo-a", PACKAGE_ID, 1),
    ] {
        seed_run(admin, run_id, package_id, stream_seq).await?;
    }
    let mut fifo = Vec::new();
    for _ in 0..3 {
        let result = plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?;
        let ProductionClaimResult::Ready {
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

    // A second claimer skips the exact FIFO head while the first claimer holds
    // its production row locks, then the rolled-back head remains claimable.
    seed_run(admin, "double-a", "cat_main", 10).await?;
    seed_run(admin, "double-b", "cat_main", 11).await?;
    let first_claimer = connect(url).await?;
    first_claimer
        .batch_execute(&format!(
            "BEGIN; SET LOCAL search_path TO {SCHEMA}, pg_catalog"
        ))
        .await?;
    first_claimer
        .execute("SELECT set_config('app.env_key', $1, true)", &[&ENV_KEY])
        .await?;
    let locked = first_claimer
        .query_one(
            &select_production_claim_sql(),
            &[
                &release_package_ids.as_slice(),
                &ENVIRONMENT,
                &Some(POD_MANIFEST_DIGEST),
            ],
        )
        .await?;
    assert_eq!(locked.get::<_, String>(0), "double-a");
    let skipped = ready_run(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
    );
    assert_eq!(skipped, "double-b");
    first_claimer.batch_execute("ROLLBACK").await?;
    let released = ready_run(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
    );
    let claimed = BTreeSet::from([skipped, released]);
    assert_eq!(
        claimed,
        BTreeSet::from(["double-a".into(), "double-b".into()])
    );

    // The effect-free exhausted path computes the generic outcome and hash in
    // the host, atomically releases an attached caller, and dequeues.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (env_key,run_id,flow_id,flow_version,status,package_id,manifest_digest, \
                    environment,wiring_id,wiring_version,trigger_source) \
                 VALUES ($1,'janitor','root',1,'running','cat_main','{POD_MANIFEST_DIGEST}', \
                         'test',$2,$3,'http')"
            ),
            &[&ENV_KEY, &WIRING_ID, &WIRING_VERSION],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (env_key,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    attempts,max_attempts) \
                 VALUES ($1,'janitor','2000-01-01',60,'dead','2000-01-01',3,3)"
            ),
            &[&ENV_KEY],
        )
        .await?;
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "janitor".into()
        }
    );
    let janitor = admin
        .query_one(
            &format!(
                "SELECT status,caller_outcome_json::text,caller_http_status,caller_release_node_id, \
                        caller_outcome_hash,caller_released_at IS NOT NULL, \
                        EXISTS (SELECT 1 FROM {SCHEMA}.run_queue q \
                                 WHERE q.env_key=r.env_key AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE env_key=$1 AND run_id='janitor'"
            ),
            &[&ENV_KEY],
        )
        .await?;
    let janitor_body = json!({"error": {
        "code": "infrastructure-failure", "flow-id": "root", "flow-version": 1,
        "run-id": "janitor"
    }});
    assert_eq!(janitor.get::<_, String>(0), "infrastructure-failure");
    assert_eq!(
        serde_json::from_str::<Value>(&janitor.get::<_, String>(1))?,
        janitor_body
    );
    assert_eq!(janitor.get::<_, i32>(2), 500);
    assert_eq!(janitor.get::<_, Option<String>>(3), None);
    assert_eq!(
        janitor.get::<_, String>(4),
        wamn_execution_contract::canonical_json_sha256(&janitor_body)
    );
    assert!(janitor.get::<_, bool>(5));
    assert!(!janitor.get::<_, bool>(6));

    seed_exhausted_run(admin, "janitor-callerless", 61).await?;
    make_callerless(admin, "janitor-callerless").await?;
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "janitor-callerless".into()
        }
    );
    assert_callerless_terminal(admin, "janitor-callerless", "infrastructure-failure").await?;

    seed_exhausted_run(admin, "janitor-winner", 62).await?;
    let janitor_winner = install_prior_caller_winner(admin, "janitor-winner").await?;
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "janitor-winner".into()
        }
    );
    assert_prior_winner_terminal(
        admin,
        "janitor-winner",
        "infrastructure-failure",
        &janitor_winner,
    )
    .await?;

    // ---- a refused grant must not starve its own janitor (wamn-0h0g.15.69) --
    //
    // `attempts` used to ride the same statement that grants the lease, so any
    // refusal of that statement rolled the increment back with it: the run
    // could never reach `max_attempts`, the janitor could never reap it, and
    // nothing locked it after rollback — one run head-of-line-blocked the
    // tenant forever. The advance now runs before the grant's subtransaction,
    // so a refusal still counts as crash evidence. A probe trigger stands in
    // for any database guard that can refuse the grant.
    seed_run(admin, "grant-refused", "cat_main", 65).await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=2, max_attempts=3 \
                  WHERE env_key=$1 AND run_id='grant-refused'"
            ),
            &[&ENV_KEY],
        )
        .await?;
    admin
        .batch_execute(&format!(
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
        .await?;
    let starved = plugin
        .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
        .await
        .expect_err("the probed grant refuses");
    assert_eq!(starved.error_type(), ProductionClaimErrorType::Storage);
    assert_eq!(starved.operation(), "grant production lease");
    assert!(
        starved.to_string().contains("probe-grant-refused"),
        "unexpected refusal: {starved}"
    );
    assert_eq!(
        queue_attempts(admin, "grant-refused").await?,
        3,
        "the refused claim rolled its own crash evidence back"
    );
    admin
        .batch_execute(&format!(
            "DROP TRIGGER refuse_probed_grant ON {SCHEMA}.runs; \
             DROP FUNCTION {SCHEMA}.refuse_probed_grant();"
        ))
        .await?;
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

    // An effect attempt does not bypass the crash budget or prevent reaping.
    seed_exhausted_run(admin, "standard-effect", 66).await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (env_key,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                    generation_fact_type,attempt_deadline_at,attempt_input_ref) \
                 VALUES ($1,'standard-effect',$2,$2,0,'effect-node',$2,'manager',0,1, \
                         'not-required','2099-01-01T00:00:00Z','sha256:claim-live-effect-input')"
            ),
            &[&ENV_KEY, &EMPTY_HASH],
        )
        .await?;
    assert_eq!(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionClaimResult::Empty,
        "an effect record bypassed the standard crash budget"
    );
    assert_eq!(
        plugin
            .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Reaped {
            run_id: "standard-effect".into()
        },
        "the janitor deferred to effect evidence the default class may not act on"
    );
    assert_terminal_status_dequeued(admin, "standard-effect", "infrastructure-failure").await?;

    // ---- the admission pin (platform-deploy.md R1) ------------------------
    //
    // Admission pins the release by its manifest digest. A matching pod claims
    // the run and never writes the pin.
    seed_run(admin, "release-record", PACKAGE_ID, 70).await?;
    let recorded = Some(POD_MANIFEST_DIGEST.to_string());
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    assert_eq!(
        ready_run(
            plugin
                .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // SAME-RELEASE RE-CLAIM. The classifier's pre-effect reclaim keeps the pin.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        ready_run(
            plugin
                .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // A release is an admission pin, not a claim-time rollout slot. A pod
    // carrying another release does not select this run at all (R20): the
    // candidate select filters by the pod's release, and the grant keeps the
    // recheck.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        plugin
            .claim_next(ROLLED_COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionClaimResult::Empty,
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
                .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // No path rewrites or erases the pin.
    for assignment in [quote_literal(EMPTY_HASH), "NULL".to_owned()] {
        let rewritten = admin
            .execute(
                &format!(
                    "UPDATE {SCHEMA}.runs SET manifest_digest={assignment} \
                      WHERE env_key=$1 AND run_id='release-record'"
                ),
                &[&ENV_KEY],
            )
            .await
            .expect_err("no path may rewrite an admitted release in place");
        let db = rewritten
            .as_db_error()
            .expect("guard refusal is a db error");
        assert_eq!(db.code().code(), "55000");
        assert_eq!(db.message(), "run-admission-pin-immutable");
    }
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    admin
        .execute(
            &format!(
                "DELETE FROM {SCHEMA}.run_queue \
                  WHERE env_key=$1 AND run_id='release-record'"
            ),
            &[&ENV_KEY],
        )
        .await?;

    teardown(fixture).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_writers_stamp_existing_executor_claims() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let fixture = install_fixture(database.url()).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    // Admission already binds the executor in its existing transaction.
    admin.execute("INSERT INTO wamn_run.environment_policies (env_key, expected_environment, durability_class) VALUES ($1, $2, 'standard')", &[&ENV_KEY, &ENVIRONMENT]).await?;
    let actor = wamn_project_state::PlatformComponent::Executor
        .principal_id()
        .to_string();
    let admitted = plugin
        .admit_manual_run(
            "default",
            ENV_KEY,
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
    admin
        .execute("DELETE FROM wamn_run.run_queue WHERE run_id=$1", &[&run_id])
        .await?;

    seed_run(admin, "history-claim", PACKAGE_ID, 0).await?;
    let claimed = ready_run(
        plugin
            .claim_next(COMPONENT, &[PACKAGE_ID.to_owned()], ENVIRONMENT, 30_000)
            .await?,
    );
    assert_eq!(claimed, "history-claim");
    wamn_test_infrastructure::record_history::assert_stamp(
        admin,
        "wamn_run.run_queue",
        "run_id='history-claim'",
        &wamn_project_state::PlatformComponent::Provisioning
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
    let database = wamn_catalog::test_database::tenant();
    let fixture = install_fixture(database.url()).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let packages = [PACKAGE_ID.to_owned()];
    for last_release in [true, false] {
        let run = if last_release {
            "last-release"
        } else {
            "last-expiry"
        };
        seed_run(admin, run, PACKAGE_ID, 1).await?;
        admin
            .execute(
                "UPDATE wamn_run.run_queue SET max_attempts=3 WHERE run_id=$1",
                &[&run],
            )
            .await?;
        let claim = plugin
            .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionClaimResult::Ready {
            lease_generation, ..
        } = claim
        else {
            panic!("first claim missing")
        };
        assert!(
            plugin
                .release_claim(COMPONENT, run, lease_generation)
                .await?
        );
        assert_eq!(queue_attempts(admin, run).await?, 1);
        admin
            .execute(
                "UPDATE wamn_run.run_queue SET available_at='2000-01-01' WHERE run_id=$1",
                &[&run],
            )
            .await?;
        let claim = plugin
            .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        assert_eq!(ready_run(claim), run);
        assert_eq!(
            queue_attempts(admin, run).await?,
            1,
            "a released claim must not count twice"
        );
        expire_effect_run(admin, run).await?;
        let claim = plugin
            .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionClaimResult::Ready {
            lease_generation, ..
        } = claim
        else {
            panic!("expired claim missing")
        };
        assert_eq!(
            queue_attempts(admin, run).await?,
            2,
            "expiry shares the release count"
        );
        if last_release {
            assert!(
                plugin
                    .release_claim(COMPONENT, run, lease_generation)
                    .await?
            );
            assert_terminal_status_dequeued(admin, run, "failed").await?;
            let row = admin.query_one("SELECT fail_type,result_json::text,caller_outcome_json::text,terminal_reason FROM wamn_run.runs WHERE run_id=$1", &[&run]).await?;
            assert_eq!(row.get::<_, String>(0), "retry-exhausted");
            assert_eq!(row.get::<_, String>(3), "retry-exhausted");
            let result: Value = serde_json::from_str(&row.get::<_, String>(1))?;
            assert_eq!(result["error"]["code"], "retry-exhausted");
            assert_eq!(
                result,
                serde_json::from_str::<Value>(&row.get::<_, String>(2))?
            );
        } else {
            expire_effect_run(admin, run).await?;
            assert_eq!(
                ready_run(
                    plugin
                        .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
                        .await?
                ),
                run
            );
            assert_eq!(queue_attempts(admin, run).await?, 3);
            expire_effect_run(admin, run).await?;
            assert!(matches!(
                plugin
                    .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
                    .await?,
                ProductionClaimResult::Empty
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
            let body: String = admin
                .query_one(
                    "SELECT caller_outcome_json::text FROM wamn_run.runs WHERE run_id=$1 AND terminal_reason='infrastructure-failure'",
                    &[&run],
                )
                .await?
                .get(0);
            assert_eq!(
                serde_json::from_str::<Value>(&body)?["error"]["code"],
                "infrastructure-failure"
            );
        }
        assert!(
            !plugin
                .release_claim(COMPONENT, run, lease_generation)
                .await?,
            "a terminal run cannot be released again"
        );
    }
    teardown(fixture).await
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_release_fences_and_bounds_backoff() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let fixture = install_fixture(database.url()).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let packages = [PACKAGE_ID.to_owned()];
    for attempts in [0_i32, 1, 2, 8, 9, 18] {
        let run = format!("backoff-{attempts}");
        seed_run(admin, &run, PACKAGE_ID, i64::from(attempts)).await?;
        let claim = plugin
            .claim_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
            .await?;
        let ProductionClaimResult::Ready {
            lease_generation, ..
        } = claim
        else {
            panic!("claim missing")
        };
        admin
            .execute(
                "UPDATE wamn_run.run_queue SET attempts=$2 WHERE run_id=$1",
                &[&run, &attempts],
            )
            .await?;
        assert!(
            !plugin
                .release_claim(COMPONENT, &run, lease_generation + 1)
                .await?
        );
        assert!(
            !plugin
                .release_claim(ROLLED_COMPONENT, &run, lease_generation)
                .await?
        );
        assert_eq!(queue_attempts(admin, &run).await?, attempts);
        let before: std::time::SystemTime = admin
            .query_one("SELECT clock_timestamp()", &[])
            .await?
            .get(0);
        assert!(
            plugin
                .release_claim(COMPONENT, &run, lease_generation)
                .await?
        );
        let base = (1000_i64 << attempts).min(300_000);
        let upper = (base * 12 / 10).min(300_000);
        let row = admin
            .query_one(
                "SELECT attempts,lease_owner IS NULL AND lease_expires_at IS NULL, \
             available_at >= $2::timestamptz + $3::bigint * interval '1 millisecond' \
             AND available_at <= clock_timestamp() + $4::bigint * interval '1 millisecond' \
             FROM wamn_run.run_queue WHERE run_id=$1",
                &[&run, &before, &base, &upper],
            )
            .await?;
        assert_eq!(row.get::<_, i32>(0), attempts + 1);
        assert!(row.get::<_, bool>(1));
        assert!(
            row.get::<_, bool>(2),
            "attempt {attempts} delay outside jitter bounds"
        );
        assert!(
            !plugin
                .release_claim(COMPONENT, &run, lease_generation)
                .await?
        );
        admin
            .execute(
                "UPDATE wamn_run.run_queue SET available_at = 'infinity' WHERE run_id=$1",
                &[&run],
            )
            .await?;
    }
    teardown(fixture).await
}
