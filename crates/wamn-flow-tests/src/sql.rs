//! SQL READ builders for the stored test-suite tables (11.2 execution, wamn-0lfu).
//!
//! The suite ENVELOPE ([`crate::TestSuite`]) is pure serde; this module adds the
//! `$n` SELECT builders the stored-suite EXECUTOR (`wamn-gates testkitbench
//! --suite/--impact-report`) reads with. Pure `format!`/string builders — the
//! effect shell holds the connection — so the table-owning crate owns its SQL
//! (mirrors `wamn_run_store::sql` / `wamn_migrate::sql`), and `wamn-gates`
//! already depends on `wamn-flow-tests` so no new gate dependency is incurred.
//!
//! **Read posture.** Tables are UNQUALIFIED so a `search_path` selects the
//! source schema (the house S6 schema-as-fixture pattern); every builder ALSO
//! carries the explicit `(tenant_id, flow_id, flow_version[, suite_id])`
//! predicate, so a read is correct under BOTH a superuser (RLS-bypassing, the
//! executor's cross-tenant admin session) AND an app-role RLS session (the
//! predicate agrees with the `app.tenant` claim). The executor reads via the
//! ADMIN session it already needs for provisioning; the explicit tenant
//! predicate — not the RLS floor — is what scopes the read (documented in the
//! executor).
//!
//! Kept aligned with `deploy/sql/flow-tests.sql` by the drift guard below.

/// Every suite of a `(tenant, flow_id, flow_version)`, in `suite_id` order — the
/// enumeration behind the `--suite <flow_id>@<version>` selector (which runs
/// ALL of a flow version's suites).
pub fn select_suites_for_flow_sql() -> String {
    "SELECT suite_id, name FROM test_suites \
     WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = $3 \
     ORDER BY suite_id"
        .to_string()
}

/// One suite's cases, in `ordinal` order. `case_body` is emitted as text so the
/// executor re-parses each body against the `wamn-testkit` vocabulary on READ
/// (the same validate pass `TestSuite::validate` runs on WRITE).
pub fn select_cases_for_suite_sql() -> String {
    "SELECT case_id, ordinal, case_body::text FROM test_cases \
     WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = $3 AND suite_id = $4 \
     ORDER BY ordinal"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every column/table the builders name exists in the canonical DDL — the
    /// deploy file and the builders cannot drift apart silently (mirrors
    /// `wamn_run_store::sql`'s `builder_columns_exist_in_the_canonical_ddl`).
    #[test]
    fn builder_columns_exist_in_the_canonical_ddl() {
        let ddl = include_str!("../../../deploy/sql/flow-tests.sql");
        assert!(ddl.contains("test_suites"), "test_suites missing from DDL");
        assert!(ddl.contains("test_cases"), "test_cases missing from DDL");
        for col in [
            "tenant_id",
            "flow_id",
            "flow_version",
            "suite_id",
            "case_id",
            "ordinal",
            "case_body",
            "name",
        ] {
            assert!(
                ddl.contains(col),
                "column {col} missing from flow-tests.sql"
            );
        }
    }

    /// Pinned-string guard: the builders SELECT the exact tables/columns/order
    /// the executor depends on. A rename here without a matching read change is
    /// caught before it reaches Postgres.
    #[test]
    fn builders_pin_their_shape() {
        let cases = select_cases_for_suite_sql();
        assert!(cases.contains("FROM test_cases"));
        assert!(cases.contains("case_body::text"));
        assert!(cases.contains("ORDER BY ordinal"));
        let suites = select_suites_for_flow_sql();
        assert!(suites.contains("FROM test_suites"));
        assert!(suites.contains("ORDER BY suite_id"));
    }

    /// Mutation guard (suite-selection WHERE predicate): the cases read is
    /// scoped by ALL FOUR keys, INCLUDING `suite_id = $4`. A mutant that drops
    /// `suite_id` from the predicate would read a SIBLING suite's cases; this
    /// fails it before the end-to-end gate does.
    #[test]
    fn cases_predicate_is_scoped_by_all_four_keys() {
        let sql = select_cases_for_suite_sql();
        for key in [
            "tenant_id = $1",
            "flow_id = $2",
            "flow_version = $3",
            "suite_id = $4",
        ] {
            assert!(sql.contains(key), "cases predicate missing {key}");
        }
    }

    /// The suite enumeration is scoped by the `(tenant, flow_id, flow_version)`
    /// tuple — a `--suite flow@version` selector must not enumerate a foreign
    /// tenant's or a foreign version's suites.
    #[test]
    fn suites_predicate_is_scoped_by_the_flow_version_tuple() {
        let sql = select_suites_for_flow_sql();
        for key in ["tenant_id = $1", "flow_id = $2", "flow_version = $3"] {
            assert!(sql.contains(key), "suites predicate missing {key}");
        }
    }
}
