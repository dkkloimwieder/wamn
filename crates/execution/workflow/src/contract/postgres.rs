//! [`PostgresWorkflows`]: the workflow contract over the run plane.

use serde_json::Value;
use tokio::sync::Mutex;
use tokio_postgres::{Client, Transaction};
use wamn_catalog::{ServingManifest, WiringDocument};
use wamn_run_state::RunStatus;
use wamn_run_state::queue::{
    insert_automation_run_sql, insert_run_queue_sql, list_workflow_runs_sql, park_queued_run_sql,
    release_parked_run_sql, select_automation_run_sql, select_run_queue_state_sql,
};
use wamn_runtime::plugins::route_authentication::queued_service_caller;

use super::{StartRequest, Trigger, WorkflowError, WorkflowErrorKind, WorkflowRun, Workflows};

/// Why a park or a release changed nothing: the run's status, whether a queue
/// row exists, whether it is parked, and whether a replica holds a live lease.
struct QueueState {
    status: String,
    queued: bool,
    parked: bool,
    leased: bool,
}

/// The release a start reads its wiring from: the snapshot publish wrote.
const READ_RELEASE_SNAPSHOT_SQL: &str = "\
SELECT canonical_bytes FROM catalog.release_manifest_v3_snapshots \
 WHERE tenant_id = $1 AND effective_release_id = $2";

/// The workflow contract over one tenant and environment.
///
/// The client must hold project-admin authority: a start reads the catalog
/// and the service principal, and writes `runs` and `run_queue`.
pub struct PostgresWorkflows {
    client: Mutex<Client>,
    /// The run plane schema, for example `wamn_run`.
    schema: String,
    tenant: String,
    environment: String,
}

impl std::fmt::Debug for PostgresWorkflows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresWorkflows")
            .field("schema", &self.schema)
            .field("tenant", &self.tenant)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl PostgresWorkflows {
    /// Serve the runs of `tenant` and `environment` in `schema`.
    pub fn new(
        client: Client,
        schema: impl Into<String>,
        tenant: impl Into<String>,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            client: Mutex::new(client),
            schema: schema.into(),
            tenant: tenant.into(),
            environment: environment.into(),
        }
    }

    fn tenant(&self) -> &str {
        &self.tenant
    }

    fn environment(&self) -> &str {
        &self.environment
    }

    /// The minted release of the request, in this tenant and environment.
    async fn release(
        &self,
        transaction: &Transaction<'_>,
        release_id: i32,
    ) -> Result<ServingManifest, WorkflowError> {
        let bytes: Vec<u8> = transaction
            .query_opt(READ_RELEASE_SNAPSHOT_SQL, &[&self.tenant(), &release_id])
            .await
            .map_err(|error| WorkflowError::storage("read the release snapshot", error))?
            .ok_or_else(|| {
                WorkflowError::new(
                    WorkflowErrorKind::Refused,
                    format!("release {release_id} has no minted snapshot"),
                )
            })?
            .try_get(0)
            .map_err(|error| WorkflowError::storage("read the release bytes", error))?;
        let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes).map_err(|error| {
            WorkflowError::with_source(
                WorkflowErrorKind::Refused,
                "the release snapshot does not parse",
                error,
            )
        })?;
        if manifest.release.tenant_id != self.tenant()
            || manifest.release.environment != self.environment()
        {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Refused,
                format!("release {release_id} belongs to another tenant or environment"),
            ));
        }
        Ok(manifest)
    }

    /// Bind the transaction to the run plane and the release tenant.
    async fn scope(&self, transaction: &Transaction<'_>) -> Result<(), WorkflowError> {
        transaction
            .query_one(
                "SELECT set_config('search_path', $1, true), set_config('app.tenant', $2, true)",
                &[&self.schema, &self.tenant()],
            )
            .await
            .map_err(|error| WorkflowError::storage("scope the transaction", error))?;
        Ok(())
    }

    /// Check the request against the release and the catalog, and return the
    /// frozen wiring hash and the durability class.
    async fn admit(
        &self,
        transaction: &Transaction<'_>,
        release_id: i32,
        request: &StartRequest,
    ) -> Result<(String, String), WorkflowError> {
        let refused = |message: String| WorkflowError::new(WorkflowErrorKind::Refused, message);
        let release = self.release(transaction, release_id).await?;
        let wiring = release
            .workflow
            .wirings
            .iter()
            .find(|wiring| {
                wiring.package_id == request.package_id
                    && wiring.wiring_id == request.wiring_id
                    && wiring.wiring_version == request.wiring_version
            })
            .ok_or_else(|| {
                refused(format!(
                    "wiring {}/{} version {} is absent from the release",
                    request.package_id, request.wiring_id, request.wiring_version
                ))
            })?;
        let package = release
            .release
            .packages
            .iter()
            .find(|package| package.package_id() == request.package_id)
            .ok_or_else(|| {
                refused(format!(
                    "package {} is absent from the release",
                    request.package_id
                ))
            })?;
        let policy = transaction
            .query_opt(
                "SELECT durability_class FROM environment_policies \
                 WHERE tenant_id = $1 AND expected_environment = $2 FOR SHARE",
                &[&self.tenant(), &self.environment()],
            )
            .await
            .map_err(|error| WorkflowError::storage("read the environment policy", error))?
            .ok_or_else(|| refused("the environment policy is absent".to_owned()))?;
        let durability: String = policy
            .try_get(0)
            .map_err(|error| WorkflowError::storage("read the durability class", error))?;
        let version = i32::try_from(request.wiring_version)
            .map_err(|_| refused("the wiring version does not fit".to_owned()))?;
        let graph = transaction
            .query_opt(
                "SELECT graph_json::text FROM catalog.wirings \
                 WHERE tenant_id = $1 AND package_id = $2 AND wiring_id = $3 \
                   AND version = $4 AND wiring_hash = $5 AND package_version = $6",
                &[
                    &self.tenant(),
                    &request.package_id,
                    &request.wiring_id,
                    &version,
                    &wiring.graph_hash.as_str(),
                    &package.package_version(),
                ],
            )
            .await
            .map_err(|error| WorkflowError::storage("read the wiring", error))?
            .ok_or_else(|| refused("the released wiring is absent from the catalog".to_owned()))?;
        let graph: String = graph
            .try_get(0)
            .map_err(|error| WorkflowError::storage("read the wiring graph", error))?;
        let document = serde_json::from_str::<Value>(&graph)
            .map_err(anyhow::Error::from)
            .and_then(|graph| WiringDocument::parse(&graph).map_err(anyhow::Error::from))
            .map_err(|error| {
                WorkflowError::with_source(
                    WorkflowErrorKind::Refused,
                    "the catalog wiring does not parse",
                    error,
                )
            })?;
        if document.wiring_hash() != wiring.graph_hash {
            return Err(refused(
                "the catalog wiring hash differs from the release".to_owned(),
            ));
        }
        Ok((wiring.graph_hash.as_str().to_owned(), durability))
    }

    /// The facts that name why a park or a release changed nothing.
    async fn queue_state(
        &self,
        transaction: &Transaction<'_>,
        run_id: &str,
    ) -> Result<Option<QueueState>, WorkflowError> {
        let row = transaction
            .query_opt(
                &select_run_queue_state_sql(),
                &[&run_id, &self.environment()],
            )
            .await
            .map_err(|error| WorkflowError::storage("read the run's queue state", error))?;
        row.map(|row| {
            Ok(QueueState {
                status: row.try_get(0)?,
                queued: row.try_get(1)?,
                parked: row.try_get(2)?,
                leased: row.try_get(3)?,
            })
        })
        .transpose()
        .map_err(|error: tokio_postgres::Error| {
            WorkflowError::storage("decode the run's queue state", error)
        })
    }
}

#[async_trait::async_trait]
impl Workflows for PostgresWorkflows {
    async fn start(&self, request: &StartRequest) -> Result<String, WorkflowError> {
        if request.idempotency_key.is_empty() {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Refused,
                "the idempotency key is empty",
            ));
        }
        let Trigger::Automation {
            service_principal_id,
        } = &request.trigger;
        let release_id = i32::try_from(request.effective_release_id).map_err(|_| {
            WorkflowError::new(
                WorkflowErrorKind::Refused,
                "the effective release id does not fit",
            )
        })?;
        let version = i32::try_from(request.wiring_version).map_err(|_| {
            WorkflowError::new(
                WorkflowErrorKind::Refused,
                "the wiring version does not fit",
            )
        })?;
        let input = serde_json::to_string(&request.input)
            .map_err(|error| WorkflowError::storage("encode the input", error))?;
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the start", error))?;
        self.scope(&transaction).await?;
        let (wiring_hash, durability) = self.admit(&transaction, release_id, request).await?;
        let caller = queued_service_caller(&transaction, self.tenant(), service_principal_id)
            .await
            .map_err(|error| {
                WorkflowError::with_source(
                    WorkflowErrorKind::Refused,
                    "the service principal does not admit the start",
                    error,
                )
            })?;
        let principal = caller.principal_id();
        let inserted = transaction
            .query_opt(
                &insert_automation_run_sql(),
                &[
                    &self.tenant(),
                    &request.package_id,
                    &release_id,
                    &self.environment(),
                    &request.wiring_id,
                    &version,
                    &wiring_hash,
                    &principal,
                    &request.idempotency_key,
                    &input,
                    &durability,
                ],
            )
            .await
            .map_err(|error| WorkflowError::storage("admit the run", error))?;
        let run_id: String = if let Some(row) = inserted {
            let run_id: String = row
                .try_get(0)
                .map_err(|error| WorkflowError::storage("read the run id", error))?;
            transaction
                .execute(insert_run_queue_sql(), &[&self.tenant(), &run_id])
                .await
                .map_err(|error| WorkflowError::storage("queue the run", error))?;
            run_id
        } else {
            transaction
                .query_opt(
                    select_automation_run_sql(),
                    &[
                        &self.tenant(),
                        &request.package_id,
                        &release_id,
                        &self.environment(),
                        &request.wiring_id,
                        &version,
                        &wiring_hash,
                        &principal,
                        &request.idempotency_key,
                        &input,
                    ],
                )
                .await
                .map_err(|error| WorkflowError::storage("read the first run", error))?
                .ok_or_else(|| {
                    WorkflowError::new(
                        WorkflowErrorKind::Conflict,
                        "the idempotency key was used for a different request",
                    )
                })?
                .try_get(0)
                .map_err(|error| WorkflowError::storage("read the first run id", error))?
        };
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("commit the start", error))?;
        Ok(run_id)
    }

    async fn park(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the park", error))?;
        self.scope(&transaction).await?;
        let parked = transaction
            .query_opt(&park_queued_run_sql(), &[&run_id, &self.environment()])
            .await
            .map_err(|error| WorkflowError::storage("park the run", error))?;
        if parked.is_none() {
            match self.queue_state(&transaction, run_id).await? {
                None => {
                    return Err(WorkflowError::new(
                        WorkflowErrorKind::NotFound,
                        format!("run {run_id} does not exist"),
                    ));
                }
                Some(QueueState {
                    queued: true,
                    parked: true,
                    ..
                }) => {}
                Some(QueueState {
                    status,
                    queued,
                    leased,
                    ..
                }) => {
                    let state = if !queued {
                        "finished"
                    } else if leased {
                        "running"
                    } else {
                        "not dispatched"
                    };
                    return Err(WorkflowError::new(
                        WorkflowErrorKind::NotParkable,
                        format!(
                            "run {run_id} is {state} (status {status}), so it cannot be parked"
                        ),
                    ));
                }
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("commit the park", error))
    }

    async fn release(&self, run_id: &str) -> Result<(), WorkflowError> {
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the release", error))?;
        self.scope(&transaction).await?;
        let released = transaction
            .query_opt(&release_parked_run_sql(), &[&run_id, &self.environment()])
            .await
            .map_err(|error| WorkflowError::storage("release the run", error))?;
        if released.is_none() {
            return Err(match self.queue_state(&transaction, run_id).await? {
                None => WorkflowError::new(
                    WorkflowErrorKind::NotFound,
                    format!("run {run_id} does not exist"),
                ),
                Some(_) => WorkflowError::new(
                    WorkflowErrorKind::NotParked,
                    format!("run {run_id} is not parked"),
                ),
            });
        }
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("commit the release", error))
    }

    async fn list(&self, limit: u32) -> Result<Vec<WorkflowRun>, WorkflowError> {
        let limit = i64::from(limit);
        let mut client = self.client.lock().await;
        let transaction = client
            .transaction()
            .await
            .map_err(|error| WorkflowError::storage("begin the list", error))?;
        self.scope(&transaction).await?;
        let rows = transaction
            .query(&list_workflow_runs_sql(), &[&self.environment(), &limit])
            .await
            .map_err(|error| WorkflowError::storage("list the runs", error))?;
        let runs = rows
            .iter()
            .map(|row| {
                let decode = |error| WorkflowError::storage("decode a run", error);
                let version: i32 = row.try_get(3).map_err(decode)?;
                let status: String = row.try_get(5).map_err(decode)?;
                Ok(WorkflowRun {
                    run_id: row.try_get(0).map_err(decode)?,
                    package_id: row.try_get(1).map_err(decode)?,
                    wiring_id: row.try_get(2).map_err(decode)?,
                    wiring_version: u32::try_from(version).map_err(|_| {
                        WorkflowError::new(
                            WorkflowErrorKind::Storage,
                            "a wiring version is negative",
                        )
                    })?,
                    trigger_source: row.try_get(4).map_err(decode)?,
                    status: RunStatus::from_sql(&status).ok_or_else(|| {
                        WorkflowError::new(
                            WorkflowErrorKind::Storage,
                            format!("run status {status:?} is unknown"),
                        )
                    })?,
                    queued: row.try_get(6).map_err(decode)?,
                    parked: row.try_get(7).map_err(decode)?,
                    created_at: row.try_get(8).map_err(decode)?,
                })
            })
            .collect::<Result<Vec<_>, WorkflowError>>()?;
        transaction
            .commit()
            .await
            .map_err(|error| WorkflowError::storage("end the list", error))?;
        Ok(runs)
    }
}
