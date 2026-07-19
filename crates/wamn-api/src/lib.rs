//! # wamn-api — generated REST API gateway logic (4.1)
//!
//! Turns a project's [`Catalog`](wamn_catalog::Catalog) (3.1) into a REST
//! surface over the tables the DDL compiler (3.2) generates: it compiles an
//! HTTP request into **injection-safe parameterized SQL** and shapes the
//! returned row-set into JSON. It is pure Rust — no host, no database, no Wasm
//! binding — so the whole routing/SQL/shaping surface is unit-testable, and the
//! same code links into the `wasi:http` serving component (`api-gateway`) that
//! binds it to the `wamn:postgres` plugin.
//!
//! ## Scope (REST v1)
//!
//! CRUD + one-level relation expansion, with `filter` / `sort` / `paginate` /
//! `expand` query support. **Not** in v1: GraphQL, aggregations/arbitrary joins,
//! authentication (4.2), field-level masks (4.3), hot-reload (4.4), OpenAPI
//! (4.5), or rate/cost limits (4.6 — v1 only enforces a max page size).
//!
//! ## Safety invariants (the S2 injection lesson, enforced by construction)
//!
//! - **Values are always `$n` parameters.** Every user-supplied value (a filter
//!   value, an `id`, a body field) is bound via [`SqlValue`] and never
//!   string-interpolated. The compiler returns `(sql_template, params)`.
//! - **Identifiers are always catalog-allowlisted.** Every table/column/relation
//!   name comes from the [`Catalog`](wamn_catalog::Catalog) and is quoted with
//!   [`wamn_ddl::sql::quote_ident`]; a request string that does not resolve to a
//!   catalog field/relation is rejected ([`ApiError`]) — it never becomes an
//!   identifier.
//! - **Tenant isolation is the database's job.** Every query runs under the
//!   host-injected `app.tenant` claim + the 3.2 tenant floor (RLS). Writes set
//!   `tenant_id = current_setting('app.tenant', true)` server-side, so no tenant
//!   value is ever taken from the request.
//! - **`tenant_id` is never projected;** `numeric` stays an exact-decimal string
//!   end to end (no float).
//!
//! ## Shape
//!
//! ```no_run
//! use wamn_api::{Method, Router};
//! # fn demo(catalog: &wamn_catalog::Catalog) -> Result<(), wamn_api::ApiError> {
//! let router = Router::new(catalog);
//! let plan = router.compile(
//!     Method::Get,
//!     "/api/rest/receipts",
//!     &[("supplier_id".into(), "eq.….".into()), ("limit".into(), "20".into())],
//!     None,
//! )?;
//! // plan.query.sql + plan.query.params -> wamn:postgres client::query
//! // plan.expands            -> one extra SELECT each (via Router::build_expand)
//! # let _ = plan; Ok(())
//! # }
//! ```

pub mod error;
pub mod registration;
pub mod router;
pub mod shape;
pub mod value;

pub use error::ApiError;
pub use router::{Compiled, Expand, ExpandDir, Method, Plan, PlanKind, Router};
pub use shape::{attach_expansion, shape_row, shape_rows};
pub use value::SqlValue;

// Re-exported so a consumer (the serving component) names the catalog through
// this one crate.
pub use wamn_catalog::Catalog;
