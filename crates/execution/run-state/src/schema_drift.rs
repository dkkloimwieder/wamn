//! Contract-owned drift guard for every proof's ephemeral run-plane stand-in DDL
//! against the schemas of record (`deploy/sql/run-queue.sql` and
//! `deploy/sql/run-state.sql`).
//!
//! History this closes: dispatchbench's stand-in silently dropped `stream_seq`
//! and every live mode broke against a throwaway PG (c32ffaf); wamn-9cn6 found
//! the same drift in four more gates; wamn-nhjg pinned runnerbench's stand-in
//! with an `include_str!` guard.
//! Each gate carries its OWN, schema-qualified, joined-to-the-flow-tables
//! stand-in (so it can never touch a shared schema), so none can be
//! `include_str!`'d verbatim. This generalizes the single-gate guard into one
//! mechanism every gate calls with a PER-GATE, explicitly-DATA spec of which
//! schema-of-record tables its stand-in needs — so a NEW table added to either
//! file forces an explicit per-gate Required/AbsentByDesign decision instead of
//! silent rot.
//!
//! wamn-y8wd: the guard covered `run-queue.sql` only, so the same drift class in
//! the `runs`/`node_runs` half of the run plane (which lives in `run-state.sql`)
//! evaded it structurally — wamn-thvs is exactly that miss, hand-patched per
//! gate. [`assert_run_state_stand_in`] is the run-state entry point; both share
//! one parenthesis-aware parse of the schema of record and of the stand-in, so
//! full column parity is compared by NAME (a stand-in that keeps
//! `parent_run_id` no longer masks a dropped `run_id`).
//!
//! This module is available only through the `test-util` feature. It lives with
//! the run-state contract so conformance, integration, and system proofs consume
//! one implementation without depending on one another.

use crate::status::RunStatus;

/// The schemas of record, compiled in — the guard reads the SHIPPED column set
/// out of them so a stand-in cannot silently drift from what we assert against.
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");

/// Which shipped file a stand-in is checked against. Both create their tables in
/// `wamn_run`; a gate that materializes tables from both calls the guard once per
/// schema, so each file's table set stays fully classified on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaOfRecord {
    RunQueue,
    RunState,
}

impl SchemaOfRecord {
    fn sql(self) -> &'static str {
        match self {
            SchemaOfRecord::RunQueue => RUN_QUEUE_SQL,
            SchemaOfRecord::RunState => RUN_STATE_SQL,
        }
    }

    /// The path named in every failure message, so a gate knows which file to
    /// reconcile against.
    fn path(self) -> &'static str {
        match self {
            SchemaOfRecord::RunQueue => "deploy/sql/run-queue.sql",
            SchemaOfRecord::RunState => "deploy/sql/run-state.sql",
        }
    }
}

/// What a gate's stand-in does with one schema-of-record table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The table is present with FULL column parity (every shipped column).
    Required,
    /// The table is absent BY DESIGN (the gate has no code path that touches it).
    /// The stand-in must NOT create it, so the exemption stays load-bearing: if a
    /// later edit adds the table, this fails and forces a re-decision.
    AbsentByDesign,
}

/// Every `CREATE TABLE wamn_run.<name>` in a schema of record, in file order.
/// Drives the "spec must classify every shipped table" check, so a new table in
/// either file forces an explicit per-gate decision.
fn schema_of_record_tables(schema: SchemaOfRecord) -> Vec<String> {
    schema
        .sql()
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("CREATE TABLE wamn_run.")?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The `CREATE TABLE wamn_run.{table} ( ... )` body of `sql` — everything between
/// the opening parenthesis and its match, with `--` comments dropped. Matching
/// parentheses (rather than the first `);`) is what lets a table close correctly
/// around the multi-line CHECK constraints `run-state.sql` uses; single-quoted
/// literals are skipped so a default value cannot unbalance the scan. Scoping
/// column checks to this body means a column named only in a trailing
/// `CREATE INDEX` does NOT mask a dropped column definition.
fn table_body(sql: &str, table: &str) -> Option<String> {
    let head = format!("CREATE TABLE wamn_run.{table} (");
    let start = sql.find(&head)? + head.len();
    let mut body = String::new();
    let mut depth = 1usize;
    let mut quoted = false;
    let mut commented = false;
    let mut characters = sql[start..].chars().peekable();
    while let Some(character) = characters.next() {
        if commented {
            commented = character != '\n';
            continue;
        }
        if quoted {
            quoted = character != '\'';
            body.push(character);
            continue;
        }
        match character {
            '\'' => quoted = true,
            '-' if characters.peek() == Some(&'-') => {
                commented = true;
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body);
                }
            }
            _ => {}
        }
        body.push(character);
    }
    None
}

/// The comma-separated items of a table body at parenthesis depth 0 — one column
/// definition or one table constraint each.
fn body_items(body: &str) -> Vec<String> {
    let mut items = vec![String::new()];
    let mut depth = 0usize;
    let mut quoted = false;
    for character in body.chars() {
        if quoted {
            quoted = character != '\'';
        } else {
            match character {
                '\'' => quoted = true,
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    items.push(String::new());
                    continue;
                }
                _ => {}
            }
        }
        items
            .last_mut()
            .expect("items is never empty")
            .push(character);
    }
    items
}

/// The declared column names of `wamn_run.{table}` in `sql`, in declaration
/// order: every top-level body item whose leading token is not a table-constraint
/// keyword.
fn declared_columns(sql: &str, table: &str) -> Option<Vec<String>> {
    let items = body_items(&table_body(sql, table)?);
    Some(
        items
            .iter()
            .filter_map(|item| item.split_whitespace().next())
            .filter(|token| {
                !matches!(
                    token.to_ascii_uppercase().as_str(),
                    "PRIMARY" | "FOREIGN" | "CONSTRAINT" | "CHECK" | "UNIQUE" | "EXCLUDE" | "LIKE"
                )
            })
            .map(|token| token.to_string())
            .collect(),
    )
}

/// The shipped column set of `wamn_run.{table}`, lifted straight out of the source
/// of truth so the parity assertion tracks it automatically.
fn record_columns(schema: SchemaOfRecord, table: &str) -> Vec<String> {
    let columns = declared_columns(schema.sql(), table).unwrap_or_else(|| {
        panic!(
            "parser sanity: no `CREATE TABLE wamn_run.{table} (` body in {}",
            schema.path()
        )
    });
    assert!(
        !columns.is_empty(),
        "parser sanity: no columns lifted for wamn_run.{table} — {} layout changed",
        schema.path()
    );
    columns
}

/// The uniform guard for `deploy/sql/run-queue.sql`. Assert `standin` (a gate's
/// ephemeral DDL, built with schema `wamn_run`) tracks the schema of record per
/// `spec`.
///
/// `spec` MUST classify every schema-of-record table exactly once: a table in
/// `run-queue.sql` with no entry fails every gate until each makes an explicit
/// Required/AbsentByDesign decision, and a stale entry (a table no longer of
/// record) fails too.
///
/// - `Required`: the stand-in must CREATE the table and declare every shipped
///   column by name, checked within the table body so a same-named index column
///   can't mask a dropped definition.
/// - `AbsentByDesign`: the stand-in must NOT create the table.
pub fn assert_stand_in(gate: &str, standin: &str, spec: &[(&str, Need)]) {
    assert_stand_in_against(SchemaOfRecord::RunQueue, gate, standin, spec);
}

/// The same guard for `deploy/sql/run-state.sql` — the `runs`/`node_runs` half of
/// the run plane, plus the invocation and effect ledgers.
///
/// When `runs` is Required, every `RunStatus` literal must appear, for the same
/// reason all persisted status literals must remain visible to stand-ins.
pub fn assert_run_state_stand_in(gate: &str, standin: &str, spec: &[(&str, Need)]) {
    assert_stand_in_against(SchemaOfRecord::RunState, gate, standin, spec);
}

fn assert_stand_in_against(
    schema: SchemaOfRecord,
    gate: &str,
    standin: &str,
    spec: &[(&str, Need)],
) {
    let path = schema.path();
    // The spec classifies exactly the schema-of-record tables — no gaps, no rot.
    let record = schema_of_record_tables(schema);
    for table in &record {
        assert!(
            spec.iter().any(|(t, _)| t == table),
            "{gate}: stand-in drift spec does not classify schema-of-record table \
             `wamn_run.{table}` (add it to {path}'s guard spec as Required or \
             AbsentByDesign — a new table needs an explicit per-gate decision, not \
             silent rot)"
        );
    }
    for (table, _) in spec {
        assert!(
            record.iter().any(|t| t == table),
            "{gate}: stand-in drift spec classifies `{table}`, which is no longer a \
             table of record in {path} (stale spec entry)"
        );
    }

    for (table, need) in spec {
        match need {
            Need::AbsentByDesign => {
                assert!(
                    !standin.contains(&format!("CREATE TABLE wamn_run.{table}")),
                    "{gate}: stand-in CREATEs `wamn_run.{table}`, but the drift spec \
                     marks it AbsentByDesign — re-decide (make it Required, or drop \
                     the table)"
                );
            }
            Need::Required => {
                let declared = declared_columns(standin, table).unwrap_or_else(|| {
                    panic!(
                        "{gate}: stand-in is missing the `wamn_run.{table}` table the \
                         drift spec marks Required (drifted from {path})"
                    )
                });
                for col in record_columns(schema, table) {
                    assert!(
                        declared.contains(&col),
                        "{gate}: `wamn_run.{table}` stand-in missing column `{col}` \
                         (drifted from {path})"
                    );
                }
                // A CHECK'd status enum must accept every literal its writers
                // materialize.
                if schema == SchemaOfRecord::RunState && *table == "runs" {
                    for status in RunStatus::ALL {
                        assert!(
                            standin.contains(&format!("'{}'", status.as_sql())),
                            "{gate}: runs stand-in status CHECK missing literal `{}`",
                            status.as_sql()
                        );
                    }
                }
            }
        }
    }
}
