//! SQL READ builders for the stored test-suite tables (11.2 execution, wamn-0lfu).
//!
//! This module adds the `$n` SELECT builders the scenario worker reads with.
//! Pure string builders — the
//! effect shell holds the connection — so the table-owning crate owns its SQL
//! (mirrors `wamn_run_state::sql` / `wamn_schema_control::sql`).
//!
//! **Read posture.** Tables are UNQUALIFIED so a `search_path` selects the
//! source schema (the house S6 schema-as-fixture pattern); every builder ALSO
//! carries the explicit `(tenant_id, flow_id, flow_version[, suite_id])`
//! predicate, so a read is correct under BOTH a deliberately scoped superuser
//! session and an app-role RLS session (where the predicate agrees with the
//! `app.tenant` claim). The product scenario worker uses the latter; integration
//! proof setup may use the former.
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
/// worker re-parses each body through its private transitional stored-test
/// decoder on read.
pub fn select_cases_for_suite_sql() -> String {
    "SELECT case_id, ordinal, case_body::text FROM test_cases \
     WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = $3 AND suite_id = $4 \
     ORDER BY ordinal"
        .to_string()
}

/// Upsert one version-bound scenario suite. Parameters are tenant, flow id,
/// flow version, suite id, and display name.
pub fn upsert_suite_sql() -> String {
    "INSERT INTO test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (tenant_id, flow_id, flow_version, suite_id) \
       DO UPDATE SET name = excluded.name, updated_at = now()"
        .to_string()
}

/// Upsert one scenario case. Parameters are tenant, flow id, flow version,
/// suite id, case id, ordinal, and the validated case JSON text.
pub fn upsert_case_sql() -> String {
    "INSERT INTO test_cases \
       (tenant_id, flow_id, flow_version, suite_id, case_id, ordinal, case_body) \
     VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb) \
     ON CONFLICT (tenant_id, flow_id, flow_version, suite_id, case_id) \
       DO UPDATE SET ordinal = excluded.ordinal, case_body = excluded.case_body"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every column/table the builders name exists in the canonical DDL — the
    /// deploy file and the builders cannot drift apart silently (mirrors
    /// `wamn_run_state::sql`'s `builder_columns_exist_in_the_canonical_ddl`).
    #[test]
    fn builder_columns_exist_in_the_canonical_ddl() {
        let ddl = include_str!("../../../../deploy/sql/flow-tests.sql");
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
        let suite_write = upsert_suite_sql();
        assert!(suite_write.contains("INSERT INTO test_suites"));
        assert!(suite_write.contains("flow_version"));
        let case_write = upsert_case_sql();
        assert!(case_write.contains("INSERT INTO test_cases"));
        assert!(case_write.contains("$7::text::jsonb"));
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
