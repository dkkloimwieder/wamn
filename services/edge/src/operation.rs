//! The edge's `wamn:operation` host, which serves reads only.
//!
//! The route guest calls this interface for each route that names a sealed
//! operation. `authorize` checks the caller and the grant of the operation
//! against the loaded release, as the cloud host does before it touches a
//! connection, and `finish-read` ends that read. The box has no SQL, so a read
//! binds no statement authority. `begin-operation` claims nothing and opens no
//! operation, so `commit`, `rollback` and `wal-position` refuse with
//! `not-begun`. The SQLite implementation replaces this (wamn-2bnru).

use std::collections::HashSet;
use std::sync::Arc;

use wamn_engine::entry_operation::operation::{self, Begun, Refusal};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::router_delivery::authorize_registered_operation;
use wash_runtime::engine::ctx::{ActiveCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, HasData, Resource, ResourceTable};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::release::EdgeRelease;

/// Host-plugin identity of the edge's operation host.
pub const EDGE_OPERATION_ID: &str = "wamn-edge-operation";

/// The edge's `wamn:operation` host over the loaded release.
#[derive(Debug)]
pub struct EdgeOperation {
    release: Arc<EdgeRelease>,
}

impl EdgeOperation {
    /// Serve the operation import for the operations of `release`.
    pub fn new(release: Arc<EdgeRelease>) -> Self {
        Self { release }
    }

    /// The checks of a call that the cloud host makes before it touches a
    /// connection, in its order. A `begin` of a read claims nothing.
    fn admit(
        &self,
        caller: Option<&AuthenticatedCaller>,
        operation: &str,
        begin: bool,
    ) -> Result<(), Refusal> {
        let manifest = self.release.release().manifest();
        let fact = manifest
            .components
            .iter()
            .find_map(|component| component.operations.get(operation))
            .ok_or(Refusal::UnknownOperation)?;
        let read = manifest
            .routes
            .iter()
            .any(|route| route.operation == operation && route.type_.is_read());
        if begin && read {
            return Err(Refusal::ReadsClaimNothing);
        }
        authorize_registered_operation(caller, fact.registered_operation.as_deref())
            .map_err(|_| Refusal::GrantDenied)?;
        // A present handle was issued for an attachment of this loaded release.
        if let Some(caller) = caller
            && manifest.attachment(caller.attachment_id()).is_none()
        {
            return Err(Refusal::InvalidCaller);
        }
        Ok(())
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

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "operation", &["operation"]) {
            return Ok(());
        }
        operation::add_to_linker::<_, EdgeOperationHost>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

/// The [`HasData`] marker of the edge's operation host.
struct EdgeOperationHost;

impl HasData for EdgeOperationHost {
    type Data<'a> = ActiveCtx<'a>;
}

/// The operation state of one Store: whether an authorized read is open.
#[derive(Debug, Default)]
struct StoreRead {
    open: bool,
}

/// The Store's read state, created on first use.
fn store_read(table: &mut ResourceTable) -> wash_runtime::wasmtime::Result<&mut StoreRead> {
    if !table.iter_mut().any(|entry| entry.is::<StoreRead>()) {
        table.push(StoreRead::default())?;
    }
    Ok(table
        .iter_mut()
        .find_map(|entry| entry.downcast_mut::<StoreRead>())
        .expect("Store read state exists"))
}

/// Check the call's caller and operation, and open the read of an authorize.
fn enter<T: 'static + Send>(
    accessor: &Accessor<T, EdgeOperationHost>,
    caller: Option<Resource<AuthenticatedCaller>>,
    operation: &str,
    begin: bool,
) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
    accessor.with(|mut access| {
        let ctx = access.get();
        if store_read(ctx.table)?.open {
            return Ok(Err(Refusal::AlreadyBegun));
        }
        let Ok(caller) = caller
            .map(|caller| ctx.table.get(&caller).cloned())
            .transpose()
        else {
            return Ok(Err(Refusal::InvalidCaller));
        };
        let plugin = ctx.try_get_plugin::<EdgeOperation>(EDGE_OPERATION_ID)?;
        if let Err(refusal) = plugin.admit(caller.as_ref(), operation, begin) {
            return Ok(Err(refusal));
        }
        if !begin {
            store_read(ctx.table)?.open = true;
        }
        Ok(Ok(()))
    })
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the binding declares each operation call `async` because the cloud host does \
              I/O; the edge answers from the loaded release and the Store"
)]
impl<T: 'static + Send> operation::HostWithStore<T> for EdgeOperationHost {
    async fn authorize(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        enter(accessor, caller, &operation, false)
    }

    async fn finish_read(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        accessor.with(|mut access| {
            let read = store_read(access.get().table)?;
            if !read.open {
                return Ok(Err(Refusal::NotBegun));
            }
            read.open = false;
            Ok(Ok(()))
        })
    }

    async fn begin_operation(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
        _idempotency_key: String,
        _request_hash: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
        // A begin that passes the checks is of an operation with an
        // idempotency rule. The box serves no write operation yet, so that
        // operation is unknown to it. wamn-2bnru replaces this refusal with
        // the SQLite implementation.
        Ok(match enter(accessor, caller, &operation, true)? {
            Ok(()) => Err(Refusal::UnknownOperation),
            Err(refusal) => Err(refusal),
        })
    }

    async fn commit(
        _accessor: &Accessor<T, Self>,
        _result: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        Ok(Err(Refusal::NotBegun))
    }

    async fn rollback(
        _accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        Ok(Err(Refusal::NotBegun))
    }

    async fn wal_position(
        _accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<String, Refusal>> {
        Ok(Err(Refusal::NotBegun))
    }
}
