//! Project-admin terminalization of one effect-uncertain run.

use std::collections::BTreeSet;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls, Transaction, error::SqlState};
use wamn_run_state::operator_action::{
    OperatorActionBasis, OperatorTerminalizeResult, insert_operator_action_sql,
    lock_operator_actions_sql, lock_operator_effect_attempts_sql, lock_operator_node_facts_sql,
    lock_operator_queue_fact_sql, lock_operator_run_sql, terminalize_operator_node_sql,
    terminalize_operator_run_sql,
};
use wamn_schema_control::BareSchemaName;

/// Exact operator input; effect identity and asserted outcome are absent by construction.
#[derive(Debug, Args)]
pub struct TerminalizeEffectUncertainArgs {
    /// Project-admin PostgreSQL URL for the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Project run-plane schema.
    #[arg(long, default_value = "wamn_run")]
    pub schema: String,

    /// Exact tenant owning the run.
    #[arg(long)]
    pub tenant: String,

    /// Exact effect-uncertain run.
    #[arg(long)]
    pub run: String,

    /// Evidence basis: external-evidence, counterparty-confirmation, or operator-judgment.
    #[arg(long)]
    pub basis: OperatorActionBasis,

    /// Opaque non-empty reference to the evidence used.
    #[arg(long)]
    pub evidence_ref: String,

    /// Opaque non-empty idempotency correlation.
    #[arg(long)]
    pub correlation_id: String,
}

/// One validated terminalization request.
#[derive(Clone, Debug)]
pub struct TerminalizeEffectUncertain<'a> {
    pub tenant: &'a str,
    pub run: &'a str,
    pub basis: OperatorActionBasis,
    pub evidence_ref: &'a str,
    pub correlation_id: &'a str,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OccurrenceKey {
    frame_id: i64,
    local_node_id: String,
    occurrence: i32,
}

#[derive(Clone, Debug)]
struct PriorAction {
    correlation_id: String,
    run_id: String,
    basis: String,
    evidence_ref: String,
    principal: String,
}

impl PriorAction {
    fn is_identical(&self, request: &TerminalizeEffectUncertain<'_>, principal: &str) -> bool {
        self.correlation_id == request.correlation_id
            && self.run_id == request.run
            && self.basis == request.basis.as_str()
            && self.evidence_ref == request.evidence_ref
            && self.principal == principal
    }
}

async fn lock_prior_actions(
    transaction: &Transaction<'_>,
    request: &TerminalizeEffectUncertain<'_>,
) -> Result<Vec<PriorAction>, tokio_postgres::Error> {
    Ok(transaction
        .query(
            lock_operator_actions_sql(),
            &[&request.tenant, &request.correlation_id, &request.run],
        )
        .await?
        .into_iter()
        .map(|row| PriorAction {
            correlation_id: row.get(0),
            run_id: row.get(1),
            basis: row.get(2),
            evidence_ref: row.get(3),
            principal: row.get(4),
        })
        .collect())
}

fn prior_action_result(
    prior_actions: &[PriorAction],
    request: &TerminalizeEffectUncertain<'_>,
    principal: &str,
) -> Option<OperatorTerminalizeResult> {
    (!prior_actions.is_empty()).then(|| {
        if prior_actions.len() == 1 && prior_actions[0].is_identical(request, principal) {
            OperatorTerminalizeResult::IdenticalRetry
        } else {
            OperatorTerminalizeResult::CorrelationConflict
        }
    })
}

pub async fn run(args: TerminalizeEffectUncertainArgs) -> anyhow::Result<()> {
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let request = TerminalizeEffectUncertain {
        tenant: &args.tenant,
        run: &args.run,
        basis: args.basis,
        evidence_ref: &args.evidence_ref,
        correlation_id: &args.correlation_id,
    };
    validate_request(&request)?;

    let (mut client, connection) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("project-admin connect")?;
    let connection_task = tokio::spawn(connection);
    let result = terminalize(&mut client, &schema, &request).await;
    drop(client);
    let _ = connection_task.await;
    println!("{}", result?);
    Ok(())
}

fn validate_request(request: &TerminalizeEffectUncertain<'_>) -> anyhow::Result<()> {
    for (name, value) in [
        ("--tenant", request.tenant),
        ("--run", request.run),
        ("--evidence-ref", request.evidence_ref),
        ("--correlation-id", request.correlation_id),
    ] {
        if value.is_empty() {
            bail!("{name} must be non-empty");
        }
    }
    Ok(())
}

/// Execute the sole operator mutation transaction.
pub async fn terminalize(
    client: &mut Client,
    schema: &BareSchemaName,
    request: &TerminalizeEffectUncertain<'_>,
) -> anyhow::Result<OperatorTerminalizeResult> {
    validate_request(request)?;
    let transaction = client
        .transaction()
        .await
        .context("begin terminalization")?;
    let outcome = terminalize_in_transaction(&transaction, schema, request).await;
    match outcome {
        Ok(OperatorTerminalizeResult::Terminalized) => {
            transaction
                .commit()
                .await
                .context("commit terminalization")?;
            Ok(OperatorTerminalizeResult::Terminalized)
        }
        Ok(result) => {
            transaction
                .rollback()
                .await
                .context("roll back non-mutating terminalization result")?;
            Ok(result)
        }
        Err(error) if is_identity_conflict(&error) => {
            transaction
                .rollback()
                .await
                .context("roll back terminalization identity conflict")?;
            Ok(OperatorTerminalizeResult::CorrelationConflict)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error).context("terminalize effect-uncertain run")
        }
    }
}

async fn terminalize_in_transaction(
    transaction: &Transaction<'_>,
    schema: &BareSchemaName,
    request: &TerminalizeEffectUncertain<'_>,
) -> Result<OperatorTerminalizeResult, tokio_postgres::Error> {
    transaction
        .query_one(
            "SELECT set_config('search_path', $1, true), set_config('app.tenant', $2, true)",
            &[&schema.as_str(), &request.tenant],
        )
        .await?;
    let principal: String = transaction
        .query_one("SELECT session_user::text", &[])
        .await?
        .get(0);

    let prior_actions = lock_prior_actions(transaction, request).await?;
    if let Some(result) = prior_action_result(&prior_actions, request, &principal) {
        return Ok(result);
    }

    let run = transaction
        .query_opt(lock_operator_run_sql(), &[&request.tenant, &request.run])
        .await?;

    // The first identity read cannot gap-lock an absent row at READ COMMITTED.
    // Recheck after owning the run lock so a concurrent winner is classified by
    // its immutable action rather than by the now-terminal run projection.
    let prior_actions = lock_prior_actions(transaction, request).await?;
    if let Some(result) = prior_action_result(&prior_actions, request, &principal) {
        return Ok(result);
    }

    let Some(run) = run else {
        return Ok(OperatorTerminalizeResult::RunNotFound);
    };
    let run_status: String = run.get(0);
    let fail_kind: Option<String> = run.get(1);

    let node_rows = transaction
        .query(
            lock_operator_node_facts_sql(),
            &[&request.tenant, &request.run],
        )
        .await?;
    let attempt_rows = transaction
        .query(
            lock_operator_effect_attempts_sql(),
            &[&request.tenant, &request.run],
        )
        .await?;
    let queue_present = transaction
        .query_opt(
            lock_operator_queue_fact_sql(),
            &[&request.tenant, &request.run],
        )
        .await?
        .is_some();

    if run_status != "effect-uncertain" {
        return Ok(OperatorTerminalizeResult::NotEffectUncertain);
    }
    if fail_kind.as_deref() != Some("effect-uncertain") || attempt_rows.is_empty() || queue_present
    {
        return Ok(OperatorTerminalizeResult::RunStateInvariant);
    }

    let attempts = attempt_rows
        .into_iter()
        .map(|row| OccurrenceKey {
            frame_id: row.get(0),
            local_node_id: row.get(1),
            occurrence: row.get(2),
        })
        .collect::<BTreeSet<_>>();
    let candidates = node_rows
        .into_iter()
        .filter_map(|row| {
            let key = OccurrenceKey {
                frame_id: row.get(0),
                local_node_id: row.get(1),
                occurrence: row.get(2),
            };
            let status: String = row.get(3);
            (status == "started" && attempts.contains(&key)).then_some(key)
        })
        .collect::<Vec<_>>();
    if candidates.len() > 1 {
        return Ok(OperatorTerminalizeResult::RunStateInvariant);
    }
    let candidate = candidates.first();
    let (frame_id, local_node_id, occurrence, node_status) =
        candidate.map_or((None, None, None, None), |candidate| {
            (
                Some(candidate.frame_id),
                Some(candidate.local_node_id.as_str()),
                Some(candidate.occurrence),
                Some("started"),
            )
        });

    transaction
        .query_one(
            insert_operator_action_sql(),
            &[
                &request.tenant,
                &request.correlation_id,
                &request.run,
                &request.basis.as_str(),
                &request.evidence_ref,
                &principal,
                &frame_id,
                &local_node_id,
                &occurrence,
                &node_status,
            ],
        )
        .await?;
    if let Some(candidate) = candidate {
        let updated = transaction
            .execute(
                terminalize_operator_node_sql(),
                &[
                    &request.tenant,
                    &request.run,
                    &candidate.frame_id,
                    &candidate.local_node_id,
                    &candidate.occurrence,
                ],
            )
            .await?;
        if updated != 1 {
            return Ok(OperatorTerminalizeResult::RunStateInvariant);
        }
    }
    let updated = transaction
        .execute(
            terminalize_operator_run_sql(),
            &[&request.tenant, &request.run],
        )
        .await?;
    if updated != 1 {
        return Ok(OperatorTerminalizeResult::RunStateInvariant);
    }
    Ok(OperatorTerminalizeResult::Terminalized)
}

fn is_identity_conflict(error: &tokio_postgres::Error) -> bool {
    error.as_db_error().is_some_and(|database| {
        database.code() == &SqlState::UNIQUE_VIOLATION
            && matches!(
                database.constraint(),
                Some("operator_run_actions_run_key" | "operator_run_actions_correlation_key")
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_input_rejects_only_empty_opaque_values() {
        let valid = TerminalizeEffectUncertain {
            tenant: "tenant",
            run: "run",
            basis: OperatorActionBasis::OperatorJudgment,
            evidence_ref: " ",
            correlation_id: "correlation",
        };
        assert!(validate_request(&valid).is_ok());
        for empty_field in 0..4 {
            let mut fields = ["tenant", "run", "evidence", "correlation"];
            fields[empty_field] = "";
            let request = TerminalizeEffectUncertain {
                tenant: fields[0],
                run: fields[1],
                basis: OperatorActionBasis::ExternalEvidence,
                evidence_ref: fields[2],
                correlation_id: fields[3],
            };
            assert!(validate_request(&request).is_err());
        }
    }
}
