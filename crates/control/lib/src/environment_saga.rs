//! The worker's records of the create-environment and copy-environment sagas
//! (`docs/plan/platform-ui.md` §5.2, §5.3 and §5.5, `wamn-zua8.3`, `wamn-zua8.4`).
//!
//! The `environment.create` and `environment.copy` routes write one saga with its request in
//! `input` and every step in `provisioning.saga_steps`, so `environment.list`
//! shows the whole chain from the start. The request types, the steps and the
//! SQL text live in `wamn_control_provision::saga`, so the control host carries
//! no code of this crate. `wamn-ctl serve` alone runs a saga and updates its
//! steps, with the functions here. It runs the steps in [`STEPS`] or [`COPY_STEPS`] order, one saga per
//! org at a time. A failure leaves the step `failed` with its error, and the
//! saga `failed`. The last step writes the operator commands into its `detail`
//! and leaves the saga `awaiting-operator`. [`saga_resume`] and
//! [`saga_abandon`] are the two ways out of a failure. The routes
//! `environment.resume` and `environment.abandon` write the same saga
//! statuses with the same SQL, and never a step.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, GenericClient, Transaction};
use wamn_catalog::RequirementType;
use wamn_control_provision::saga::{
    COPY_ENVIRONMENT, ConnectionReplacement, ConnectionRequest, CopyRequest, EnvironmentRequest,
    SourceRead, abandon_saga_sql, await_operator_sql, complete_step_sql, fail_step_sql,
    lock_environment_saga_sql, next_open_saga_sql, resume_saga_sql, start_step_sql,
    step_detail_sql,
};

#[cfg(doc)]
use wamn_control_provision::saga::{COPY_STEPS, STEPS};

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
        None => bail!("no create or copy saga has the id {saga_id}"),
    }
}

/// One open saga the worker runs next.
#[derive(Debug)]
pub struct OpenSaga {
    pub saga_id: String,
    pub org: String,
    pub request: SagaRequest,
    /// The first step that is not `completed`.
    pub next_step: i32,
}

/// The request of a create or a copy saga.
#[derive(Debug)]
pub enum SagaRequest {
    Create(EnvironmentRequest),
    Copy(CopyRequest),
}

/// The oldest `pending` or `running` create or copy saga of an org with no
/// other running saga, in one query.
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
    let decode = || format!("decode the request of saga {saga_id}");
    let request = if row.get::<_, String>(4) == COPY_ENVIRONMENT {
        SagaRequest::Copy(serde_json::from_value(input).with_context(decode)?)
    } else {
        SagaRequest::Create(serde_json::from_value(input).with_context(decode)?)
    };
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

/// What `read-source` recorded in the detail of step 1 of a copy saga.
pub async fn read_source_detail(
    client: &impl GenericClient,
    saga_id: &str,
) -> anyhow::Result<SourceRead> {
    let detail: Option<Value> = client
        .query_opt(step_detail_sql(), &[&saga_id, &1_i32])
        .await
        .context("read the detail of read-source")?
        .and_then(|row| row.get(0));
    serde_json::from_value(detail.context("read-source of the saga is not completed")?)
        .context("decode the detail of read-source")
}

/// One connection that the head release of the source environment binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceConnection {
    pub instance_id: String,
    pub alias: String,
    pub requirement_type: RequirementType,
    pub definition: Value,
    pub credential_handle: Option<String>,
}

/// The connections of a copy: each source connection, with the definition
/// of its replacement when the request names it (owner rulings of
/// 2026-10-04 on `wamn-zua8.4`). It fails with every problem at once: a
/// replacement that names no source connection, a source connection with a
/// credential and no replacement, and a replacement that `bind-connection`
/// refuses with no credential handle.
pub fn apply_replacements(
    source: Vec<SourceConnection>,
    replacements: &[ConnectionReplacement],
) -> anyhow::Result<Vec<ConnectionRequest>> {
    let mut problems = Vec::new();
    let mut named = BTreeMap::new();
    for replacement in replacements {
        if named
            .insert(replacement.instance_id.as_str(), &replacement.definition)
            .is_some()
        {
            problems.push(format!(
                "the request replaces the connection {} twice",
                replacement.instance_id
            ));
        }
    }
    let unknown: Vec<&str> = named
        .keys()
        .copied()
        .filter(|id| !source.iter().any(|each| each.instance_id == *id))
        .collect();
    if !unknown.is_empty() {
        problems.push(format!(
            "the source environment has no connection {}",
            unknown.join(", ")
        ));
    }
    let shared: Vec<String> = source
        .iter()
        .filter(|each| {
            each.credential_handle.is_some() && !named.contains_key(each.instance_id.as_str())
        })
        .map(|each| format!("{} (alias {})", each.instance_id, each.alias))
        .collect();
    if !shared.is_empty() {
        problems.push(format!(
            "the source connections {} use a credential, and the request gives them no new \
             definition",
            shared.join(", ")
        ));
    }
    let mut connections = Vec::new();
    for each in source {
        let definition = match named.get(each.instance_id.as_str()) {
            Some(definition) => {
                if let Err(reason) =
                    each.requirement_type
                        .check_definition(definition)
                        .and_then(|()| {
                            each.requirement_type
                                .check_credential_handle(definition, None)
                        })
                {
                    problems.push(format!(
                        "the definition of {} is refused: {reason}",
                        each.instance_id
                    ));
                }
                (*definition).clone()
            }
            None => each.definition,
        };
        connections.push(ConnectionRequest {
            instance_id: each.instance_id,
            requirement_type: each.requirement_type,
            alias: each.alias,
            definition,
        });
    }
    if !problems.is_empty() {
        bail!("{}", problems.join(". "));
    }
    Ok(connections)
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn source(instance_id: &str, alias: &str, credential: bool) -> SourceConnection {
        SourceConnection {
            instance_id: instance_id.to_owned(),
            alias: alias.to_owned(),
            requirement_type: RequirementType::Blobstore,
            definition: if credential {
                json!({"provider": "s3", "endpoint": "https://s3.example.test", "container": "b", "prefix": "p"})
            } else {
                json!({"provider": "gcs", "container": instance_id, "prefix": "p"})
            },
            credential_handle: credential.then(|| "shared".to_owned()),
        }
    }

    fn replace(instance_id: &str, definition: Value) -> ConnectionReplacement {
        ConnectionReplacement {
            instance_id: instance_id.to_owned(),
            definition,
        }
    }

    #[test]
    fn a_listed_connection_is_replaced_and_an_unlisted_one_keeps_its_definition() {
        let new = json!({"provider": "gcs", "container": "fresh", "prefix": "q"});
        let connections = apply_replacements(
            vec![
                source("labels", "label-store", true),
                source("photos", "photo-store", false),
            ],
            &[replace("labels", new.clone())],
        )
        .expect("the replacement applies");
        assert_eq!(
            connections,
            vec![
                ConnectionRequest {
                    instance_id: "labels".to_owned(),
                    requirement_type: RequirementType::Blobstore,
                    alias: "label-store".to_owned(),
                    definition: new,
                },
                ConnectionRequest {
                    instance_id: "photos".to_owned(),
                    requirement_type: RequirementType::Blobstore,
                    alias: "photo-store".to_owned(),
                    definition: json!({"provider": "gcs", "container": "photos", "prefix": "p"}),
                },
            ]
        );
    }

    #[test]
    fn every_problem_is_named_at_once() {
        let error = apply_replacements(
            vec![
                source("labels", "label-store", true),
                source("photos", "photo-store", true),
                source("scans", "scan-store", false),
            ],
            &[
                replace(
                    "missing",
                    json!({"provider": "gcs", "container": "c", "prefix": "p"}),
                ),
                replace("scans", json!({"provider": "gcs", "prefix": "p"})),
            ],
        )
        .expect_err("the replacements are refused");
        assert_eq!(
            error.to_string(),
            "the source environment has no connection missing. the source connections labels \
             (alias label-store), photos (alias photo-store) use a credential, and the request \
             gives them no new definition. the definition of scans is refused: the generation \
             definition lacks container; Blobstore reads it at resolve time"
        );
    }

    #[test]
    fn a_replacement_that_needs_a_credential_is_refused() {
        let error = apply_replacements(
            vec![source("labels", "label-store", true)],
            &[replace(
                "labels",
                json!({"provider": "s3", "endpoint": "https://s3.example.test", "container": "b", "prefix": "p"}),
            )],
        )
        .expect_err("the replacement needs a credential");
        assert_eq!(
            error.to_string(),
            "the definition of labels is refused: the credential handle must not be empty; the \
             host resolves it by name"
        );
    }
}
