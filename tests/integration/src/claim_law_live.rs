//! `[CLAIM-LAW-LIVE]` — the emitted claim contract tests, executed.
//!
//! The generator emits `move.claim-tests.json` next to the SQL it emits, and
//! until now nothing ran it. A test emitted by the pass that emits the SQL
//! moves with a mutant, so it can only catch one by running the SQL against a
//! real server. These tests do that.
//!
//! The first consumer is `wamn-wms:inventory/move@1.0.0`. It is create-shaped:
//! the claim row mints `movement_id`, and every later caller depends on that
//! id being the same one after a replay. The claim table carries no
//! foreign key to the pallet, so the two cases need the migration and nothing
//! else.
//!
//! `WAMN_CLAIM_LAW_PG_URL` names a fresh disposable PostgreSQL 18 database.
//! The live tests are `#[ignore]`d and read the variable with an error rather
//! than a skip, so an unarmed run never counts them as passing.

use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;
use tokio::sync::OnceCell;
use tokio_postgres::{Client, NoTls, Row};
use uuid::Uuid;
use wamn_execution_contract::canonical_json_bytes;
use wamn_gate_harness::claim_law::{self, BindValue, ClaimContract, CommandFixture};

const URL_ENV: &str = "WAMN_CLAIM_LAW_PG_URL";
const PACKAGE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/wamn_wms");
const CLAIM_TESTS: &str = "generated/contracts/inventory/move.claim-tests.json";
const OPERATION: &str = "wamn-wms:inventory/move@1.0.0";
const MIGRATION: &str = include_str!("../../../apps/wamn_wms/migrations/0001_initial.sql");

const REPLAY_CASE: &str = "replay_returns_the_immutable_original";
const CONFLICT_CASE: &str = "changed_request_under_a_live_key_refuses";

/// The canonical command fields, spelled the way
/// `apps/wamn_wms/data/src/inventory_move.rs` spells them.
const PALLET_ID: Uuid = Uuid::from_u128(0xc1a1_0001);
const TO_LOCATION_ID: Uuid = Uuid::from_u128(0xc1a1_0002);
const CHANGED_LOCATION_ID: Uuid = Uuid::from_u128(0xc1a1_0003);
const OCCURRED_AT: &str = "2026-09-05T12:00:00.000000Z";

/// One prepared schema for the whole test binary. The migration creates
/// `wms.*` by name, so the tests share the schema and separate themselves by
/// idempotency key.
static PREPARED: OnceCell<String> = OnceCell::const_new();

/// The values the emitted statements bind, other than the canonical command.
struct MoveFixture {
    idempotency_key: String,
}

impl CommandFixture for MoveFixture {
    fn bind(&self, statement: &str, bind: &str, claim: Option<&Row>) -> Result<BindValue> {
        match (statement, bind) {
            (_, "idempotency_key") => Ok(Box::new(self.idempotency_key.clone())),
            ("claim_command", "pallet_id") => Ok(Box::new(PALLET_ID)),
            // THE IDENTITY COMES FROM THE CLAIM. The command binds back the
            // value the claim statement returned. Nothing here mints an id,
            // so a replay returns the same one by construction.
            ("finalize_command", "movement_id") => {
                let claim = claim.context("finalize_command runs after the claim")?;
                Ok(Box::new(claim.try_get::<_, Uuid>("movement_id")?))
            }
            ("finalize_command", "pallet_status") => Ok(Box::new("available".to_owned())),
            ("finalize_command", "row_version") => Ok(Box::new(2_i64)),
            _ => bail!("the fixture has no value for {statement}.{bind}"),
        }
    }
}

#[test]
fn the_emitted_contract_names_the_two_cases_the_live_tests_execute() -> Result<()> {
    let contract = contract()?;
    ensure!(
        contract.operation == OPERATION,
        "the emitted contract names {}, not {OPERATION}",
        contract.operation
    );
    ensure!(
        contract.law == claim_law::LAW,
        "the emitted contract establishes {}, not {}",
        contract.law,
        claim_law::LAW
    );
    ensure!(
        contract.cases.len() == 2,
        "the emitted contract names {} cases, and the live tests execute two",
        contract.cases.len()
    );
    contract.case(REPLAY_CASE)?;
    contract.case(CONFLICT_CASE)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a fresh disposable PostgreSQL 18 URL in WAMN_CLAIM_LAW_PG_URL"]
async fn a_replay_returns_the_immutable_original_result_and_writes_nothing() -> Result<()> {
    let contract = contract()?;
    let mut client = session().await?;
    let fixture = MoveFixture {
        idempotency_key: "claim-law-replay".to_owned(),
    };
    let report = contract
        .run_case(
            &mut client,
            contract.case(REPLAY_CASE)?,
            &fixture,
            &canonical_move_command(TO_LOCATION_ID),
            &canonical_move_command(CHANGED_LOCATION_ID),
        )
        .await?;
    ensure!(
        report.refusal.is_none(),
        "a replay of the same request must not refuse, and it gave {:?}",
        report.refusal
    );
    ensure!(
        report.claim_identity.contains_key("movement_id"),
        "the claim statement must mint movement_id, and it returned {:?}",
        report.claim_identity.keys().collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires a fresh disposable PostgreSQL 18 URL in WAMN_CLAIM_LAW_PG_URL"]
async fn a_changed_request_under_a_live_key_refuses_with_idempotency_conflict() -> Result<()> {
    let contract = contract()?;
    let mut client = session().await?;
    let fixture = MoveFixture {
        idempotency_key: "claim-law-conflict".to_owned(),
    };
    let report = contract
        .run_case(
            &mut client,
            contract.case(CONFLICT_CASE)?,
            &fixture,
            &canonical_move_command(TO_LOCATION_ID),
            &canonical_move_command(CHANGED_LOCATION_ID),
        )
        .await?;
    ensure!(
        report.refusal.as_deref() == Some("idempotency_conflict"),
        "a changed request under a live key must refuse with idempotency_conflict, \
         and it gave {:?}",
        report.refusal
    );
    Ok(())
}

/// The mutant. `ON CONFLICT ... DO NOTHING` is the one clause that keeps a
/// replay read-only and keeps the first request's bytes authoritative. Turn it
/// into `DO UPDATE` and the second call writes, hands back an id, and adopts
/// the changed request as if it had always been the original. Both emitted
/// cases must go red, and this test fails if either one still passes.
#[tokio::test]
#[ignore = "requires a fresh disposable PostgreSQL 18 URL in WAMN_CLAIM_LAW_PG_URL"]
async fn a_claim_that_updates_on_conflict_fails_both_emitted_cases() -> Result<()> {
    let mut mutant = contract()?;
    mutant.mutate(
        "claim_command",
        "DO NOTHING",
        "DO UPDATE SET canonical_command = EXCLUDED.canonical_command",
    )?;
    let mut client = session().await?;
    for (case_id, key) in [
        (REPLAY_CASE, "claim-law-mutant-replay"),
        (CONFLICT_CASE, "claim-law-mutant-conflict"),
    ] {
        let fixture = MoveFixture {
            idempotency_key: key.to_owned(),
        };
        let outcome = mutant
            .run_case(
                &mut client,
                mutant.case(case_id)?,
                &fixture,
                &canonical_move_command(TO_LOCATION_ID),
                &canonical_move_command(CHANGED_LOCATION_ID),
            )
            .await;
        ensure!(
            outcome.is_err(),
            "the mutant claim passed case {case_id}, so the case does not test the SQL"
        );
    }
    Ok(())
}

fn contract() -> Result<ClaimContract> {
    claim_law::load(Path::new(PACKAGE_ROOT), Path::new(CLAIM_TESTS))
}

/// The canonical command bytes for one move, built the way
/// `apps/wamn_wms/data/src/inventory_move.rs` builds them: the request
/// value without the idempotency key, as canonical JSON.
fn canonical_move_command(to_location_id: Uuid) -> Vec<u8> {
    canonical_json_bytes(&json!({
        "pallet_id": PALLET_ID.hyphenated().to_string(),
        "to_location_id": to_location_id.hyphenated().to_string(),
        "expected_row_version": 1,
        "occurred_at": OCCURRED_AT,
    }))
}

/// A connection that reaches the WMS tables the way the runtime does.
async fn session() -> Result<Client> {
    let url = prepared_database().await?;
    let client = connect(&url).await?;
    client
        .batch_execute("SET search_path = wms, public")
        .await
        .context("select WMS through trusted connection context")?;
    Ok(client)
}

/// Apply the exact WMS migration once, to a database that must be fresh.
async fn prepared_database() -> Result<String> {
    PREPARED.get_or_try_init(prepare).await.cloned()
}

async fn prepare() -> Result<String> {
    let url = std::env::var(URL_ENV)
        .with_context(|| format!("{URL_ENV} names a fresh disposable PostgreSQL 18 database"))?;
    let client = connect(&url).await?;
    let version = client
        .query_one("SELECT current_setting('server_version_num')", &[])
        .await
        .context("read PostgreSQL server version")?
        .get::<_, String>(0)
        .parse::<u32>()
        .context("server_version_num is not numeric")?;
    ensure!(
        (180_000..190_000).contains(&version),
        "the claim-law gate requires PostgreSQL 18, found {version}"
    );
    let exists = client
        .query_one("SELECT to_regnamespace('wms') IS NOT NULL", &[])
        .await
        .context("inspect WMS schema freshness")?
        .get::<_, bool>(0);
    ensure!(!exists, "disposable database already contains schema wms");
    client
        .batch_execute("CREATE SCHEMA wms")
        .await
        .context("create the WMS schema")?;
    client
        .batch_execute(MIGRATION)
        .await
        .context("apply the exact WMS migration")?;
    Ok(url)
}

async fn connect(url: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to disposable PostgreSQL")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            "SET statement_timeout = '10s'; \
             SET lock_timeout = '5s'; \
             SET transaction_timeout = '30s'",
        )
        .await
        .context("bound claim-law gate timeouts")?;
    Ok(client)
}
