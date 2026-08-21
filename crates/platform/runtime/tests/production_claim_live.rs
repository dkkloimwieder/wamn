//! THE SURVIVING SPINE of the production claim, live on the DEFAULT tier.
//!
//! wamn-0h0g.20.4. Every proof here runs on the `standard` durability class —
//! the class every admitted run carries unless it asks otherwise
//! (`deploy/sql/run-state.sql`, `durability_class ... DEFAULT 'standard'`).
//! FIFO, SKIP LOCKED, the lease grant, the pre-effect reclaim, crash-evidence
//! accounting, the janitor, the claim-time release record, park/wake and
//! dequeue are all proven here, and NONE of them writes an effect attempt.
//! That is the point: this file is the green signal that the crash floor
//! wamn-0h0g.20.2 shelved carried none of the queue with it.
//!
//! THE EFFECT LEDGER IS NOT ABSENT, ONLY UNPOPULATED BY THE RUNS UNDER TEST.
//! `select_production_claim_sql` names `FROM effect_attempts AS effect`
//! unconditionally — the class gate is a conjunct INSIDE that `EXISTS`, not a
//! removal of the relation — so a fixture without the table would fail at PLAN
//! TIME (42P01) on the queue's hottest statement. The relation is installed and
//! one leg deliberately POPULATES it, to show a `standard` run's claim path
//! ignores evidence it may not act on.
//!
//! The effect-uncertain floor itself lives in `production_claim_durable_live.rs`.

use std::collections::BTreeSet;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_run_state::{
    EffectWriterErrorKind, ResetProjectionFence,
    queue::{park_sql, select_production_claim_sql},
    schema_drift::{Need, assert_run_state_stand_in},
};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimErrorKind, ProductionClaimResult, ProductionReapResult,
};

mod common;

use common::{
    COMPONENT, EMPTY_HASH, POD_MANIFEST_DIGEST, POD_RELEASE_VERSION, ROLLED_COMPONENT,
    ROLLED_MANIFEST_DIGEST, ROLLED_RELEASE_VERSION, SCHEMA, TENANT, assert_callerless_terminal,
    assert_prior_winner_terminal, assert_terminal_status_dequeued, connect, digest,
    expire_effect_run, install_fixture, install_prior_caller_winner, make_callerless,
    queue_attempts, quote_literal, ready_run, release_record, seed_exhausted_run, seed_run,
    teardown,
};

#[test]
fn production_claim_run_state_stand_in_tracks_schema_of_record() {
    let stand_in = common::run_state_stand_in_ddl();
    let fixture_schema = format!("{SCHEMA}.");
    assert_eq!(stand_in.matches(&fixture_schema).count(), 3);
    let normalized = stand_in.replace(&fixture_schema, "wamn_run.");

    assert_run_state_stand_in(
        "production-claim",
        &normalized,
        &[
            ("environment_policies", Need::AbsentByDesign),
            ("runs", Need::Required),
            ("invocation_admissions", Need::AbsentByDesign),
            ("node_runs", Need::Required),
            ("effect_attempts", Need::Required),
            ("effect_attempt_dispatches", Need::AbsentByDesign),
            ("effect_attempt_outcomes", Need::AbsentByDesign),
            ("operator_run_actions", Need::AbsentByDesign),
        ],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a disposable PostgreSQL 18 URL in WAMN_PRODUCTION_CLAIM_PG_URL"]
async fn production_claim_live() -> anyhow::Result<()> {
    let url = std::env::var("WAMN_PRODUCTION_CLAIM_PG_URL")
        .context("set WAMN_PRODUCTION_CLAIM_PG_URL to a disposable PostgreSQL database")?;
    let fixture = install_fixture(&url).await?;
    let admin = &fixture.admin;
    let plugin = &fixture.plugin;
    let writer = &fixture.writer;
    let release_hash = fixture.release_hash.clone();

    // Exact global FIFO: stream sequence, then run id, breaks equal timestamps.
    for (run_id, stream_seq) in [("fifo-c", 2), ("fifo-b", 1), ("fifo-a", 1)] {
        seed_run(admin, run_id, "cat-main", &release_hash, stream_seq).await?;
    }
    let mut fifo = Vec::new();
    for _ in 0..3 {
        fifo.push(ready_run(
            plugin.claim_next_production(COMPONENT, 30_000).await?,
        ));
    }
    assert_eq!(fifo, ["fifo-a", "fifo-b", "fifo-c"]);

    // A second claimer skips the exact FIFO head while the first claimer holds
    // its production row locks, then the rolled-back head remains claimable.
    seed_run(admin, "double-a", "cat-main", &release_hash, 10).await?;
    seed_run(admin, "double-b", "cat-main", &release_hash, 11).await?;
    let first_claimer = connect(&url).await?;
    first_claimer
        .batch_execute(&format!(
            "BEGIN; SET LOCAL search_path TO {SCHEMA}, pg_catalog"
        ))
        .await?;
    first_claimer
        .execute("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
        .await?;
    let locked = first_claimer
        .query_one(&select_production_claim_sql(), &[])
        .await?;
    assert_eq!(locked.get::<_, String>(0), "double-a");
    let skipped = ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?);
    assert_eq!(skipped, "double-b");
    first_claimer.batch_execute("ROLLBACK").await?;
    let released = ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?);
    let claimed = BTreeSet::from([skipped, released]);
    assert_eq!(
        claimed,
        BTreeSet::from(["double-a".into(), "double-b".into()])
    );

    // Expired pre-effect recovery deletes only node projections and NULLs only
    // state_json; every other admitted or lineage column survives the retry.
    // `status` and the claim-time release record are excluded from the snapshot
    // because the retry CLAIM writes them; they are asserted separately below.
    //
    // THIS LEG IS BOTH-TIER AND STAYS HERE (wamn-0h0g.20.4). A pre-effect
    // reclaim needs no effect ledger at all: `classify_production_claim` returns
    // `ExpiredPreEffect` for a `standard` run with an expired prior lease, and
    // `reset_expired_pre_effect_projection` has a live production caller on both
    // tiers (`crates/execution/host/src/lib.rs`, the `ResetRequired` arm).
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,input_json,state_json,invocation_context, \
                    trigger_source,event_source_run_id,event_root_run_id,event_depth, \
                    admission_context_version,platform_revision,capture_mode,idempotency_key, \
                    response_deadline_at,run_deadline_at) \
                 VALUES ($1,'pre-effect','root',1,'running','cat-main',1,'test',$2, \
                    '{{\"input\":7}}','{{\"cursor\":9}}','{{\"source\":{{\"case\":\"a\"}}}}', \
                    'event','source-run','root-run',3,'0.1','platform-a','full','idem-a', \
                    '2030-01-01','2030-01-02')"
            ),
            &[&TENANT, &release_hash],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    lease_generation,attempts,max_attempts) \
                 VALUES ($1,'pre-effect','2000-01-01',20,'dead','2000-01-01',4,1,3)"
            ),
            &[&TENANT],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.node_runs \
                   (tenant_id,run_id,local_node_id) VALUES ($1,'pre-effect','old-node')"
            ),
            &[&TENANT],
        )
        .await?;
    let before: Value = serde_json::from_str(
        &admin
            .query_one(
                &format!(
                    "SELECT (to_jsonb(r) - ARRAY['state_json','status','updated_at',\
                    'release_version','manifest_digest']::text[])::text \
                   FROM {SCHEMA}.runs AS r WHERE tenant_id=$1 AND run_id='pre-effect'"
                ),
                &[&TENANT],
            )
            .await?
            .get::<_, String>(0),
    )?;
    let reset_required = plugin.claim_next_production(COMPONENT, 30_000).await?;
    let reset_fence = match &reset_required {
        ProductionClaimResult::ResetRequired {
            run_id,
            prior_lease_owner,
            prior_lease_expires_at,
            prior_lease_generation,
        } => ResetProjectionFence {
            run_id,
            prior_lease_owner,
            prior_lease_expires_at,
            prior_lease_generation: *prior_lease_generation,
        },
        other => panic!("expected private projection reset handoff, got {other:?}"),
    };
    assert!(
        admin
            .query_one(
                &format!("SELECT state_json IS NOT NULL FROM {SCHEMA}.runs WHERE tenant_id=$1 AND run_id='pre-effect'"),
                &[&TENANT],
            )
            .await?
            .get::<_, bool>(0),
        "the app handoff committed without clearing checkpoint state"
    );
    let wrong_generation = ResetProjectionFence {
        prior_lease_generation: reset_fence.prior_lease_generation + 1,
        ..reset_fence
    };
    let mismatch = writer
        .reset_expired_pre_effect_projection(wrong_generation)
        .await
        .expect_err("a different lease generation reset projection state");
    assert_eq!(mismatch.kind(), EffectWriterErrorKind::ResetFenceLost);
    assert_eq!(
        admin
            .query_one(
                &format!("SELECT count(*) FROM {SCHEMA}.node_runs WHERE tenant_id=$1 AND run_id='pre-effect'"),
                &[&TENANT],
            )
            .await?
            .get::<_, i64>(0),
        1,
        "fence mismatch mutated the projection"
    );
    assert_eq!(
        writer
            .reset_expired_pre_effect_projection(reset_fence)
            .await
            .map_err(anyhow::Error::new)?,
        1
    );
    let pre_effect = plugin.claim_next_production(COMPONENT, 30_000).await?;
    let (run_id, payload, lease_generation) = match pre_effect {
        ProductionClaimResult::Ready {
            run_id,
            payload,
            lease_generation,
        } => (run_id, payload, lease_generation),
        other => panic!("expected pre-effect retry to execute, got {other:?}"),
    };
    assert_eq!(run_id, "pre-effect");
    assert_eq!(lease_generation, 5);
    assert_eq!(
        serde_json::from_str::<Value>(&payload)?,
        json!({
            "input": 7,
            "causation": {"run": "pre-effect", "root": "root-run", "depth": 3}
        })
    );
    let after: Value = serde_json::from_str(
        &admin
            .query_one(
                &format!(
                    "SELECT (to_jsonb(r) - ARRAY['state_json','status','updated_at',\
                    'release_version','manifest_digest']::text[])::text \
                   FROM {SCHEMA}.runs AS r WHERE tenant_id=$1 AND run_id='pre-effect'"
                ),
                &[&TENANT],
            )
            .await?
            .get::<_, String>(0),
    )?;
    assert_eq!(after, before);
    assert_eq!(
        release_record(admin, "pre-effect").await?,
        (
            Some(POD_RELEASE_VERSION),
            Some(POD_MANIFEST_DIGEST.to_string())
        ),
        "the retry claim recorded the claiming pod's release exactly once"
    );
    let (state, nodes): (Option<String>, i64) = {
        let row = admin
            .query_one(
                &format!(
                    "SELECT r.state_json::text, \
                            (SELECT count(*) FROM {SCHEMA}.node_runs n \
                              WHERE n.tenant_id=r.tenant_id AND n.run_id=r.run_id) \
                       FROM {SCHEMA}.runs r WHERE r.tenant_id=$1 AND r.run_id='pre-effect'"
                ),
                &[&TENANT],
            )
            .await?;
        (row.get(0), row.get(1))
    };
    assert_eq!((state, nodes), (None, 0));

    // The effect-free exhausted path computes the generic outcome and hash in
    // the host, atomically releases an attached caller, and dequeues.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,trigger_source) \
                 VALUES ($1,'janitor','root',1,'running','cat-main',1,'test',$2,'http')"
            ),
            &[&TENANT, &release_hash],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    attempts,max_attempts) \
                 VALUES ($1,'janitor','2000-01-01',60,'dead','2000-01-01',3,3)"
            ),
            &[&TENANT],
        )
        .await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
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
                                 WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE tenant_id=$1 AND run_id='janitor'"
            ),
            &[&TENANT],
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
        wamn_flow::canonical_json_sha256(&janitor_body)
    );
    assert!(janitor.get::<_, bool>(5));
    assert!(!janitor.get::<_, bool>(6));

    seed_exhausted_run(admin, "janitor-callerless", &release_hash, 61).await?;
    make_callerless(admin, "janitor-callerless").await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "janitor-callerless".into()
        }
    );
    assert_callerless_terminal(admin, "janitor-callerless", "infrastructure-failure").await?;

    seed_exhausted_run(admin, "janitor-winner", &release_hash, 62).await?;
    let janitor_winner = install_prior_caller_winner(admin, "janitor-winner").await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
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
    seed_run(admin, "grant-refused", "cat-main", &release_hash, 65).await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=max_attempts-1 \
                  WHERE tenant_id=$1 AND run_id='grant-refused'"
            ),
            &[&TENANT],
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
        .claim_next_production(COMPONENT, 30_000)
        .await
        .expect_err("the probed grant refuses");
    assert_eq!(starved.kind(), ProductionClaimErrorKind::Storage);
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
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "grant-refused".into()
        },
        "the budget the refusal spent must reach the janitor"
    );
    assert_terminal_status_dequeued(admin, "grant-refused", "infrastructure-failure").await?;

    // ---- the default class ignores a POPULATED effect ledger (wamn-0h0g.20.2,
    // proven live by wamn-0h0g.20.4) ----------------------------------------
    //
    // This is the one leg that seeds an effect attempt on this tier, and it
    // seeds it precisely to show the claim path does not act on it. The run is
    // `standard`, its lease is expired and its crash budget is spent, and an
    // attributed effect attempt exists for it. Both class gates must hold:
    //
    //   * the eligibility predicate's effect disjunct carries
    //     `AND selected_run.durability_class = 'durable'`, so the run is NOT
    //     claimable and the claim sees an empty queue; and
    //   * the janitor's fresh-snapshot classification is skipped entirely, so
    //     the reaper returns `Reaped`, never `EffectAttempt`.
    //
    // The `effect-race` run in `production_claim_durable_live.rs` is this exact
    // shape on the other class — budget spent, lease expired, one attributed
    // attempt — and BOTH results flip there: the claim reaches the row and
    // returns `Terminalized`, and the reaper returns `EffectAttempt`. The two
    // legs together are the class gate, live.
    //
    // Nothing else in this file is downstream of the row: it is dequeued by the
    // reap, and the ledger row it leaves behind is correlated by `run_id` in
    // every statement that reads one.
    seed_exhausted_run(admin, "standard-ledger", &release_hash, 66).await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                    generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                 VALUES ($1,'standard-ledger',$2,$2,0,'ledger-node',$2,'manager',0,1, \
                         'not-required','2099-01-01T00:00:00Z','sha256:claim-live-effect-input')"
            ),
            &[&TENANT, &EMPTY_HASH],
        )
        .await?;
    // The fixture's release-pin guard shares the class gate: even with an
    // attributed attempt, this `standard` run may clear a claim-time pair.
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs \
                    SET release_version=$2, manifest_digest=$3 \
                  WHERE tenant_id=$1 AND run_id='standard-ledger'"
            ),
            &[&TENANT, &POD_RELEASE_VERSION, &POD_MANIFEST_DIGEST],
        )
        .await?;
    assert_eq!(
        admin
            .execute(
                &format!(
                    "UPDATE {SCHEMA}.runs \
                        SET release_version=NULL, manifest_digest=NULL \
                      WHERE tenant_id=$1 AND run_id='standard-ledger'"
                ),
                &[&TENANT],
            )
            .await
            .context("clear a standard run's release pair despite attributed effect evidence")?,
        1
    );
    assert_eq!(
        plugin.claim_next_production(COMPONENT, 30_000).await?,
        ProductionClaimResult::Empty,
        "the default class was let into the shelved crash floor by its ledger"
    );
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "standard-ledger".into()
        },
        "the janitor deferred to effect evidence the default class may not act on"
    );
    assert_terminal_status_dequeued(admin, "standard-ledger", "infrastructure-failure").await?;

    // ---- the claim-time release record (wamn-0h0g.15.11, carrying the two
    // surviving proof legs of the superseded wamn-0h0g.4.14) -----------------
    //
    // MID-RUN REPUBLISH INVISIBILITY. The run is admitted under catalog version
    // 1; a republish then lands version 2 between admission and claim. The pair
    // the claim records is the CLAIMING POD's (version 7), so it matches neither
    // the admitted release nor the republished one, and the run's own admission
    // identity is untouched.
    seed_run(admin, "release-record", "cat-main", &release_hash, 70).await?;
    assert_eq!(release_record(admin, "release-record").await?, (None, None));
    admin
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             VALUES ($1,'cat-main',2,'root',1,$2)",
            &[&TENANT, &release_hash],
        )
        .await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "release-record"
    );
    let recorded = (
        Some(POD_RELEASE_VERSION),
        Some(POD_MANIFEST_DIGEST.to_string()),
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);
    assert_eq!(
        admin
            .query_one(
                &format!(
                    "SELECT catalog_version FROM {SCHEMA}.runs \
                      WHERE tenant_id=$1 AND run_id='release-record'"
                ),
                &[&TENANT],
            )
            .await?
            .get::<_, i32>(0),
        1,
        "a republish must not move the run's admitted release either"
    );

    // SAME-RELEASE RE-CLAIM. The classifier's pre-effect reclaim clears the
    // abandoned attempt's pair and the grant records this pod's again, so the
    // observable pair is unchanged.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "release-record"
    );
    assert_eq!(release_record(admin, "release-record").await?, recorded);

    // RESET PER CLAIM ATTEMPT (wamn-0h0g.15.55). A pod carrying a DIFFERENT
    // release re-claims an expired pre-effect run successfully: the pair is
    // write-once per ATTEMPT, and the classifier's reset — not an exception in
    // the guard — is what lets the next claim record afresh. Under a rollout
    // this is the normal case, not an edge case.
    expire_effect_run(admin, "release-record").await?;
    assert_eq!(
        ready_run(
            plugin
                .claim_next_production(ROLLED_COMPONENT, 30_000)
                .await?
        ),
        "release-record"
    );
    let rerecorded = (
        Some(ROLLED_RELEASE_VERSION),
        Some(ROLLED_MANIFEST_DIGEST.to_string()),
    );
    assert_eq!(
        release_record(admin, "release-record").await?,
        rerecorded,
        "the reclaiming pod records its own release, not the dead attempt's"
    );

    // THE ERASURE IS NOT A BLANKET HOLE — BUT ITS PRECONDITION IS NO LONGER THE
    // RUN'S HISTORY. The record names the CLAIM currently executing the run
    // (wamn-0h0g.13.55), so an executed node no longer pins it: a parked run has
    // generally executed nodes and must still be able to reopen its
    // claimability (wamn-0h0g.15.82). What still pins it is a TERMINAL STATUS
    // here, and — on the premium class only — an ATTRIBUTED EFFECT, which is
    // `production_claim_durable_live.rs`'s leg. value -> value' is refused on
    // every path and every class.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.node_runs \
                   (tenant_id,run_id,local_node_id) VALUES ($1,'release-record','a-node')"
            ),
            &[&TENANT],
        )
        .await?;
    let third_pair = format!(
        "release_version=9, manifest_digest={}",
        quote_literal(EMPTY_HASH)
    );
    let rewritten = admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET {third_pair} \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await
        .expect_err("no path may rewrite a recorded release in place");
    let db = rewritten
        .as_db_error()
        .expect("guard refusal is a db error");
    assert_eq!(db.code().code(), "55000");
    assert_eq!(db.message(), "run-release-record-immutable");

    // A NODE PROJECTION NO LONGER REFUSES THE ERASURE. This is the exact leg
    // that made the queue park the one claimability-reopening arm which could
    // not clear.
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET release_version=NULL, manifest_digest=NULL \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await
        .expect("a runnable, effect-free run may reopen its claimability");
    assert_eq!(release_record(admin, "release-record").await?, (None, None));
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs \
                    SET release_version=$2, manifest_digest=$3 \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT, &ROLLED_RELEASE_VERSION, &ROLLED_MANIFEST_DIGEST],
        )
        .await?;
    assert_eq!(release_record(admin, "release-record").await?, rerecorded);

    // A TERMINAL STATUS STILL DOES REFUSE IT: a finished run keeps the audit
    // link to the plan hashes it ran, on every class.
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET status='completed' \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await?;
    let terminal = admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET release_version=NULL, manifest_digest=NULL \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await
        .expect_err("a terminal run keeps the audit link to the plan hashes it ran");
    assert_eq!(
        terminal
            .as_db_error()
            .expect("guard refusal is a db error")
            .message(),
        "run-release-record-immutable"
    );
    assert_eq!(release_record(admin, "release-record").await?, rerecorded);
    admin
        .execute(
            &format!(
                "DELETE FROM {SCHEMA}.run_queue \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await?;

    // PARK/WAKE IS COVERED BY THE PARK'S OWN RESET (wamn-0h0g.15.82). `park_sql`
    // releases the lease (`lease_owner`/`lease_expires_at` to NULL), so a
    // doorbell wake classifies `Ordinary` and no classifier arm runs — which is
    // exactly why the PARK, the arm that reopens claimability, clears the pair
    // itself. A pod carrying a different release therefore wakes the run and
    // records its own identity. Before this, the wake refused at the guard
    // forever: a released lease is not crash evidence, so the refusal spent no
    // budget, the janitor could never reap the run, and it stayed its tenant's
    // FIFO head.
    seed_run(admin, "park-wake", "cat-main", &release_hash, 71).await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "park-wake"
    );
    assert_eq!(release_record(admin, "park-wake").await?, recorded);
    // The SHIPPED park statement, not a hand-written stand-in: a mutation that
    // dropped its reset fails exactly here.
    let tenant_literal = quote_literal(TENANT);
    admin
        .batch_execute(&format!(
            "SET search_path = {SCHEMA}; SET app.tenant = {tenant_literal};"
        ))
        .await?;
    admin.execute(&park_sql(), &[&"park-wake", &0_i64]).await?;
    admin
        .batch_execute("RESET search_path; RESET app.tenant;")
        .await?;
    assert_eq!(
        release_record(admin, "park-wake").await?,
        (None, None),
        "the park cleared the record of the claim it released"
    );
    assert_eq!(
        ready_run(
            plugin
                .claim_next_production(ROLLED_COMPONENT, 30_000)
                .await?
        ),
        "park-wake"
    );
    assert_eq!(
        release_record(admin, "park-wake").await?,
        rerecorded,
        "the waking pod records its own release, not the parked claim's"
    );
    assert_eq!(
        queue_attempts(admin, "park-wake").await?,
        0,
        "a released lease is not crash evidence, so the wake spends no budget"
    );
    admin
        .execute(
            &format!("DELETE FROM {SCHEMA}.run_queue WHERE tenant_id=$1 AND run_id='park-wake'"),
            &[&TENANT],
        )
        .await?;

    // WHERE THE FIVE RESOLUTION FAIL KINDS WENT (wamn-0h0g.15.67). Until
    // wamn-0h0g.15.10 the claim resolved the run's release here and a bundle whose
    // bytes did not hash to their name was terminalized `hash-invalid-bytes`
    // before any lease was granted. Resolution left the claim with that commit, so
    // `unresolvable-name`, `hash-invalid-bytes`, `foreign-revision`,
    // `incompatible-contract` and `unbound-requirement` have no producer anywhere
    // in the tree — deliberately, not by omission: the claim is
    // lock/classify/lease and reads no catalog at all. Hash-at-transfer
    // verification lives on the supply path now
    // (`plugins::runner_plan_supply`'s `insert_verified`), refuses as
    // `SupplyError::HashMismatch`, and is proven live in
    // `tests/oci_plan_source_live.rs`.
    //
    // So this claims a run whose named bundle was never published to the catalog
    // at all, and it must LEASE. That is the deleted assertion restored inverted,
    // which is the point of keeping it: a reintroduced claim-time verification
    // fails right here rather than silently re-pinning runs to a release.
    let unpublished = digest(b"a bundle no catalog row carries");
    seed_run(admin, "unpublished", "cat-main", &unpublished, 72).await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "unpublished"
    );

    teardown(fixture).await
}
