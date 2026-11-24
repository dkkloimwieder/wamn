//! A composed component links its per-member statements imports.
//!
//! The composer gives each member its own `sql-<position>` import, labelled
//! `(implements "wamn:postgres/statements@0.4.0")`. The release declares one
//! unnamed `wamn:postgres` host interface entry, so the bind sees no name.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use wamn_engine::engine::build_engine;
use wamn_plugin::OperationHost;
use wamn_postgres::WamnPostgres;
use wash_runtime::engine::WasmProposal;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::plugin::WitInterfaces;
use wash_runtime::wasmtime::component::{Component, Linker};
use wash_runtime::wit::WitInterface;

/// The imports the composer emits for a base and an overlay:
///
/// ```wat
/// (component
///   (import "sql-0" (implements "wamn:postgres/statements@0.4.0")
///     (instance (export "transaction" (type (sub resource)))))
///   (import "sql-1" (implements "wamn:postgres/statements@0.4.0")
///     (instance (export "transaction" (type (sub resource))))))
/// ```
const COMPOSED_IMPORTS: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00, 0x07, 0x13, 0x01, 0x42, 0x01, 0x04, 0x00, 0x0b,
    0x74, 0x72, 0x61, 0x6e, 0x73, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x03, 0x01, 0x0a, 0x2b, 0x01,
    0x02, 0x05, 0x73, 0x71, 0x6c, 0x2d, 0x30, 0x01, 0x00, 0x1e, 0x77, 0x61, 0x6d, 0x6e, 0x3a, 0x70,
    0x6f, 0x73, 0x74, 0x67, 0x72, 0x65, 0x73, 0x2f, 0x73, 0x74, 0x61, 0x74, 0x65, 0x6d, 0x65, 0x6e,
    0x74, 0x73, 0x40, 0x30, 0x2e, 0x34, 0x2e, 0x30, 0x05, 0x00, 0x07, 0x13, 0x01, 0x42, 0x01, 0x04,
    0x00, 0x0b, 0x74, 0x72, 0x61, 0x6e, 0x73, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x03, 0x01, 0x0a,
    0x2b, 0x01, 0x02, 0x05, 0x73, 0x71, 0x6c, 0x2d, 0x31, 0x01, 0x00, 0x1e, 0x77, 0x61, 0x6d, 0x6e,
    0x3a, 0x70, 0x6f, 0x73, 0x74, 0x67, 0x72, 0x65, 0x73, 0x2f, 0x73, 0x74, 0x61, 0x74, 0x65, 0x6d,
    0x65, 0x6e, 0x74, 0x73, 0x40, 0x30, 0x2e, 0x34, 0x2e, 0x30, 0x05, 0x01,
];

#[test]
fn an_unnamed_statements_entry_links_every_member_import() {
    let engine = build_engine(&[WasmProposal::WasmComponentModelImplements])
        .expect("the production component engine");
    let component =
        Component::new(engine.inner(), COMPOSED_IMPORTS).expect("compile the composed imports");
    let pools = Arc::new(WamnPostgres::from_host_config(BTreeMap::new()).expect("empty pools"));
    let authorization = wamn_authentication::authorization::Authorization::new(Arc::clone(&pools));
    let host = OperationHost::new(pools, Arc::new(authorization), None);
    // The environment application chart writes this interface without a name.
    let entry = HashSet::from([WitInterface::from("wamn:postgres/statements,types@0.4.0")]);
    let mut linker = Linker::<SharedCtx>::new(engine.inner());

    host.add_linker_entries(&mut linker, &component, &WitInterfaces::new(&entry))
        .expect("add the wamn:postgres linker entries");

    linker
        .instantiate_pre(&component)
        .expect("every sql-<position> import links");
}
