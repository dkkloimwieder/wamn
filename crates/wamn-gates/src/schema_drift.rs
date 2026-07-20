//! wamn-9mg8 [GATE-DRIFT]: ONE uniform drift guard for every gate's ephemeral
//! `run_queue` stand-in DDL against the schema of record
//! (`deploy/sql/run-queue.sql`).
//!
//! History this closes: dispatchbench's stand-in silently dropped `stream_seq`
//! and every live mode broke against a throwaway PG (c32ffaf); wamn-9cn6 found
//! the same drift in four more gates; wamn-nhjg pinned runnerbench's stand-in
//! with an `include_str!` guard and wamn-v8cv extended it for `run_dead_letters`.
//! Each gate carries its OWN, schema-qualified, joined-to-the-flow-tables
//! stand-in (so it can never touch a shared schema), so none can be
//! `include_str!`'d verbatim. This generalizes the single-gate guard into one
//! mechanism every gate calls with a PER-GATE, explicitly-DATA spec of which
//! schema-of-record tables its stand-in needs — so a NEW table added to
//! `run-queue.sql` forces an explicit per-gate Required/AbsentByDesign decision
//! instead of silent rot.
//!
//! Test-only: this module compiles only under `cfg(test)`, so nothing here rides
//! the shipped `wamn-gates` binary.

use wamn_run_queue::PartitionPolicy;

/// The schema of record, compiled in — the guard reads the SHIPPED column set out
/// of it so a stand-in cannot silently drift from what we assert against.
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");

/// What a gate's stand-in does with one schema-of-record table.
pub(crate) enum Need {
    /// The table is present with FULL column parity (every shipped column).
    Required,
    /// The table is present, but these columns are exempt — the gate's
    /// claim/enqueue path provably never reads them. An EXPLICIT, documented
    /// exemption, not silent drift.
    RequiredExcept(&'static [&'static str]),
    /// The table is absent BY DESIGN (the gate has no code path that touches it).
    /// The stand-in must NOT create it, so the exemption stays load-bearing: if a
    /// later edit adds the table, this fails and forces a re-decision.
    AbsentByDesign,
}

/// Every `CREATE TABLE wamn_run.<name>` in the schema of record, in file order.
/// Drives the "spec must classify every shipped table" check, so a new table in
/// `run-queue.sql` forces an explicit per-gate decision.
fn schema_of_record_tables() -> Vec<String> {
    RUN_QUEUE_SQL
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

/// The top-level column names of `CREATE TABLE wamn_run.{table} ( ... )` in the
/// schema of record (one column per line in `deploy/sql`), skipping comments and
/// the PRIMARY/FOREIGN/CONSTRAINT/CHECK/UNIQUE constraint clauses. Lifts the
/// shipped column set straight out of the source of truth so the parity assertion
/// tracks it automatically.
fn record_columns(table: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut in_table = false;
    for line in RUN_QUEUE_SQL.lines() {
        let t = line.trim();
        if !in_table {
            if t.starts_with(&format!("CREATE TABLE wamn_run.{table} (")) {
                in_table = true;
            }
            continue;
        }
        if t.starts_with(')') {
            break; // end of the CREATE TABLE body
        }
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        let Some(tok) = t.split_whitespace().next() else {
            continue;
        };
        if matches!(
            tok,
            "PRIMARY" | "FOREIGN" | "CONSTRAINT" | "CHECK" | "UNIQUE"
        ) {
            continue;
        }
        cols.push(tok.to_string());
    }
    assert!(
        !cols.is_empty(),
        "parser sanity: no columns lifted for wamn_run.{table} — schema-of-record \
         layout changed (record_columns expects one column per line)"
    );
    cols
}

/// The `CREATE TABLE wamn_run.{table} ( ... )` body of a stand-in DDL string, from
/// the opening `(` to the first `);` (the table terminator — no column line ends
/// in `);`). Scoping column checks to this body means a column named only in a
/// trailing `CREATE INDEX` does NOT mask a dropped column definition.
fn stand_in_table_body<'a>(standin: &'a str, table: &str) -> Option<&'a str> {
    let head = format!("CREATE TABLE wamn_run.{table} (");
    let start = standin.find(&head)? + head.len();
    let rest = &standin[start..];
    let end = rest.find(");")?;
    Some(&rest[..end])
}

/// The uniform guard. Assert `standin` (a gate's ephemeral DDL, built with schema
/// `wamn_run`) tracks the schema of record per `spec`.
///
/// `spec` MUST classify every schema-of-record table exactly once: a table in
/// `run-queue.sql` with no entry fails every gate until each makes an explicit
/// Required/AbsentByDesign decision, and a stale entry (a table no longer of
/// record) fails too.
///
/// - `Required` / `RequiredExcept`: the stand-in must CREATE the table and carry
///   every shipped column (minus the exempt set), checked within the table body
///   so a same-named index column can't mask a dropped definition. When
///   `run_queue` carries `partition_policy`, both `PartitionPolicy` literals must
///   appear (the CHECK must accept what the enqueue writers materialize); when
///   `partition_owner` is Required, the `run_queue_partition` index the claim path
///   scans must be present.
/// - `AbsentByDesign`: the stand-in must NOT create the table.
pub(crate) fn assert_stand_in(gate: &str, standin: &str, spec: &[(&str, Need)]) {
    // The spec classifies exactly the schema-of-record tables — no gaps, no rot.
    let record = schema_of_record_tables();
    for table in &record {
        assert!(
            spec.iter().any(|(t, _)| t == table),
            "{gate}: stand-in drift spec does not classify schema-of-record table \
             `wamn_run.{table}` (add it to deploy/sql/run-queue.sql's guard spec as \
             Required or AbsentByDesign — a new table needs an explicit per-gate \
             decision, not silent rot)"
        );
    }
    for (table, _) in spec {
        assert!(
            record.iter().any(|t| t == table),
            "{gate}: stand-in drift spec classifies `{table}`, which is no longer a \
             table of record in deploy/sql/run-queue.sql (stale spec entry)"
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
            Need::Required | Need::RequiredExcept(_) => {
                let body = stand_in_table_body(standin, table).unwrap_or_else(|| {
                    panic!(
                        "{gate}: stand-in is missing the `wamn_run.{table}` table the \
                         drift spec marks Required (drifted from deploy/sql/run-queue.sql)"
                    )
                });
                let exempt: &[&str] = match need {
                    Need::RequiredExcept(cols) => cols,
                    _ => &[],
                };
                for col in record_columns(table) {
                    if exempt.contains(&col.as_str()) {
                        continue;
                    }
                    assert!(
                        body.contains(&col),
                        "{gate}: `wamn_run.{table}` stand-in missing column `{col}` \
                         (drifted from deploy/sql/run-queue.sql)"
                    );
                }
                // run_queue's partition_policy is a CHECK'd enum: if the stand-in
                // carries the column, its CHECK must accept every literal the
                // enqueue writers materialize (D20). Skip when the column is exempt.
                if *table == "run_queue" && !exempt.contains(&"partition_policy") {
                    for p in PartitionPolicy::ALL {
                        assert!(
                            standin.contains(&format!("'{}'", p.as_sql())),
                            "{gate}: run_queue stand-in partition_policy CHECK missing \
                             literal `{}`",
                            p.as_sql()
                        );
                    }
                }
                // The per-partition claim path (acquire/claim head) scans the
                // run_queue_partition index and leases against partition_owner, so a
                // gate that Requires partition_owner needs that index too.
                if *table == "partition_owner" {
                    assert!(
                        standin.contains("run_queue_partition"),
                        "{gate}: stand-in Requires partition_owner but is missing the \
                         run_queue_partition index the claim path scans"
                    );
                }
            }
        }
    }
}
