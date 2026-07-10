//! S1 stub of the `wamn:postgres` host plugin.
//!
//! Serves canned results so components importing `wamn:postgres/client` can
//! link and run before the real plugin (S2) exists. No connection pooling, no
//! claims, no Postgres: `query` returns a marker row, `execute` returns 0,
//! transactions/cursors are inert resources with drop-rolls-back-nothing
//! semantics. Contract source of truth: docs/wamn-postgres.wit.

use std::collections::HashSet;

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Resource;
use wash_runtime::wit::{WitInterface, WitWorld};

/// Host-side representation of an open (stub) transaction.
pub struct StubTransaction;
/// Host-side representation of an open (stub) cursor.
pub struct StubCursor;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "postgres-plugin",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:postgres/client.transaction": super::StubTransaction,
            "wamn:postgres/client.cursor": super::StubCursor,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{Column, PgError, RowSet, SqlValue};

pub const WAMN_POSTGRES_ID: &str = "wamn-postgres";

fn stub_rowset() -> RowSet {
    RowSet {
        columns: vec![Column {
            name: "stub".to_string(),
            type_name: "text".to_string(),
        }],
        rows: vec![vec![SqlValue::Text(
            "wamn:postgres S1 stub — real plugin lands with S2".to_string(),
        )]],
    }
}

#[derive(Default)]
pub struct WamnPostgresStub;

#[async_trait::async_trait]
impl HostPlugin for WamnPostgresStub {
    fn id(&self) -> &'static str {
        WAMN_POSTGRES_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:postgres/types@0.1.0"),
                WitInterface::from("wamn:postgres/client@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "postgres", &["client"]) {
            return Ok(());
        }
        client::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        tracing::debug!(component = item.id(), "bound wamn:postgres stub");
        Ok(())
    }
}

impl client::Host for ActiveCtx<'_> {
    async fn query(
        &mut self,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        Ok(Ok(stub_rowset()))
    }

    async fn execute(
        &mut self,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        Ok(Ok(0))
    }

    async fn begin(
        &mut self,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<StubTransaction>, PgError>> {
        Ok(Ok(self.table.push(StubTransaction)?))
    }
}

impl client::HostTransaction for ActiveCtx<'_> {
    async fn query(
        &mut self,
        _rep: Resource<StubTransaction>,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        Ok(Ok(stub_rowset()))
    }

    async fn execute(
        &mut self,
        _rep: Resource<StubTransaction>,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        Ok(Ok(0))
    }

    async fn open_cursor(
        &mut self,
        _rep: Resource<StubTransaction>,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<StubCursor>, PgError>> {
        Ok(Ok(self.table.push(StubCursor)?))
    }

    async fn commit(
        &mut self,
        _rep: Resource<StubTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        Ok(Ok(()))
    }

    async fn rollback(
        &mut self,
        _rep: Resource<StubTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        Ok(Ok(()))
    }

    async fn drop(&mut self, rep: Resource<StubTransaction>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl client::HostCursor for ActiveCtx<'_> {
    async fn fetch(
        &mut self,
        _rep: Resource<StubCursor>,
        _max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        // Empty batch = exhausted, per the contract.
        Ok(Ok(RowSet {
            columns: vec![],
            rows: vec![],
        }))
    }

    async fn drop(&mut self, rep: Resource<StubCursor>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}
