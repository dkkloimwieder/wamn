//! The worker's records of the create-environment saga
//! (`docs/plan/platform-ui.md` §5.2 and §5.5, `wamn-zua8.3`).
//!
//! The `environment.create` route writes one saga with its request in
//! `input` and every step in `provisioning.saga_steps`, so `environment.list`
//! shows the whole chain from the start. The request types, the steps and the
//! SQL text live in `wamn_control_provision::saga`, so the control host carries
//! no code of this crate. `wamn-ctl serve` alone runs a saga and updates its
//! steps, with the functions here. It runs the steps in [`STEPS`] order, one saga per
//! org at a time. A failure leaves the step `failed` with its error, and the
//! saga `failed`. The last step writes the operator commands into its `detail`
//! and leaves the saga `awaiting-operator`. [`saga_resume`] and
//! [`saga_abandon`] are the two ways out of a failure. The routes
//! `environment.resume` and `environment.abandon` write the same saga
//! statuses with the same SQL, and never a step.

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, GenericClient, Transaction};
use wamn_control_provision::saga::{
    EnvironmentRequest, abandon_saga_sql, await_operator_sql, complete_step_sql, fail_step_sql,
    lock_environment_saga_sql, next_open_saga_sql, resume_saga_sql, start_step_sql,
};

#[cfg(doc)]
use wamn_control_provision::saga::STEPS;

/// Return a `failed` saga to `pending`. Its failed step keeps its error until
/// the worker claims the saga and starts that step again.
pub async fn saga_resume(client: &mut Client, saga_id: &str) -> anyhow::Result<()> {
    let transaction = client.transaction().await.context("open the transaction")?;
    let status = lock_saga(&transaction, saga_id).await?;
    if status != "failed" {
        bail!("saga {saga_id} is {status}; only a failed saga resumes");
    }
    transaction
        .execute(resume_saga_sql(), &[&saga_id])
        .await
        .context("return the saga to pending")?;
    transaction.commit().await.context("commit the resume")
}

/// End a `failed` or `pending` saga as `abandoned`. A `running` saga
/// belongs to the worker and is refused.
pub async fn saga_abandon(client: &mut Client, saga_id: &str) -> anyhow::Result<()> {
    let transaction = client.transaction().await.context("open the transaction")?;
    let status = lock_saga(&transaction, saga_id).await?;
    if status != "failed" && status != "pending" {
        bail!("saga {saga_id} is {status}; only a failed or pending saga is abandoned");
    }
    transaction
        .execute(abandon_saga_sql(), &[&saga_id])
        .await
        .context("abandon the saga")?;
    transaction.commit().await.context("commit the abandon")
}

async fn lock_saga(transaction: &Transaction<'_>, saga_id: &str) -> anyhow::Result<String> {
    let row = transaction
        .query_opt(lock_environment_saga_sql(), &[&saga_id])
        .await
        .context("read the saga")?;
    match row {
        Some(row) => Ok(row.get(0)),
        None => bail!("no create-environment saga has the id {saga_id}"),
    }
}

/// One open saga the worker runs next.
#[derive(Debug)]
pub struct OpenSaga {
    pub saga_id: String,
    pub org: String,
    pub request: EnvironmentRequest,
    /// The first step that is not `completed`.
    pub next_step: i32,
}

/// The oldest `pending` or `running` create-environment saga of an org with
/// no other running saga, in one query.
pub async fn next_open_saga(client: &impl GenericClient) -> anyhow::Result<Option<OpenSaga>> {
    let row = client
        .query_opt(next_open_saga_sql(), &[])
        .await
        .context("read the next open saga")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let saga_id: String = row.get(0);
    let input: Value = row.get(2);
    let request = serde_json::from_value(input)
        .with_context(|| format!("decode the request of saga {saga_id}"))?;
    let next_step: Option<i32> = row.get(3);
    Ok(Some(OpenSaga {
        next_step: next_step
            .with_context(|| format!("saga {saga_id} is open but every step is completed"))?,
        org: row.get(1),
        saga_id,
        request,
    }))
}

/// Mark step `step` `running` and the saga `running` at that step.
pub async fn start_step(
    client: &impl GenericClient,
    saga_id: &str,
    step: i32,
) -> anyhow::Result<()> {
    client
        .execute(start_step_sql(), &[&saga_id, &step])
        .await
        .context("record the step start")?;
    Ok(())
}

/// Mark step `step` `completed`, with the detail the step reports.
pub async fn complete_step(
    client: &impl GenericClient,
    saga_id: &str,
    step: i32,
    detail: Option<&Value>,
) -> anyhow::Result<()> {
    client
        .execute(complete_step_sql(), &[&saga_id, &step, &detail])
        .await
        .context("record the step completion")?;
    Ok(())
}

/// Mark step `step` `failed` with `error`, and the saga `failed`. The caller
/// never passes PostgreSQL error Display text, which can carry personal data.
pub async fn fail_step(
    client: &impl GenericClient,
    saga_id: &str,
    step: i32,
    error: &str,
) -> anyhow::Result<()> {
    client
        .execute(fail_step_sql(), &[&saga_id, &step, &error])
        .await
        .context("record the step failure")?;
    Ok(())
}

/// Complete the last step with the operator commands in `detail`, and leave
/// the saga `awaiting-operator`, its end state.
pub async fn await_operator(
    client: &impl GenericClient,
    saga_id: &str,
    step: i32,
    detail: &Value,
) -> anyhow::Result<()> {
    client
        .execute(await_operator_sql(), &[&saga_id, &step, detail])
        .await
        .context("record the operator commands")?;
    Ok(())
}
