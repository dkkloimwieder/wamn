//! HTTP and device calls use one operation boundary over the edge's durable store.

use crate::application::EdgeApplication;
use crate::policy::EdgeFacts;
use crate::release::EdgeRelease;
use crate::store::{Claim, EdgeStore};
use anyhow::Context as _;
use std::collections::HashSet;
use std::sync::Arc;
use wamn_engine::entry_operation::operation::{self, Begun, Refusal};
use wamn_engine::entry_operation::postgres_types::PgError;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::native_routes::{authorize_registered_operation, bounded_node_deadline_ms};
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wash_runtime::engine::ctx::{ActiveCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, HasData, Resource, ResourceTable};
use wash_runtime::wit::{WitInterface, WitWorld};

pub const EDGE_OPERATION_ID: &str = "wamn-edge-operation";

#[derive(Debug)]
pub struct EdgeOperation {
    release: Arc<EdgeRelease>,
    application: Arc<EdgeApplication>,
    store: EdgeStore,
    org: String,
    scopes: Arc<wamn_engine::invocation_scope::InvocationScopes>,
}

#[derive(Debug, Clone)]
struct AuthorizedCall {
    operation: String,
    caller: Option<AuthenticatedCaller>,
}

#[derive(Debug)]
struct Open {
    authorized: AuthorizedCall,
    claim: Option<i64>,
    settled: bool,
}

impl EdgeOperation {
    pub fn new(
        release: Arc<EdgeRelease>,
        application: Arc<EdgeApplication>,
        store: EdgeStore,
        org: String,
        scopes: Arc<wamn_engine::invocation_scope::InvocationScopes>,
    ) -> Self {
        Self {
            release,
            application,
            store,
            org,
            scopes,
        }
    }

    fn authorize(
        &self,
        caller: Option<AuthenticatedCaller>,
        operation: String,
        write: bool,
    ) -> Result<AuthorizedCall, Refusal> {
        let manifest = self.release.release().manifest();
        let fact = manifest
            .components
            .iter()
            .find_map(|component| component.descriptor.operations.get(&operation))
            .ok_or(Refusal::UnknownOperation)?;
        if write
            && manifest
                .routes
                .iter()
                .any(|route| route.operation == operation && route.type_.is_read())
        {
            return Err(Refusal::ReadsClaimNothing);
        }
        authorize_registered_operation(caller.as_ref(), fact.registered_operation.as_deref())
            .map_err(|_| Refusal::GrantDenied)?;
        if caller
            .as_ref()
            .is_some_and(|caller| manifest.attachment(caller.attachment_id()).is_none())
        {
            return Err(Refusal::InvalidCaller);
        }
        Ok(AuthorizedCall { operation, caller })
    }

    async fn begin(
        &self,
        authorized: AuthorizedCall,
        key: &str,
        request: &[u8],
    ) -> Result<(Open, Begun), Refusal> {
        let claim = self
            .store
            .claim(&self.org, &authorized.operation, key, request)
            .await
            .map_err(|error| storage_refusal(&error))?;
        let (claim, begun, settled) = match claim {
            Claim::New(id) => (
                Some(id),
                Begun::New(uuid::Uuid::new_v4().to_string()),
                false,
            ),
            Claim::Finished(result) => (None, Begun::Finished(result), true),
            Claim::InProgress => return Err(Refusal::InProgress),
            Claim::OutcomeUnknown => return Err(Refusal::OutcomeUnknown),
            Claim::Conflict => return Err(Refusal::Conflict),
        };
        Ok((
            Open {
                authorized,
                claim,
                settled,
            },
            begun,
        ))
    }

    async fn invoke(
        &self,
        authorized: AuthorizedCall,
        context: node_types::NodeContext,
        input: &serde_json::Value,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let components = self.release.components();
        let component = components
            .iter()
            .find(|component| component.operations.contains_key(&authorized.operation))
            .context("operation is outside the release")?;
        invoke_operation(
            &*self.application,
            OperationCall {
                closure: OperationClosure::Released(components),
                component,
                operation: &authorized.operation,
                deadline_ms: bounded_node_deadline_ms(context.deadline_ms),
                context,
                input,
                facts: EdgeFacts::entry(authorized.caller),
            },
        )
        .await
    }

    /// The device calls the same admission, claim, typed export and completion as HTTP.
    pub(crate) async fn sample(
        &self,
        caller: AuthenticatedCaller,
        key: &str,
        captured_at: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let attachment = self
            .release
            .release()
            .manifest()
            .attachment(caller.attachment_id())
            .context("device attachment is missing")?;
        let wamn_catalog::AttachmentTarget::Route { operation, .. } = attachment.target();
        let authorized = self
            .authorize(Some(caller), operation, true)
            .map_err(|error| anyhow::anyhow!("device admission refused: {error:?}"))?;
        let mut body = input[0].clone();
        body.as_object_mut()
            .context("device request is not an object")?
            .remove("request_id");
        let request = wamn_digest::canonical_json_sha256(&body);
        let (open, begun) = self
            .begin(authorized, key, request.as_bytes())
            .await
            .map_err(|error| anyhow::anyhow!("device claim refused: {error:?}"))?;
        let Begun::New(delivery_id) = begun else {
            return Ok(());
        };
        let context = node_types::NodeContext {
            wiring_id: String::new(),
            wiring_version: 0,
            node_id: String::new(),
            delivery_id,
            input_port: None,
            occurrence: 0,
            traceparent: None,
            tracestate: None,
            deadline_ms: Some(bounded_node_deadline_ms(None)),
            config: "null".into(),
        };
        let id = open.claim.context("new device call has a claim")?;
        let outcome = self.invoke(open.authorized, context, input).await?;
        let emission = match outcome {
            Ok(emission) => emission,
            Err(error) => {
                self.store.release(id).await?;
                anyhow::bail!("device operation refused: {error:?}");
            }
        };
        let mut results: Vec<serde_json::Value> = serde_json::from_str(&emission.payload)?;
        anyhow::ensure!(
            results.len() == 1 && results[0]["request_id"].as_str() == Some(key),
            "device result does not match its request"
        );
        let mut result = results.remove(0);
        result
            .as_object_mut()
            .context("device result is not an object")?
            .remove("request_id");
        if result.get("error").is_some() {
            self.store.release(id).await?;
            anyhow::bail!("device operation refused the sample");
        }
        let Some(body) = result.get("value") else {
            self.store.release(id).await?;
            anyhow::bail!("device operation returned no sample");
        };
        self.store
            .complete(
                id,
                &serde_json::to_string(&result)?,
                Some((captured_at, body)),
            )
            .await
    }
}

#[async_trait::async_trait]
impl HostPlugin for EdgeOperation {
    fn id(&self) -> &'static str {
        EDGE_OPERATION_ID
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:operation/operation@0.1.0")]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.scopes.revoke_workload(workload);
        Ok(())
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.scopes
            .bind_release(item.id(), Arc::clone(self.release.release()));
        item.add_plugin(
            wamn_engine::invocation_scope::INVOCATION_SCOPES_ID,
            Arc::clone(&self.scopes) as _,
        );
        if interfaces.contains("wamn", "operation", &["operation"]) {
            operation::add_to_linker::<_, EdgeOperationHost>(item.linker()?, extract_active_ctx)?;
        }
        Ok(())
    }
}

struct EdgeOperationHost;
impl HasData for EdgeOperationHost {
    type Data<'a> = ActiveCtx<'a>;
}

#[derive(Debug, Default, Clone)]
struct StoreOperation(Arc<tokio::sync::Mutex<Option<Open>>>);
fn state(table: &mut ResourceTable) -> wash_runtime::wasmtime::Result<StoreOperation> {
    if !table.iter_mut().any(|entry| entry.is::<StoreOperation>()) {
        table.push(StoreOperation::default())?;
    }
    Ok(table
        .iter_mut()
        .find_map(|entry| entry.downcast_ref::<StoreOperation>())
        .expect("store operation exists")
        .clone())
}
fn plugin<T: 'static + Send>(
    accessor: &Accessor<T, EdgeOperationHost>,
) -> wash_runtime::wasmtime::Result<(Arc<EdgeOperation>, StoreOperation)> {
    accessor.with(|mut access| {
        let ctx = access.get();
        Ok((
            ctx.try_get_plugin::<EdgeOperation>(EDGE_OPERATION_ID)?,
            state(ctx.table)?,
        ))
    })
}
fn storage_refusal(error: &anyhow::Error) -> Refusal {
    tracing::warn!(%error, "edge storage failed");
    Refusal::Postgres(PgError::QueryError(("58030".into(), error.to_string())))
}

impl<T: 'static + Send> operation::HostWithStore<T> for EdgeOperationHost {
    async fn invoke(
        accessor: &Accessor<T, Self>,
        operation: String,
        context: node_types::NodeContext,
        input: String,
    ) -> wash_runtime::wasmtime::Result<Result<node_types::Emission, node_types::NodeError>> {
        let (plugin, state) = plugin(accessor)?;
        let authorized = state
            .0
            .lock()
            .await
            .as_ref()
            .filter(|open| !open.settled && open.authorized.operation == operation)
            .map(|open| open.authorized.clone())
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("operation is not authorized"))?;
        let input = serde_json::from_str(&input)
            .map_err(|error| wash_runtime::wasmtime::Error::from_anyhow(error.into()))?;
        plugin
            .invoke(authorized, context, &input)
            .await
            .map_err(wash_runtime::wasmtime::Error::from_anyhow)
    }
    fn sign_result(
        _accessor: &Accessor<T, Self>,
        payload: String,
    ) -> impl Future<Output = wash_runtime::wasmtime::Result<Result<String, node_types::NodeError>>>
    {
        std::future::ready(Ok(Ok(payload)))
    }
    async fn authorize(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        enter(accessor, caller, operation, None)
            .await
            .map(|answer| answer.map(drop))
    }
    async fn begin_operation(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
        key: String,
        request: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
        enter(accessor, caller, operation, Some((key, request))).await
    }
    async fn finish_read(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let (_, state) = plugin(accessor)?;
        let mut current = state.0.lock().await;
        let Some(open) = current
            .as_mut()
            .filter(|open| !open.settled && open.claim.is_none())
        else {
            return Ok(Err(Refusal::NotBegun));
        };
        open.settled = true;
        Ok(Ok(()))
    }
    async fn commit(
        accessor: &Accessor<T, Self>,
        result: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let (plugin, state) = plugin(accessor)?;
        let mut current = state.0.lock().await;
        let Some(open) = current
            .as_mut()
            .filter(|open| !open.settled && open.claim.is_some())
        else {
            return Ok(Err(Refusal::NotBegun));
        };
        open.settled = true;
        Ok(plugin
            .store
            .complete(open.claim.expect("write has a claim"), &result, None)
            .await
            .map_err(|error| storage_refusal(&error)))
    }
    async fn rollback(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let (plugin, state) = plugin(accessor)?;
        let mut current = state.0.lock().await;
        let Some(open) = current
            .as_mut()
            .filter(|open| !open.settled && open.claim.is_some())
        else {
            return Ok(Err(Refusal::NotBegun));
        };
        open.settled = true;
        Ok(plugin
            .store
            .release(open.claim.expect("write has a claim"))
            .await
            .map_err(|error| storage_refusal(&error)))
    }
    async fn wal_position(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<String, Refusal>> {
        let (plugin, state) = plugin(accessor)?;
        if state
            .0
            .lock()
            .await
            .as_ref()
            .is_none_or(|open| open.settled)
        {
            return Ok(Err(Refusal::NotBegun));
        }
        Ok(plugin
            .store
            .revision()
            .await
            .map_err(|error| storage_refusal(&error)))
    }
}
async fn enter<T: 'static + Send>(
    accessor: &Accessor<T, EdgeOperationHost>,
    caller: Option<Resource<AuthenticatedCaller>>,
    operation: String,
    claim: Option<(String, Vec<u8>)>,
) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
    let (plugin, state) = plugin(accessor)?;
    let mut current = state.0.lock().await;
    if current.as_ref().is_some_and(|open| !open.settled) {
        return Ok(Err(Refusal::AlreadyBegun));
    }
    let caller = accessor.with(|mut access| {
        caller
            .map(|caller| access.get().table.get(&caller).cloned())
            .transpose()
    });
    let Ok(caller) = caller else {
        return Ok(Err(Refusal::InvalidCaller));
    };
    let authorized = match plugin.authorize(caller, operation, claim.is_some()) {
        Ok(authorized) => authorized,
        Err(error) => return Ok(Err(error)),
    };
    let (open, begun) = match claim {
        Some((key, request)) => match plugin.begin(authorized, &key, &request).await {
            Ok(answer) => answer,
            Err(error) => return Ok(Err(error)),
        },
        None => (
            Open {
                authorized,
                claim: None,
                settled: false,
            },
            Begun::New(String::new()),
        ),
    };
    *current = Some(open);
    Ok(Ok(begun))
}
