//! Dispatch-time capability policy enforcement (docs/platform-plan.md 5.3).
//!
//! Two layers, both here in the pure crate so both are unit-testable:
//!
//! 1. **The dispatch check** ([`check_grants`]): a node type's declared
//!    capability row must be covered by what the runner granted this dispatch,
//!    or the dispatch is refused with `Terminal("capability-denied")` before
//!    the node runs at all. This is where the D8 raw-SQL flag bites: `RawSql`
//!    is granted only when the project's flag is ON (default OFF).
//! 2. **The gated context** ([`GatedCtx`]): even a buggy node implementation
//!    cannot reach a capability outside its declared row — undeclared calls
//!    fail with `NotGranted` at the facade.

use wamn_node_sdk::{
    Capability, CredentialCapError, ErrorDetail, HttpCapError, HttpRequest, HttpResponse, NodeCtx,
    NodeError, PgCapError, PgRows, PgValue,
};

/// The default grant set a runner extends to standard-library dispatches.
pub const GRANTS_DEFAULT: &[Capability] = &[Capability::HttpEgress, Capability::Postgres];
/// The grant set when the project's D8 raw-SQL flag is ON.
pub const GRANTS_WITH_RAW_SQL: &[Capability] = &[
    Capability::HttpEgress,
    Capability::Postgres,
    Capability::RawSql,
];

/// The grant set for a project — `raw_sql_enabled` is the D8 per-project flag
/// (default OFF; enablement for real projects is gated on the dedicated
/// user-SQL role, wamn-1nd).
pub fn granted_for(raw_sql_enabled: bool) -> &'static [Capability] {
    if raw_sql_enabled {
        GRANTS_WITH_RAW_SQL
    } else {
        GRANTS_DEFAULT
    }
}

/// Refuse the dispatch unless every declared capability is granted.
pub(crate) fn check_grants(
    node_type: &str,
    declared: &[Capability],
    granted: &[Capability],
) -> Result<(), NodeError> {
    for cap in declared {
        if !granted.contains(cap) {
            let message = match cap {
                Capability::RawSql => format!(
                    "node type {node_type:?} runs author-written SQL, which is \
                     disabled for this project (D8 raw-SQL flag, default off)"
                ),
                other => format!(
                    "node type {node_type:?} requires capability {other:?}, \
                     which this runner does not grant"
                ),
            };
            return Err(NodeError::Terminal(ErrorDetail::coded(
                "capability-denied",
                message,
            )));
        }
    }
    Ok(())
}

/// A capability facade narrowed to a node's declared row. Calls outside the
/// row fail with `NotGranted` — the node never observes the wider world.
pub(crate) struct GatedCtx<'a> {
    pub inner: &'a mut dyn NodeCtx,
    pub allowed: &'static [Capability],
}

impl GatedCtx<'_> {
    fn allows(&self, cap: Capability) -> bool {
        self.allowed.contains(&cap)
    }
}

impl NodeCtx for GatedCtx<'_> {
    fn http(&mut self, req: &HttpRequest) -> Result<HttpResponse, HttpCapError> {
        if !self.allows(Capability::HttpEgress) {
            return Err(HttpCapError::NotGranted);
        }
        self.inner.http(req)
    }

    fn pg_query(&mut self, sql: &str, params: &[PgValue]) -> Result<PgRows, PgCapError> {
        if !self.allows(Capability::Postgres) {
            return Err(PgCapError::NotGranted);
        }
        self.inner.pg_query(sql, params)
    }

    fn pg_execute(&mut self, sql: &str, params: &[PgValue]) -> Result<u64, PgCapError> {
        if !self.allows(Capability::Postgres) {
            return Err(PgCapError::NotGranted);
        }
        self.inner.pg_execute(sql, params)
    }

    fn catalog_json(&mut self) -> Result<String, PgCapError> {
        if !self.allows(Capability::Postgres) {
            return Err(PgCapError::NotGranted);
        }
        self.inner.catalog_json()
    }

    fn raw_sql_enabled(&self) -> bool {
        self.inner.raw_sql_enabled()
    }

    fn credential(&mut self) -> Result<String, CredentialCapError> {
        // Not a Capability row: the grant is the flow-level declaration
        // (`node.credential`, validated at 5.1) enforced by the runner's
        // per-dispatch scoping — a node that declared none gets `NotGranted`
        // from the facade itself. Pass through, like `raw_sql_enabled`.
        self.inner.credential()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A permissive inner ctx that records whether each capability was reached.
    #[derive(Default)]
    struct Open {
        http_hits: u32,
        pg_hits: u32,
        raw_flag: bool,
    }

    impl NodeCtx for Open {
        fn http(&mut self, _req: &HttpRequest) -> Result<HttpResponse, HttpCapError> {
            self.http_hits += 1;
            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: b"{}".to_vec(),
            })
        }
        fn pg_query(&mut self, _sql: &str, _params: &[PgValue]) -> Result<PgRows, PgCapError> {
            self.pg_hits += 1;
            Ok(PgRows::default())
        }
        fn pg_execute(&mut self, _sql: &str, _params: &[PgValue]) -> Result<u64, PgCapError> {
            self.pg_hits += 1;
            Ok(0)
        }
        fn catalog_json(&mut self) -> Result<String, PgCapError> {
            self.pg_hits += 1;
            Ok("{}".into())
        }
        fn raw_sql_enabled(&self) -> bool {
            self.raw_flag
        }
        fn credential(&mut self) -> Result<String, CredentialCapError> {
            Ok("open-secret".into())
        }
    }

    /// The gated ctx refuses every capability outside the declared row — even
    /// a buggy node implementation cannot reach the wider world.
    #[test]
    fn gated_ctx_refuses_undeclared_capabilities() {
        let mut inner = Open::default();
        let mut gated = GatedCtx {
            inner: &mut inner,
            allowed: &[],
        };
        assert_eq!(
            gated.http(&HttpRequest::default()),
            Err(HttpCapError::NotGranted)
        );
        assert_eq!(gated.pg_query("SELECT 1", &[]), Err(PgCapError::NotGranted));
        assert_eq!(
            gated.pg_execute("SELECT 1", &[]),
            Err(PgCapError::NotGranted)
        );
        assert_eq!(gated.catalog_json(), Err(PgCapError::NotGranted));
        assert_eq!(
            inner.http_hits + inner.pg_hits,
            0,
            "inner ctx never reached"
        );
    }

    /// Declared capabilities pass through to the inner ctx.
    #[test]
    fn gated_ctx_passes_declared_capabilities_through() {
        let mut inner = Open {
            raw_flag: true,
            ..Open::default()
        };
        let mut gated = GatedCtx {
            inner: &mut inner,
            allowed: GRANTS_DEFAULT,
        };
        assert!(gated.http(&HttpRequest::default()).is_ok());
        assert!(gated.pg_query("SELECT 1", &[]).is_ok());
        assert!(gated.catalog_json().is_ok());
        assert!(gated.raw_sql_enabled(), "flag is a passthrough, not a gate");
        assert_eq!(inner.http_hits, 1);
        assert_eq!(inner.pg_hits, 2);
    }

    /// `credential()` passes through the gate unconditionally — the grant is
    /// the flow-level declaration enforced by the runner's per-dispatch
    /// scoping, not a Capability row. A gated ctx must not mask the vault
    /// behind the fail-closed trait default.
    #[test]
    fn gated_ctx_passes_credential_through_to_the_runner_facade() {
        let mut inner = Open::default();
        let mut gated = GatedCtx {
            inner: &mut inner,
            allowed: &[],
        };
        assert_eq!(gated.credential(), Ok("open-secret".into()));
    }

    /// The grant check names the D8 flag when the missing capability is RawSql.
    #[test]
    fn check_grants_refuses_with_capability_denied() {
        let err = check_grants(
            "postgres-query",
            &[Capability::Postgres, Capability::RawSql],
            GRANTS_DEFAULT,
        )
        .unwrap_err();
        let NodeError::Terminal(detail) = &err else {
            panic!("expected Terminal, got {err:?}");
        };
        assert_eq!(detail.code.as_deref(), Some("capability-denied"));
        assert!(
            detail.message.contains("D8"),
            "names the flag: {}",
            detail.message
        );
        assert!(check_grants("postgres-query", &[Capability::RawSql], GRANTS_WITH_RAW_SQL).is_ok());
    }

    /// The grant helper: the flag is the ONLY difference, and OFF is default.
    #[test]
    fn granted_for_flags_raw_sql_only() {
        assert!(!granted_for(false).contains(&Capability::RawSql));
        assert!(granted_for(true).contains(&Capability::RawSql));
        assert!(granted_for(false).contains(&Capability::Postgres));
        assert!(granted_for(false).contains(&Capability::HttpEgress));
    }
}
