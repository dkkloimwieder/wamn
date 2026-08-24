//! THE SHELVED CRASH FLOOR, live on the PREMIUM `durable` tier.
//!
//! wamn-0h0g.20.4. Everything here is reachable only when
//! `DurabilityClass::admits_effect_evidence()` is true
//! (`crates/execution/run-state/src/durability.rs`): the eligibility
//! predicate's effect disjunct, the claim-time advisory fence, the
//! `ExpiredWithAttempt` classification and the `Terminalized` /
//! `EffectAttempt` results it produces. wamn-0h0g.20.2 made every one of those
//! unreachable on the class every run carries by default — UNREACHABLE, NOT
//! DELETED — so the proofs survive verbatim, on the class that pays for them.
//!
//! EVERY RUN THIS FILE SEEDS IS `durable`, AT ADMISSION. The class is an
//! admission pin: `wamn_run.guard_run_admission_pins_immutable` names
//! `durability_class` in its trigger column list and refuses a post-admission
//! change as `run-admission-pin-immutable`. Seeding `standard` and promoting
//! later would be a fixture that production forbids.
//!
//! The queue itself — FIFO, SKIP LOCKED, the lease grant, the pre-effect
//! reclaim, crash-evidence accounting, the janitor, the release record,
//! park/wake and dequeue — is proven on the DEFAULT tier in
//! `production_claim_live.rs`, and none of it is duplicated here.

use std::sync::Arc;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_run_state::{EffectAttempt, EffectWriterErrorKind, FailKind, RunStatus};
use wamn_runtime::plugins::wamn_postgres::{ProductionClaimResult, ProductionReapResult};

mod common;

use common::{
    CATALOG_ID, COMPONENT, EMPTY_HASH, ENVIRONMENT, POD_MANIFEST_DIGEST, POD_RELEASE_VERSION,
    RUNTIME_APPLICATION_NAME, SCHEMA, TENANT, WIRING_ID, WIRING_VERSION, WRITER_LATCH,
    assert_callerless_terminal, assert_prior_winner_terminal, effect_attempt, expire_effect_run,
    install_fixture, install_prior_caller_winner, make_callerless, ready_run, release_record,
    seed_durable_run, seed_live_effect_run, teardown, wait_for_advisory_wait,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a disposable PostgreSQL 18 URL in WAMN_DURABLE_TIER_PG_URL"]
async fn production_claim_durable_live() -> anyhow::Result<()> {
    let url = std::env::var("WAMN_DURABLE_TIER_PG_URL").context(
        "set WAMN_DURABLE_TIER_PG_URL to a disposable PostgreSQL database \
         (a DIFFERENT one from WAMN_PRODUCTION_CLAIM_PG_URL: both suites install \
         the same schema and drop it on teardown)",
    )?;
    let fixture = install_fixture(&url).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let writer = &fixture.writer;
    let writer_role = fixture.writer_role.clone();

    // A writer that fenced and validated while the lease was live may commit
    // after the lease expires. The reaper holds the row lock, waits on the same
    // tenant/run fence, then uses a fresh snapshot and must observe the attempt.
    // The fence is class-gated (wamn-0h0g.20.2), so the run is admitted
    // `durable`.
    //
    // THE MIRROR OF THIS ROW IS `standard-ledger` IN `production_claim_live.rs`:
    // same shape — crash budget spent, lease expired, one attributed attempt —
    // and both results flip on the default class, where the claim never reaches
    // the row (`Empty`) and the reaper never defers (`Reaped`).
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,wiring_id,wiring_version,trigger_source,durability_class) \
                 VALUES ($1,'effect-race','root',1,'running','cat-main',1,'test',$2,$3,'http', \
                         'durable')"
            ),
            &[&TENANT, &WIRING_ID, &WIRING_VERSION],
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
    admin
        .batch_execute(&format!(
            "CREATE FUNCTION {SCHEMA}.hold_effect_insert() RETURNS trigger \
               LANGUAGE plpgsql AS $hold$ BEGIN \
                 PERFORM pg_advisory_xact_lock({WRITER_LATCH}); RETURN NEW; \
               END $hold$; \
             CREATE TRIGGER hold_effect_insert BEFORE INSERT ON {SCHEMA}.effect_attempts \
               FOR EACH ROW EXECUTE FUNCTION {SCHEMA}.hold_effect_insert();"
        ))
        .await?;
    admin
        .query_one("SELECT pg_advisory_lock($1)", &[&WRITER_LATCH])
        .await?;
    let writer_task = {
        let writer = fixture.writer.clone();
        tokio::spawn(async move {
            writer
                .begin_attempt(effect_attempt("effect-race", "effect-node"))
                .await
        })
    };
    wait_for_advisory_wait(admin, None, Some(&writer_role)).await?;
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
        tokio::spawn(async move {
            plugin
                .reap_one_exhausted_production(COMPONENT, CATALOG_ID, ENVIRONMENT, 0)
                .await
        })
    };
    wait_for_advisory_wait(admin, Some(RUNTIME_APPLICATION_NAME), None).await?;
    let unlocked: bool = admin
        .query_one("SELECT pg_advisory_unlock($1)", &[&WRITER_LATCH])
        .await?
        .get(0);
    assert!(unlocked);
    let inserted_effect: EffectAttempt = writer_task.await??;
    assert_eq!(
        reaper.await??,
        ProductionReapResult::EffectAttempt {
            run_id: "effect-race".into()
        }
    );
    assert_eq!(
        plugin
            .claim_next_production(COMPONENT, CATALOG_ID, ENVIRONMENT, 30_000)
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
    assert_eq!(
        writer
            .begin_attempt(effect_attempt("effect-race", "effect-node"))
            .await?,
        inserted_effect,
        "an exact immutable retry survives later terminalization"
    );
    let inactive_new = writer
        .begin_attempt(effect_attempt("effect-race", "second-effect-node"))
        .await
        .expect_err("a new coordinate cannot begin after terminalization");
    assert_eq!(inactive_new.kind(), EffectWriterErrorKind::RunNotRunnable);

    seed_live_effect_run(admin, "effect-callerless", 51).await?;
    make_callerless(admin, "effect-callerless").await?;
    writer
        .begin_attempt(effect_attempt("effect-callerless", "effect-node"))
        .await?;
    expire_effect_run(admin, "effect-callerless").await?;
    assert_eq!(
        plugin
            .claim_next_production(COMPONENT, CATALOG_ID, ENVIRONMENT, 30_000)
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
    writer
        .begin_attempt(effect_attempt("effect-winner", "effect-node"))
        .await?;
    expire_effect_run(admin, "effect-winner").await?;
    assert_eq!(
        plugin
            .claim_next_production(COMPONENT, CATALOG_ID, ENVIRONMENT, 30_000)
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
    seed_durable_run(admin, "effect-pin", "cat-main", 80).await?;
    assert_eq!(
        ready_run(
            plugin
                .claim_next_production(COMPONENT, CATALOG_ID, ENVIRONMENT, 30_000)
                .await?
        ),
        "effect-pin"
    );
    let recorded = (
        Some(POD_RELEASE_VERSION),
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
                "UPDATE {SCHEMA}.runs SET release_version=NULL, manifest_digest=NULL \
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
    admin
        .execute(
            &format!(
                "DELETE FROM {SCHEMA}.effect_attempts \
                  WHERE tenant_id=$1 AND run_id='effect-pin'"
            ),
            &[&TENANT],
        )
        .await?;

    teardown(fixture).await
}
