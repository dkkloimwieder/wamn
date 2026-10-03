//! The create-environment saga (`docs/plan/platform-ui.md` §5.2 and §5.5,
//! `wamn-zua8.3`).
//!
//! The `environment.create` route writes one saga with its request in
//! `input` and every step in `provisioning.saga_steps`, so `environment.list`
//! shows the whole chain from the start. `wamn-ctl serve` alone updates a
//! saga and its steps. It runs the steps in [`STEPS`] order, one saga per org
//! at a time. A failure leaves the step `failed` with its error, and the saga
//! `failed`. The last step writes the operator commands into its `detail` and
//! leaves the saga `awaiting-operator`. [`saga_resume`] and [`saga_abandon`]
//! are the two ways out of a failure.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_postgres::{Client, GenericClient, Transaction};

use crate::bind_connection::RequirementType;

/// The saga type of `provisioning.sagas`.
pub const CREATE_ENVIRONMENT: &str = "create-environment";

/// The steps of a create-environment saga, in the order the worker runs them
/// (owner rulings of 2026-10-03 on `wamn-zua8.3`). Step `n` is `STEPS[n - 1]`.
pub const STEPS: [&str; 14] = [
    "provision-project-env",
    "reconcile-run-plane",
    "apply-packages",
    "reconcile-package-data-access",
    "enable-cdc",
    "wait-publication",
    "admit-components",
    "publish-release",
    "bind-connection",
    "push-release-manifest",
    "select-release",
    "upload-ui",
    "materialize-admin-grants",
    "awaiting-operator",
];

/// The request of one create-environment saga, kept in `input`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequest {
    pub project: String,
    pub env: String,
    pub tenant: String,
    /// The hostname applied to every HTTP route of the release.
    pub route_host: String,
    /// Package artifacts that `push-package` pushed.
    pub packages: Vec<PackageReference>,
    pub connections: Vec<ConnectionRequest>,
}

/// One package artifact `<package_id>-<version>`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub package_id: String,
    pub version: String,
}

/// One connection that `bind-connection` binds to the store alias `alias`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    pub instance_id: String,
    pub requirement_type: RequirementType,
    pub alias: String,
    /// The non-secret definition, a JSON object.
    pub definition: Value,
}

/// Write one create-environment saga and all of its steps, `pending`.
/// Returns the new saga id.
pub async fn create_environment_saga(
    transaction: &Transaction<'_>,
    org: &str,
    request: &EnvironmentRequest,
) -> anyhow::Result<String> {
    let saga_id = uuid::Uuid::new_v4().to_string();
    let target = format!("{org}/{}/{}", request.project, request.env);
    let input = serde_json::to_value(request).context("encode the saga request")?;
    let total_steps = i32::try_from(STEPS.len()).context("count the steps")?;
    transaction
        .execute(
            "INSERT INTO provisioning.sagas (saga_id, type, target, total_steps, org, input) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &saga_id,
                &CREATE_ENVIRONMENT,
                &target,
                &total_steps,
                &org,
                &input,
            ],
        )
        .await
        .context("insert the saga")?;
    let steps: Vec<i32> = (1..=total_steps).collect();
    let names: Vec<&str> = STEPS.to_vec();
    transaction
        .execute(
            "INSERT INTO provisioning.saga_steps (saga_id, step, name) \
             SELECT $1, step, name FROM unnest($2::int[], $3::text[]) AS s (step, name)",
            &[&saga_id, &steps, &names],
        )
        .await
        .context("insert the saga steps")?;
    Ok(saga_id)
}

/// Return a `failed` saga to `pending`: its failed step becomes `pending`
/// again, without its error and times, and the worker runs it next.
pub async fn saga_resume(client: &mut Client, saga_id: &str) -> anyhow::Result<()> {
    let transaction = client.transaction().await.context("open the transaction")?;
    let status = lock_saga(&transaction, saga_id).await?;
    if status != "failed" {
        bail!("saga {saga_id} is {status}; only a failed saga resumes");
    }
    transaction
        .execute(
            "UPDATE provisioning.saga_steps \
             SET status = 'pending', error = NULL, started_at = NULL, finished_at = NULL \
             WHERE saga_id = $1 AND status = 'failed'",
            &[&saga_id],
        )
        .await
        .context("return the failed step to pending")?;
    transaction
        .execute(
            "UPDATE provisioning.sagas \
             SET status = 'pending', last_error = NULL, updated_at = now() \
             WHERE saga_id = $1",
            &[&saga_id],
        )
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
        .execute(
            "UPDATE provisioning.sagas SET status = 'abandoned', updated_at = now() \
             WHERE saga_id = $1",
            &[&saga_id],
        )
        .await
        .context("abandon the saga")?;
    transaction.commit().await.context("commit the abandon")
}

async fn lock_saga(transaction: &Transaction<'_>, saga_id: &str) -> anyhow::Result<String> {
    let row = transaction
        .query_opt(
            "SELECT status FROM provisioning.sagas \
             WHERE saga_id = $1 AND type = $2 FOR UPDATE",
            &[&saga_id, &CREATE_ENVIRONMENT],
        )
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
        .query_opt(
            "SELECT s.saga_id, s.org, s.input, \
                    (SELECT min(t.step) FROM provisioning.saga_steps t \
                      WHERE t.saga_id = s.saga_id AND t.status <> 'completed') \
               FROM provisioning.sagas s \
              WHERE s.type = $1 AND s.status IN ('pending', 'running') \
                AND NOT EXISTS ( \
                    SELECT FROM provisioning.sagas o \
                     WHERE o.org = s.org AND o.saga_id <> s.saga_id AND o.status = 'running') \
              ORDER BY s.created_at, s.saga_id \
              LIMIT 1",
            &[&CREATE_ENVIRONMENT],
        )
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
        .execute(
            "WITH saga AS ( \
               UPDATE provisioning.sagas SET status = 'running', step = $2, updated_at = now() \
                WHERE saga_id = $1) \
             UPDATE provisioning.saga_steps \
                SET status = 'running', error = NULL, started_at = now(), finished_at = NULL \
              WHERE saga_id = $1 AND step = $2",
            &[&saga_id, &step],
        )
        .await
        .context("record the step start")?;
    Ok(())
}

/// Mark step `step` `completed`.
pub async fn complete_step(
    client: &impl GenericClient,
    saga_id: &str,
    step: i32,
) -> anyhow::Result<()> {
    client
        .execute(
            "UPDATE provisioning.saga_steps SET status = 'completed', finished_at = now() \
              WHERE saga_id = $1 AND step = $2",
            &[&saga_id, &step],
        )
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
        .execute(
            "WITH saga AS ( \
               UPDATE provisioning.sagas SET status = 'failed', last_error = $3, updated_at = now() \
                WHERE saga_id = $1) \
             UPDATE provisioning.saga_steps \
                SET status = 'failed', error = $3, finished_at = now() \
              WHERE saga_id = $1 AND step = $2",
            &[&saga_id, &step, &error],
        )
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
        .execute(
            "WITH saga AS ( \
               UPDATE provisioning.sagas SET status = 'awaiting-operator', updated_at = now() \
                WHERE saga_id = $1) \
             UPDATE provisioning.saga_steps \
                SET status = 'completed', detail = $3, finished_at = now() \
              WHERE saga_id = $1 AND step = $2",
            &[&saga_id, &step, detail],
        )
        .await
        .context("record the operator commands")?;
    Ok(())
}
