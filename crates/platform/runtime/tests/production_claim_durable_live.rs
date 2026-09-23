//! THE SHELVED CRASH FLOOR, live on the PREMIUM `durable` tier.
//!
//! wamn-0h0g.20.4. Everything here is reachable only when
//! `DurabilityClass::admits_effect_evidence()` is true
//! (`crates/execution/run-state/src/durability.rs`): the eligibility
//! predicate's effect disjunct, the claim-time advisory fence, the
//! `ExpiredWithAttempt` classification and the `Terminalized` /
//! `EffectAttempt` results it produces. wamn-0h0g.20.2 made every one of those
//! unreachable on the class every run carries by default — UNREACHABLE, NOT
//! DELETED — so the tests survive verbatim, on the class that pays for them.
//!
//! EVERY RUN THIS FILE SEEDS IS `durable`, AT ADMISSION. The class is an
//! admission pin: `wamn_run.guard_run_admission_pins_immutable` names
//! `durability_class` in its trigger column list and refuses a post-admission
//! change as `run-admission-pin-immutable`. Seeding `standard` and promoting
//! later would be a fixture that production forbids.
//!
//! The queue itself — FIFO, SKIP LOCKED, the lease grant, the pre-effect
//! reclaim, crash-evidence accounting, the janitor, the release record,
//! park/wake and dequeue — is shown on the DEFAULT tier in
//! `production_claim_live.rs`, and none of it is duplicated here.

use std::sync::Arc;

use serde_json::{Value, json};
use wamn_run_state::queue::serialize_effect_intent_sql;
use wamn_run_state::{FailKind, RunStatus, RunStore as _};
use wamn_runtime::plugins::wamn_postgres::{ProductionClaimResult, ProductionReapResult};

mod common;

use common::{
    COMPONENT, EMPTY_HASH, ENVIRONMENT, PACKAGE_ID, POD_EFFECTIVE_RELEASE_ID, POD_MANIFEST_DIGEST,
    RUNTIME_APPLICATION_NAME, SCHEMA, TENANT, WIRING_ID, WIRING_VERSION,
    assert_callerless_terminal, assert_prior_winner_terminal, connect, expire_effect_run,
    insert_effect_attempt, install_fixture, install_prior_caller_winner, make_callerless,
    ready_run, release_record, seed_durable_run, seed_live_effect_run, teardown,
    wait_for_advisory_wait,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_claim_durable_live() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_catalog::test_database::tenant();
    let fixture = install_fixture(database.url()).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let release_package_ids = [PACKAGE_ID.to_owned(), "cat_overlay".to_owned()];

    // A transaction that took the effect-intent fence and recorded an attempt
    // while the lease was live may commit after the lease expires. The reaper
    // holds the row lock, waits on the same tenant/run fence, then uses a fresh
    // snapshot and must observe the attempt. The fence is class-gated
    // (wamn-0h0g.20.2), so the run is admitted `durable`. No effect writer
    // exists (wamn-0h0g.10.15), so the fixture superuser takes the production
    // fence statement and records the attempt.
    //
    // THE MIRROR OF THIS ROW IS `standard-effect` IN `production_claim_live.rs`:
    // same shape — crash budget spent, lease expired, one attributed attempt —
    // and both results flip on the default class, where the claim never reaches
    // the row (`Empty`) and the reaper never defers (`Reaped`).
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,package_id,effective_release_id, \
                    environment,wiring_id,wiring_version,input_json,trigger_source, \
                    durability_class) \
                 VALUES ($1,'effect-race','root',1,'running',$2,1,'test',$3,$4, \
                         '{{\"input\":true}}','http','durable')"
            ),
            &[&TENANT, &PACKAGE_ID, &WIRING_ID, &WIRING_VERSION],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    attempts,max_attempts) \
                 VALUES ($1,'effect-race','2000-01-01',50,'runner-a','2099-01-01',2,3)"
            ),
            &[&TENANT],
        )
        .await?;
    let mut writer = connect(database.url()).await?;
    let intent = writer.transaction().await?;
    intent
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
        .await?;
    intent
        .query_one(&serialize_effect_intent_sql(), &[&"effect-race"])
        .await?;
    insert_effect_attempt(&intent, "effect-race", "effect-node").await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_expires_at='2000-01-01', attempts=max_attempts \
                  WHERE tenant_id=$1 AND run_id='effect-race'"
            ),
            &[&TENANT],
        )
        .await?;
    let reaper = {
        let plugin = Arc::clone(plugin);
        let release_package_ids = release_package_ids.clone();
        tokio::spawn(async move {
            plugin
                .reap_exhausted(COMPONENT, &release_package_ids, ENVIRONMENT, 0)
                .await
        })
    };
    wait_for_advisory_wait(admin, Some(RUNTIME_APPLICATION_NAME), None).await?;
    intent.commit().await?;
    drop(writer);
    assert_eq!(
        reaper.await??,
        ProductionReapResult::EffectAttempt {
            run_id: "effect-race".into()
        }
    );
    assert_eq!(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-race".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    let effect = admin
        .query_one(
            &format!(
                "SELECT caller_outcome_json::text,caller_http_status,caller_outcome_hash, \
                        EXISTS (SELECT 1 FROM {SCHEMA}.run_queue q \
                                 WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE tenant_id=$1 AND run_id='effect-race'"
            ),
            &[&TENANT],
        )
        .await?;
    let effect_body = json!({"code": "effect-uncertain", "run_id": "effect-race"});
    assert_eq!(
        serde_json::from_str::<Value>(&effect.get::<_, String>(0))?,
        effect_body
    );
    assert_eq!(effect.get::<_, i32>(1), 500);
    assert_eq!(
        effect.get::<_, String>(2),
        wamn_execution_contract::canonical_json_sha256(&effect_body)
    );
    assert!(!effect.get::<_, bool>(3));

    seed_live_effect_run(admin, "effect-callerless", 51).await?;
    make_callerless(admin, "effect-callerless").await?;
    insert_effect_attempt(admin, "effect-callerless", "effect-node").await?;
    expire_effect_run(admin, "effect-callerless").await?;
    assert_eq!(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-callerless".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    assert_callerless_terminal(admin, "effect-callerless", "effect-uncertain").await?;

    seed_live_effect_run(admin, "effect-winner", 52).await?;
    let effect_winner = install_prior_caller_winner(admin, "effect-winner").await?;
    insert_effect_attempt(admin, "effect-winner", "effect-node").await?;
    expire_effect_run(admin, "effect-winner").await?;
    assert_eq!(
        plugin
            .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
            .await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-winner".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    assert_prior_winner_terminal(admin, "effect-winner", "effect-uncertain", &effect_winner)
        .await?;

    // AN ATTRIBUTED EFFECT PINS THE RELEASE THAT FIRED IT (wamn-0h0g.15.11,
    // class-gated by wamn-0h0g.20.2). The attempt names the release that fired
    // it, and that link is never rewritten out from under it — but only on the
    // class whose claim path may act on the attempt. `park_sql` and
    // `guard_run_admission_pins_immutable` carry the identical
    // `durability_class = 'durable'` conjunct inside the `EXISTS`, and they must
    // move together: gate one and not the other and the run plane breaks
    // (`crates/execution/run-state/src/queue/sql.rs`, `park_sql`).
    //
    // The claim is what records the pair, so the run is claimed first.
    seed_durable_run(admin, "effect-pin", PACKAGE_ID, 80).await?;
    assert_eq!(
        ready_run(
            plugin
                .claim_next(COMPONENT, &release_package_ids, ENVIRONMENT, 30_000)
                .await?
        ),
        "effect-pin"
    );
    let recorded = (
        POD_EFFECTIVE_RELEASE_ID,
        Some(POD_MANIFEST_DIGEST.to_string()),
    );
    assert_eq!(release_record(admin, "effect-pin").await?, recorded);
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                    generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                 VALUES ($1,'effect-pin',$2,$2,0,'a-node',$2,'manager',0,1, \
                         'not-required','2099-01-01T00:00:00Z','sha256:claim-live-effect-input')"
            ),
            &[&TENANT, &EMPTY_HASH],
        )
        .await?;
    let mid_effect = admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET manifest_digest=NULL \
                  WHERE tenant_id=$1 AND run_id='effect-pin'"
            ),
            &[&TENANT],
        )
        .await
        .expect_err("an attributed effect pins the release that fired it");
    assert_eq!(
        mid_effect
            .as_db_error()
            .expect("guard refusal is a db error")
            .message(),
        "run-release-record-immutable"
    );
    assert_eq!(release_record(admin, "effect-pin").await?, recorded);

    teardown(fixture).await
}
