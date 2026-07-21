//! The run-plane schema reconciler (E4/R14-migration, wamn-1wdq).
//!
//! `deploy/sql/run-state.sql` / `flows.sql` / `run-queue.sql` evolve, but a
//! schema instantiated from an older revision has NO migration path: the 2jkm.41
//! sweep found live demo schemas missing the E4 `stream_seq` column (runner
//! drains failed 42703), the fqg.20/D20 `partition_policy`, whole queue tables
//! (`poc_f1` predated per-project queue provisioning), and — after the ephemeral
//! fixture pod restarted — everything at once, including the `catalog` metadata
//! schema. This module is the PURE decision (the reconcile-replica-identity
//! precedent — no DB, clock, or wasm): given what the driver OBSERVED live
//! (tables + columns + index definitions + legacy outbox-era objects + the
//! `catalog` schema state), it produces the idempotent, ADDITIVE plan that
//! brings one project-env's run-plane schema to the schema of record. The
//! `wamn-ctl reconcile-run-plane` shell reads/executes; the throwaway-PG gate
//! proves the live transitions.
//!
//! **The schema of record is the deploy/sql source itself**, embedded at compile
//! time (`include_str!`) — the SAME files the wamn-gates `schema_drift` guard
//! (wamn-9mg8) pins — and sliced per table, so the plan can never drift from
//! what provisioning applies. Per-project schemas are the `wamn_run` → target
//! rewrite (`rewrite_schema`, the `publish-catalog --runstate` convention).
//!
//! What the plan covers (the wamn-1wdq manifestation set):
//!
//! 1. **Additive column drift** — a present table missing record columns gains
//!    `ALTER TABLE … ADD COLUMN <record definition>` (e.g. E4 `stream_seq
//!    bigint NOT NULL DEFAULT 0`, D20 `partition_policy … CHECK …`).
//! 2. **Index drift** — a record index absent live is created; a present one
//!    whose live definition lacks a record column the record definition names
//!    (the pre-E4 `run_queue_claimable` without `stream_seq`) is dropped and
//!    recreated from record.
//! 3. **Wholly-missing tables** — created from their record section (DDL +
//!    indexes + RLS + policy + grants), in file order so FKs resolve.
//! 4. **The pre-l5i9.19 outbox era** — legacy `outbox`/`evt_shadow` tables, the
//!    constant-named `wamn_outbox_event` trigger (per entity table) and its
//!    function are DROPPED (trigger before function — the function drop is
//!    RESTRICT), and stored registrations carrying the legacy `state` key are
//!    stripped (a state-carrying document fails parse post-teardown → HELD).
//! 5. **From-zero restore** — an empty database plans the full set, including
//!    `deploy/sql/catalog-schema.sql` (the `catalog` metadata schema the
//!    registration storage and the RI reconcile read).
//! 6. **`fail_kind` CHECK literal drift** (wamn-fqg.16) — a `runs.fail_kind`
//!    CHECK provisioned before cjv.4 added the `'runaway-budget'` literal admits
//!    only the 3 legacy literals, so a runaway run's `mark_failed` UPDATE is
//!    rejected and the failure verdict silently lost from the audit row. The
//!    CHECK is DROPped by its OBSERVED name and re-ADDed under the auto-name
//!    fresh provisioning yields (`runs_fail_kind_check`) with the record's
//!    literals, so a reconciled schema converges byte-for-byte with a freshly
//!    provisioned one in `pg_constraint`.
//!
//! **Additive only, with one targeted constraint exception:** the plan never
//! drops a live column, table, or index other than the named legacy outbox-era
//! objects and a stale-definition record index; live columns not in the record
//! are SURFACED (`extra_columns`), never touched. The sole constraint it
//! rewrites is the `runs.fail_kind` CHECK above (drop-and-re-add of a widening
//! literal set — every existing row still satisfies it); this is NOT a generic
//! constraint reconciler, scoped strictly to that one CHECK.

use std::collections::{BTreeMap, BTreeSet};

/// The schema of record, compiled in — the same sources provisioning applies
/// (`publish-catalog --runstate`, the f1 provisioning Job) and the wamn-9mg8
/// stand-in drift guard pins.
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The run-plane record files in APPLY ORDER: run-state first (schema header +
/// `runs`, which everything FKs), then the flow registry, then the queue.
const RUN_PLANE_FILES: [&str; 3] = [RUN_STATE_SQL, FLOWS_SQL, RUN_QUEUE_SQL];

/// The outbox-era tables the l5i9.19 teardown retired. A pre-teardown schema
/// (or one restored from a pre-teardown snapshot) still carries them.
pub const LEGACY_OUTBOX_TABLES: [&str; 2] = ["outbox", "evt_shadow"];

/// The constant trigger AND function name the retired wamn-ddl outbox emission
/// used (`CREATE OR REPLACE TRIGGER wamn_outbox_event … EXECUTE FUNCTION
/// wamn_outbox_event()`, one trigger per entity table, the function unqualified
/// so it landed in the apply-time schema).
pub const OUTBOX_TRIGGER_NAME: &str = "wamn_outbox_event";

/// The constraint name Postgres auto-generates for the inline unnamed
/// `runs.fail_kind` CHECK in `run-state.sql` (empirically `runs_fail_kind_check`
/// — the `<table>_<column>_check` rule). The fqg.16 repair re-adds under this
/// name so a reconciled schema converges byte-for-byte with a freshly
/// provisioned one in `pg_constraint`.
const FAIL_KIND_CHECK_NAME: &str = "runs_fail_kind_check";

/// What the driver observed live, scoped to ONE project-env schema (plus the
/// per-database `catalog` metadata schema). Everything here is a read — the
/// pure planner turns it into the action list.
#[derive(Debug, Clone, Default)]
pub struct RunPlaneObservation {
    /// EVERY ordinary table in the target schema → its live column names.
    /// Includes entity/floor tables (ignored by the planner) and any legacy
    /// outbox-era tables (planned for teardown).
    pub tables: BTreeMap<String, BTreeSet<String>>,
    /// EVERY index in the target schema → its live `pg_indexes.indexdef`.
    pub indexes: BTreeMap<String, String>,
    /// Tables in the target schema carrying the legacy `wamn_outbox_event`
    /// trigger.
    pub outbox_trigger_tables: Vec<String>,
    /// Whether the legacy `wamn_outbox_event()` function exists in the target
    /// schema.
    pub outbox_function_present: bool,
    /// Whether the per-database `catalog` metadata schema exists.
    pub catalog_schema_present: bool,
    /// Tables present in the `catalog` schema (empty when the schema is absent).
    pub catalog_tables: BTreeSet<String>,
    /// Rows in `catalog.event_registrations` still carrying the legacy `state`
    /// key (0 when the table is absent — nothing to strip).
    pub stale_registration_state_rows: i64,
    /// The live CHECK constraint on `runs.fail_kind`: `(constraint name,
    /// canonical `pg_get_constraintdef`)`, or `None` when `runs` or the CHECK is
    /// absent. The planner compares its literal set against the record and
    /// repairs legacy literal drift (wamn-fqg.16 — the missing
    /// `'runaway-budget'`).
    pub runs_fail_kind_check: Option<(String, String)>,
}

/// What one plan action does (for reporting; the SQL is on the action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlaneActionKind {
    /// `CREATE SCHEMA IF NOT EXISTS` + role usage grant (the run-state.sql
    /// header, rewritten) — emitted once when any run-plane table is missing.
    EnsureSchema,
    /// Create a missing run-plane table from its record section.
    CreateTable,
    /// Add a record column missing from a present table.
    AddColumn,
    /// Drop the legacy `runs.fail_kind` CHECK (by its observed name) and re-add
    /// it with the record's literals (the missing `'runaway-budget'`).
    RepairFailKindCheck,
    /// Create a record index absent from a present table.
    CreateIndex,
    /// Drop + recreate a present index whose live definition lost a record
    /// column (the pre-E4 claimable index).
    RecreateIndex,
    /// Drop a legacy outbox-era table.
    DropLegacyTable,
    /// Drop a legacy `wamn_outbox_event` trigger from one table.
    DropLegacyTrigger,
    /// Drop the legacy `wamn_outbox_event()` function (after its triggers).
    DropLegacyFunction,
    /// Apply the whole `catalog-schema.sql` (the `catalog` schema is absent).
    EnsureCatalogSchema,
    /// Create a missing `catalog` table from its record section.
    CreateCatalogTable,
    /// Strip the legacy `state` key from stored registrations.
    StripRegistrationState,
}

/// One reconcile action: the SQL to run and what it targets (for reporting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlaneAction {
    pub kind: RunPlaneActionKind,
    /// The table / index / object the action targets (reporting label).
    pub target: String,
    pub sql: String,
}

/// The reconcile plan: ordered actions, plus the record tables already fully at
/// target (reported, never executed) and live columns the record does not know
/// (SURFACED — the plan never drops them). Idempotent: planning against the
/// post-apply state yields no actions.
#[derive(Debug, Clone, Default)]
pub struct RunPlanePlan {
    pub actions: Vec<RunPlaneAction>,
    /// Run-plane record tables present live with full column + index parity.
    pub at_target: Vec<String>,
    /// `(table, column)` live columns not in the record — extra, untouched.
    pub extra_columns: Vec<(String, String)>,
}

impl RunPlanePlan {
    /// Whether there is anything to apply (a no-op reconcile is the expected
    /// steady state and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Reconcile one project-env's run-plane schema (+ the per-database `catalog`
/// metadata schema) against the schema of record. Pure: `obs` is what the
/// driver read; the returned plan is what it should execute, in order.
pub fn plan_run_plane(schema: &str, obs: &RunPlaneObservation) -> RunPlanePlan {
    let mut plan = RunPlanePlan::default();

    // 1. Missing run-plane tables → EnsureSchema once, then per-table sections
    //    in file order (FKs resolve: runs before node_runs/flows/queue).
    let mut any_missing = false;
    let mut creates = Vec::new();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            if obs.tables.contains_key(&table) {
                continue;
            }
            any_missing = true;
            creates.push(RunPlaneAction {
                kind: RunPlaneActionKind::CreateTable,
                target: table.clone(),
                sql: rewrite_schema(&table_section(file, "wamn_run", &table), schema),
            });
        }
    }
    if any_missing {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureSchema,
            target: schema.to_string(),
            sql: rewrite_schema(&header_section(RUN_STATE_SQL, "wamn_run"), schema),
        });
    }
    plan.actions.extend(creates);

    // 2. Column drift on PRESENT record tables: add what the record has and the
    //    live table lacks (record order); surface live extras, never drop them.
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let Some(live_cols) = obs.tables.get(&table) else {
                continue;
            };
            let record_cols = record_columns(file, "wamn_run", &table);
            for (col, def) in &record_cols {
                if !live_cols.contains(col) {
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::AddColumn,
                        target: format!("{table}.{col}"),
                        sql: format!(
                            "ALTER TABLE {}.{} ADD COLUMN {def}",
                            quote_ident(schema),
                            quote_ident(&table),
                        ),
                    });
                }
            }
            let known: BTreeSet<&str> = record_cols.iter().map(|(c, _)| c.as_str()).collect();
            for col in live_cols {
                if !known.contains(col.as_str()) {
                    plan.extra_columns.push((table.clone(), col.clone()));
                }
            }
        }
    }

    // 2b. `runs.fail_kind` CHECK literal drift (wamn-fqg.16): a schema
    //    provisioned before cjv.4 added `'runaway-budget'` carries only the 3
    //    legacy literals, so a runaway `mark_failed` UPDATE is CHECK-rejected and
    //    the verdict lost. Repaired ONLY when `runs` and its `fail_kind` column
    //    are BOTH present live — a missing table/column already gets the record's
    //    inline 4-literal CHECK via CreateTable / AddColumn above. Comparison is
    //    on the LITERAL SET parsed from the canonical `pg_get_constraintdef`
    //    (robust to the `IN (…)` → `= ANY (ARRAY[…])` rewrite pg applies); the
    //    re-add lists the record's literals in record order under the auto-name
    //    for byte-identical convergence with fresh provisioning.
    if obs
        .tables
        .get("runs")
        .is_some_and(|cols| cols.contains("fail_kind"))
    {
        let record = fail_kind_literals(&record_fail_kind_definition());
        let expected: BTreeSet<&str> = record.iter().map(String::as_str).collect();
        let add = format!(
            "ADD CONSTRAINT {} CHECK (fail_kind IN ({}))",
            quote_ident(FAIL_KIND_CHECK_NAME),
            record
                .iter()
                .map(|lit| format!("'{lit}'"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let repair_sql = match &obs.runs_fail_kind_check {
            // Present and admits exactly the record literals → nothing to do.
            Some((_, def))
                if fail_kind_literals(def)
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    == expected =>
            {
                None
            }
            // Drifted: drop the OBSERVED name, re-add the convergent one.
            Some((name, _)) => Some(format!(
                "ALTER TABLE {}.{} DROP CONSTRAINT {}, {add}",
                quote_ident(schema),
                quote_ident("runs"),
                quote_ident(name),
            )),
            // Column present but the CHECK is absent → ADD only.
            None => Some(format!(
                "ALTER TABLE {}.{} {add}",
                quote_ident(schema),
                quote_ident("runs"),
            )),
        };
        if let Some(sql) = repair_sql {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairFailKindCheck,
                target: "runs.fail_kind".to_string(),
                sql,
            });
        }
    }

    // 3. Index drift on PRESENT tables only (a created section carries its own
    //    indexes): absent → create from record; present but the live definition
    //    lost a record column the record definition names → drop + recreate.
    for file in RUN_PLANE_FILES {
        for (name, table, stmt) in index_statements(file, "wamn_run") {
            if !obs.tables.contains_key(&table) {
                continue;
            }
            match obs.indexes.get(&name) {
                None => plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateIndex,
                    target: name.clone(),
                    sql: rewrite_schema(&stmt, schema),
                }),
                Some(live_def) if index_definition_stale(file, &table, &stmt, live_def) => {
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::RecreateIndex,
                        target: name.clone(),
                        sql: format!(
                            "DROP INDEX {}.{}; {}",
                            quote_ident(schema),
                            quote_ident(&name),
                            rewrite_schema(&stmt, schema),
                        ),
                    });
                }
                Some(_) => {}
            }
        }
    }

    // 4. Legacy outbox-era teardown: tables, then triggers BEFORE the function
    //    (DROP FUNCTION is RESTRICT while a trigger still references it).
    for legacy in LEGACY_OUTBOX_TABLES {
        if obs.tables.contains_key(legacy) {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropLegacyTable,
                target: legacy.to_string(),
                sql: format!(
                    "DROP TABLE IF EXISTS {}.{}",
                    quote_ident(schema),
                    quote_ident(legacy),
                ),
            });
        }
    }
    for table in &obs.outbox_trigger_tables {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyTrigger,
            target: table.clone(),
            sql: format!(
                "DROP TRIGGER IF EXISTS {OUTBOX_TRIGGER_NAME} ON {}.{}",
                quote_ident(schema),
                quote_ident(table),
            ),
        });
    }
    if obs.outbox_function_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyFunction,
            target: OUTBOX_TRIGGER_NAME.to_string(),
            sql: format!(
                "DROP FUNCTION IF EXISTS {}.{OUTBOX_TRIGGER_NAME}()",
                quote_ident(schema),
            ),
        });
    }

    // 5. The `catalog` metadata schema (per-database, NOT schema-rewritten):
    //    absent → the whole record file (its CREATE SCHEMA is unguarded);
    //    present → per-table sections for what is missing, in file order.
    if !obs.catalog_schema_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureCatalogSchema,
            target: "catalog".to_string(),
            sql: CATALOG_SCHEMA_SQL.to_string(),
        });
    } else {
        for table in record_tables(CATALOG_SCHEMA_SQL, "catalog") {
            if !obs.catalog_tables.contains(&table) {
                plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateCatalogTable,
                    target: table.clone(),
                    sql: table_section(CATALOG_SCHEMA_SQL, "catalog", &table),
                });
            }
        }
    }
    if obs.stale_registration_state_rows > 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StripRegistrationState,
            target: format!("{} registrations", obs.stale_registration_state_rows),
            sql: strip_registration_state_sql().to_string(),
        });
    }

    // Report the run-plane tables that needed nothing at all.
    let touched: BTreeSet<&str> = plan
        .actions
        .iter()
        .map(|a| a.target.split('.').next().unwrap_or(&a.target))
        .collect();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let index_touched = index_statements(file, "wamn_run")
                .iter()
                .any(|(name, t, _)| *t == table && touched.contains(name.as_str()));
            if obs.tables.contains_key(&table)
                && !touched.contains(table.as_str())
                && !index_touched
            {
                plan.at_target.push(table);
            }
        }
    }

    plan
}

/// A present index is STALE when the record definition names a record column of
/// its table that the live definition does not (word-boundary token match, so
/// `run_id` never matches inside `root_run_id`). This is deliberately the
/// narrow, real drift class — the pre-E4 `run_queue_claimable` without
/// `stream_seq` — not a general definition differ.
fn index_definition_stale(file: &str, table: &str, record_stmt: &str, live_def: &str) -> bool {
    let record_tokens = ident_tokens(record_stmt);
    let live_tokens = ident_tokens(live_def);
    record_columns(file, "wamn_run", table)
        .iter()
        .any(|(col, _)| record_tokens.contains(col.as_str()) && !live_tokens.contains(col.as_str()))
}

/// Identifier-ish tokens of a SQL string: maximal `[A-Za-z0-9_]+` runs.
fn ident_tokens(sql: &str) -> BTreeSet<&str> {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The single-quoted string literals in a SQL fragment, in order. Used to pull
/// the `fail_kind` CHECK literals out of BOTH the record column definition and
/// the live `pg_get_constraintdef` — the drift check compares this list's SET,
/// so the `IN (…)` vs `= ANY (ARRAY[…])` canonicalization pg applies is
/// irrelevant. Assumes literals free of embedded quotes (the fail_kind enum is).
fn fail_kind_literals(sql: &str) -> Vec<String> {
    sql.split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The `runs.fail_kind` column definition from the record — the inline CHECK
/// carries the canonical literal list in record order (the `mark_failed` verdicts
/// the schema must admit).
fn record_fail_kind_definition() -> String {
    record_columns(RUN_STATE_SQL, "wamn_run", "runs")
        .into_iter()
        .find(|(col, _)| col == "fail_kind")
        .expect("runs.fail_kind in the schema of record")
        .1
}

fn quote_ident(s: &str) -> String {
    wamn_ddl::sql::quote_ident(s)
}

/// The legacy registration `state`-key strip (the l5i9.19 teardown runbook): a
/// stored document still carrying `state` fails parse post-teardown, so the
/// materializer HOLDs its flow (delayed-never-lost) until the key is removed.
/// Runs as the superuser (RLS bypassed — the key is legacy across all tenants).
pub fn strip_registration_state_sql() -> &'static str {
    "UPDATE catalog.event_registrations SET registration = registration - 'state' \
     WHERE registration ? 'state'"
}

// ---------------------------------------------------------------------------
// Observation SQL (the shell binds these; pinned by tests like the RI module's
// `select_replica_identity_sql`). SR12: the pure decision has no pg_catalog —
// the throwaway-PG gate covers that these really observe the live state.
// ---------------------------------------------------------------------------

/// Every ordinary table + column in `$1`: `(relname, attname)` in attnum order.
pub fn select_schema_columns_sql() -> &'static str {
    "SELECT c.relname, a.attname FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_attribute a ON a.attrelid = c.oid \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped \
     ORDER BY c.relname, a.attnum"
}

/// Every index in `$1`: `(indexname, indexdef)`.
pub fn select_schema_indexes_sql() -> &'static str {
    "SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = $1"
}

/// Tables in `$1` carrying the legacy `wamn_outbox_event` trigger.
pub fn select_outbox_trigger_tables_sql() -> &'static str {
    "SELECT c.relname FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND t.tgname = 'wamn_outbox_event' AND NOT t.tgisinternal"
}

/// Whether the legacy `wamn_outbox_event()` function exists in `$1`.
pub fn select_outbox_function_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 AND p.proname = 'wamn_outbox_event' )"
}

/// Whether the per-database `catalog` metadata schema exists.
pub fn catalog_schema_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_namespace WHERE nspname = 'catalog' )"
}

/// Rows in `catalog.event_registrations` still carrying the legacy `state` key
/// (the shell runs this only when the table was observed present).
pub fn count_stale_registration_state_sql() -> &'static str {
    "SELECT count(*) FROM catalog.event_registrations WHERE registration ? 'state'"
}

/// The live CHECK constraint on `$1.runs.fail_kind`: `(conname,
/// pg_get_constraintdef)`, or zero rows when `runs`/the CHECK is absent.
/// Identified by CONKEY — the CHECK whose ONLY referenced column is `fail_kind`
/// — never by name, so a legacy auto-name is found regardless of what it is; the
/// fqg.16 repair then DROPs exactly that observed name. `query_opt` in the shell.
pub fn select_runs_fail_kind_check_sql() -> &'static str {
    "SELECT con.conname, pg_get_constraintdef(con.oid) \
     FROM pg_constraint con \
     JOIN pg_class c ON c.oid = con.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relname = 'runs' AND con.contype = 'c' \
       AND con.conkey = ARRAY[( \
         SELECT a.attnum FROM pg_attribute a \
         WHERE a.attrelid = c.oid AND a.attname = 'fail_kind')]"
}

// ---------------------------------------------------------------------------
// Record parsing: slice the deploy/sql sources per table. The files follow the
// repo layout convention (one `CREATE TABLE <q>.<t> (` per table, one column
// per definition start line, full-line `--` comments, statements after the
// table body up to the next CREATE TABLE belong to that table's section); the
// tests below pin the parse against all four shipped files so a layout change
// fails here, not silently.
// ---------------------------------------------------------------------------

/// The canonical deploy DDL rewrite from the `wamn_run` schema to the target
/// project schema (the `publish-catalog --runstate` convention, relocated here
/// as the single owner). The dot-anchored replace leaves prose mentions like
/// `wamn_run_store` untouched; the caller has validated `schema` as a bare
/// lowercase identifier, so bare interpolation is safe.
pub fn rewrite_schema(ddl: &str, schema: &str) -> String {
    ddl.replace("wamn_run.", &format!("{schema}."))
        // The guarded form FIRST: `SCHEMA wamn_run` is not a substring of it, so
        // missing it left `CREATE SCHEMA IF NOT EXISTS wamn_run` unrewritten (the
        // pre-wamn-1wdq bug: publish --runstate silently created a stray
        // `wamn_run` schema on the target DB while publish pre-created the real
        // target — caught by this verb's from-zero gate leg).
        .replace(
            "SCHEMA IF NOT EXISTS wamn_run",
            &format!("SCHEMA IF NOT EXISTS {schema}"),
        )
        .replace("SCHEMA wamn_run", &format!("SCHEMA {schema}"))
}

/// Every `CREATE TABLE <qualifier>.<name>` in `src`, in file order.
fn record_tables(src: &str, qualifier: &str) -> Vec<String> {
    let head = format!("CREATE TABLE {qualifier}.");
    src.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(&head)?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The file header: every line before the first `CREATE TABLE <qualifier>.`.
/// For run-state.sql this is the idempotent `CREATE SCHEMA IF NOT EXISTS` +
/// role usage grant (plus prose comments).
fn header_section(src: &str, qualifier: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.");
    src.lines()
        .take_while(|line| !line.trim().starts_with(&head))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One table's section: from its `CREATE TABLE` line up to (excluding) the next
/// `CREATE TABLE <qualifier>.` line or EOF — the table body plus its indexes,
/// RLS enablement, policy, and grants. Leading comment banners belong to the
/// PREVIOUS section (they are comments; nothing is lost).
fn table_section(src: &str, qualifier: &str, table: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let any_head = format!("CREATE TABLE {qualifier}.");
    let mut out = Vec::new();
    let mut in_section = false;
    for line in src.lines() {
        let t = line.trim();
        if !in_section {
            if t.starts_with(&head) {
                in_section = true;
                out.push(line);
            }
            continue;
        }
        if t.starts_with(&any_head) {
            break;
        }
        out.push(line);
    }
    assert!(
        !out.is_empty(),
        "record parse: no section for {qualifier}.{table} — schema-of-record layout changed"
    );
    out.join("\n")
}

/// The column definitions of `CREATE TABLE <qualifier>.<table> ( … )` in `src`:
/// `(name, full definition)` pairs, in record order, constraints and comments
/// skipped. Parenthesis-depth aware so a multi-line definition (the `runs`
/// status CHECK) parses whole; definitions are whitespace-collapsed for direct
/// use in `ALTER TABLE … ADD COLUMN`.
fn record_columns(src: &str, qualifier: &str, table: &str) -> Vec<(String, String)> {
    const CONSTRAINT_KEYWORDS: [&str; 5] = ["PRIMARY", "FOREIGN", "CONSTRAINT", "CHECK", "UNIQUE"];
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let mut cols = Vec::new();
    let mut in_table = false;
    let mut depth: i32 = 0;
    let mut item: Option<(bool, Vec<String>)> = None; // (is_column, lines)
    let flush = |item: &mut Option<(bool, Vec<String>)>, cols: &mut Vec<(String, String)>| {
        if let Some((true, lines)) = item.take() {
            let def = lines
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let def = def.strip_suffix(',').unwrap_or(&def).to_string();
            let name = def
                .split_whitespace()
                .next()
                .expect("non-empty column definition")
                .to_string();
            cols.push((name, def));
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if !in_table {
            if t.starts_with(&head) {
                in_table = true;
            }
            continue;
        }
        if depth == 0 && item.is_none() && t.starts_with(')') {
            break; // end of the table body
        }
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        if item.is_none() {
            let tok = t.split_whitespace().next().unwrap_or_default();
            let is_column = !CONSTRAINT_KEYWORDS.contains(&tok);
            item = Some((is_column, Vec::new()));
        }
        if let Some((_, lines)) = &mut item {
            lines.push(t.to_string());
        }
        depth += t.chars().filter(|c| *c == '(').count() as i32;
        depth -= t.chars().filter(|c| *c == ')').count() as i32;
        if depth <= 0 && t.ends_with(',') {
            depth = 0;
            flush(&mut item, &mut cols);
        }
        if depth < 0 {
            // The body's closing `)` rode the last item's line; flush and stop.
            flush(&mut item, &mut cols);
            break;
        }
    }
    flush(&mut item, &mut cols);
    assert!(
        !cols.is_empty(),
        "record parse: no columns for {qualifier}.{table} — schema-of-record layout changed"
    );
    cols
}

/// Every `CREATE [UNIQUE] INDEX <name> ON <qualifier>.<table> …;` statement in
/// `src`: `(index name, table, full statement)`.
fn index_statements(src: &str, qualifier: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in src.lines() {
        let t = line.trim();
        match &mut current {
            None if t.starts_with("CREATE INDEX ") || t.starts_with("CREATE UNIQUE INDEX ") => {
                current = Some(vec![t.to_string()]);
            }
            None => continue,
            Some(lines) => lines.push(t.to_string()),
        }
        if t.ends_with(';') {
            let stmt = current.take().expect("complete statement").join(" ");
            let stmt = stmt.strip_suffix(';').unwrap_or(&stmt).to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "INDEX");
            words.next(); // "INDEX"
            let name = words.next().expect("index name").to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "ON");
            words.next(); // "ON"
            let table = words
                .next()
                .expect("index table")
                .trim_start_matches(&format!("{qualifier}."))
                .to_string();
            out.push((name, table, stmt));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the observation the record itself describes: every record table
    /// with its record columns, every record index with the record statement as
    /// its live definition, the catalog schema complete, nothing legacy.
    fn observation_at_record() -> RunPlaneObservation {
        let mut obs = RunPlaneObservation {
            catalog_schema_present: true,
            ..Default::default()
        };
        for file in RUN_PLANE_FILES {
            for table in record_tables(file, "wamn_run") {
                let cols = record_columns(file, "wamn_run", &table)
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect();
                obs.tables.insert(table.clone(), cols);
            }
            for (name, _, stmt) in index_statements(file, "wamn_run") {
                obs.indexes.insert(name, stmt);
            }
        }
        obs.catalog_tables = record_tables(CATALOG_SCHEMA_SQL, "catalog")
            .into_iter()
            .collect();
        // The runs.fail_kind CHECK as fresh provisioning leaves it: the auto-name
        // plus the canonical `= ANY (ARRAY[…])` form pg reports, built from the
        // record literals so the fixture tracks the record.
        let lits = fail_kind_literals(&record_fail_kind_definition());
        obs.runs_fail_kind_check = Some((
            FAIL_KIND_CHECK_NAME.to_string(),
            format!(
                "CHECK ((fail_kind = ANY (ARRAY[{}])))",
                lits.iter()
                    .map(|l| format!("'{l}'::text"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ));
        obs
    }

    #[test]
    fn record_tables_are_pinned() {
        assert_eq!(
            record_tables(RUN_STATE_SQL, "wamn_run"),
            ["runs", "node_runs"]
        );
        assert_eq!(record_tables(FLOWS_SQL, "wamn_run"), ["flows"]);
        assert_eq!(
            record_tables(RUN_QUEUE_SQL, "wamn_run"),
            ["run_queue", "partition_owner", "run_dead_letters"]
        );
        let catalog = record_tables(CATALOG_SCHEMA_SQL, "catalog");
        assert!(catalog.first().is_some_and(|t| t == "catalogs"));
        assert!(catalog.contains(&"event_registrations".to_string()));
        assert_eq!(
            catalog.len(),
            10,
            "catalog-schema.sql table count: {catalog:?}"
        );
    }

    #[test]
    fn run_queue_record_columns_carry_the_drift_set() {
        let cols = record_columns(RUN_QUEUE_SQL, "wamn_run", "run_queue");
        let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "tenant_id",
                "run_id",
                "partition_key",
                "partition_policy",
                "priority",
                "available_at",
                "stream_seq",
                "lease_owner",
                "lease_expires_at",
                "attempts",
                "max_attempts",
                "enqueued_at",
            ]
        );
        // The E4 / D20 definitions the drifted-schema ALTERs are built from.
        let def = |n: &str| cols.iter().find(|(c, _)| c == n).unwrap().1.clone();
        assert_eq!(def("stream_seq"), "stream_seq bigint NOT NULL DEFAULT 0");
        assert_eq!(
            def("partition_policy"),
            "partition_policy text NOT NULL DEFAULT 'blocking' \
             CHECK (partition_policy IN ('blocking', 'leapfrog'))"
        );
    }

    /// The multi-line `runs.status` CHECK parses whole (paren-depth), and
    /// `fail_kind` — the fqg.16 sibling — is present as a column.
    #[test]
    fn multi_line_column_definitions_parse_whole() {
        let cols = record_columns(RUN_STATE_SQL, "wamn_run", "runs");
        let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
        assert!(names.contains(&"status"));
        assert!(names.contains(&"fail_kind"));
        assert!(
            !names.contains(&"'cancelled',"),
            "continuation line misparsed"
        );
        let status = &cols.iter().find(|(c, _)| c == "status").unwrap().1;
        assert!(status.contains("'infrastructure-failure'"), "{status}");
        assert!(
            status.ends_with("))"),
            "CHECK closes inside the definition: {status}"
        );
    }

    /// Sections carry the table's whole apparatus: indexes, RLS, policy, grant.
    #[test]
    fn table_sections_carry_indexes_rls_and_grants() {
        let rq = table_section(RUN_QUEUE_SQL, "wamn_run", "run_queue");
        assert!(rq.contains("CREATE INDEX run_queue_claimable"));
        assert!(rq.contains("CREATE INDEX run_queue_partition"));
        assert!(rq.contains("FORCE ROW LEVEL SECURITY"));
        assert!(!rq.contains("CREATE TABLE wamn_run.partition_owner"));

        let dl = table_section(RUN_QUEUE_SQL, "wamn_run", "run_dead_letters");
        assert!(dl.contains("GRANT SELECT, INSERT ON wamn_run.run_dead_letters"));
        assert!(dl.contains("CREATE POLICY run_dead_letters_tenant"));

        let cat = table_section(CATALOG_SCHEMA_SQL, "catalog", "catalogs");
        assert!(cat.contains("catalogs_one_applied_per_env"));

        let hdr = header_section(RUN_STATE_SQL, "wamn_run");
        assert!(hdr.contains("CREATE SCHEMA IF NOT EXISTS wamn_run"));
        assert!(hdr.contains("GRANT USAGE ON SCHEMA wamn_run TO wamn_app"));
    }

    #[test]
    fn index_statements_are_pinned() {
        let mut names: Vec<String> = RUN_PLANE_FILES
            .iter()
            .flat_map(|f| index_statements(f, "wamn_run"))
            .map(|(n, _, _)| n)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "flows_active",
                "flows_active_webhook_path",
                "node_runs_seq",
                "run_queue_claimable",
                "run_queue_partition",
                "runs_cron_anchor",
                "runs_flow",
                "runs_idempotency",
                "runs_root",
            ]
        );
        let (_, table, stmt) = index_statements(RUN_QUEUE_SQL, "wamn_run")
            .into_iter()
            .find(|(n, _, _)| n == "run_queue_claimable")
            .unwrap();
        assert_eq!(table, "run_queue");
        assert!(stmt.contains("stream_seq"));
        // The multi-line partial expression index parses to one statement.
        let (_, _, wh) = index_statements(FLOWS_SQL, "wamn_run")
            .into_iter()
            .find(|(n, _, _)| n == "flows_active_webhook_path")
            .unwrap();
        assert!(wh.contains("IS NOT NULL"), "{wh}");
    }

    /// THE load-bearing self-consistency invariant: an observation derived from
    /// the record itself plans NOTHING. Whatever the record files evolve into,
    /// a schema at record is a no-op — this is what makes the verb idempotent
    /// at target by construction.
    #[test]
    fn observation_at_record_plans_a_noop() {
        let plan = plan_run_plane("demo", &observation_at_record());
        assert!(plan.is_noop(), "actions: {:#?}", plan.actions);
        assert!(plan.extra_columns.is_empty());
        assert_eq!(
            plan.at_target.len(),
            6,
            "all six run-plane tables at target"
        );
    }

    /// The v1-era drift set (the live 2jkm.41 sweep findings) plans exactly the
    /// additive repairs: E4/D20 columns, the claimable-index recreate, the
    /// missing fqg.20/v8cv tables, the outbox-era teardown, the registration
    /// state strip.
    #[test]
    fn v1_era_drift_plans_the_additive_repairs() {
        let mut obs = observation_at_record();
        // run_queue predates E4 + D20; the claimable index predates stream_seq.
        let rq = obs.tables.get_mut("run_queue").unwrap();
        rq.remove("stream_seq");
        rq.remove("partition_policy");
        obs.indexes.insert(
            "run_queue_claimable".into(),
            "CREATE INDEX run_queue_claimable ON demo.run_queue \
             USING btree (tenant_id, available_at, lease_expires_at)"
                .into(),
        );
        // fqg.20 / v8cv tables not yet provisioned.
        obs.tables.remove("partition_owner");
        obs.tables.remove("run_dead_letters");
        obs.indexes.remove("run_queue_partition");
        // The outbox era: tables + trigger + function + a stored state key.
        obs.tables
            .insert("outbox".into(), BTreeSet::from(["id".into()]));
        obs.tables
            .insert("evt_shadow".into(), BTreeSet::from(["id".into()]));
        obs.outbox_trigger_tables = vec!["receipts".into()];
        obs.outbox_function_present = true;
        obs.stale_registration_state_rows = 2;
        // The catalog schema predates l5i9.16.
        obs.catalog_tables.remove("event_registrations");

        let plan = plan_run_plane("demo", &obs);
        let sqls: Vec<&str> = plan.actions.iter().map(|a| a.sql.as_str()).collect();
        let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();

        assert!(sqls.contains(
            &"ALTER TABLE \"demo\".\"run_queue\" ADD COLUMN stream_seq bigint NOT NULL DEFAULT 0"
        ));
        assert!(sqls.iter().any(|s| s.starts_with(
            "ALTER TABLE \"demo\".\"run_queue\" ADD COLUMN partition_policy text NOT NULL"
        )));
        let recreate = plan
            .actions
            .iter()
            .find(|a| a.kind == RunPlaneActionKind::RecreateIndex)
            .expect("claimable index recreates");
        assert!(
            recreate
                .sql
                .starts_with("DROP INDEX \"demo\".\"run_queue_claimable\"; ")
        );
        assert!(recreate.sql.contains("stream_seq"));
        assert!(plan
            .actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::CreateTable && a.target == "partition_owner"));
        assert!(plan.actions.iter().any(|a| {
            a.kind == RunPlaneActionKind::CreateTable
                && a.target == "run_dead_letters"
                && a.sql
                    .contains("GRANT SELECT, INSERT ON demo.run_dead_letters")
        }));
        assert!(sqls.contains(&"DROP TABLE IF EXISTS \"demo\".\"outbox\""));
        assert!(sqls.contains(&"DROP TABLE IF EXISTS \"demo\".\"evt_shadow\""));
        assert!(
            sqls.contains(&"DROP TRIGGER IF EXISTS wamn_outbox_event ON \"demo\".\"receipts\"")
        );
        assert!(sqls.contains(&"DROP FUNCTION IF EXISTS \"demo\".wamn_outbox_event()"));
        // Trigger drops precede the RESTRICT function drop.
        let trig = kinds
            .iter()
            .position(|k| *k == RunPlaneActionKind::DropLegacyTrigger)
            .unwrap();
        let func = kinds
            .iter()
            .position(|k| *k == RunPlaneActionKind::DropLegacyFunction)
            .unwrap();
        assert!(trig < func);
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::CreateCatalogTable
                    && a.target == "event_registrations")
        );
        assert!(sqls.contains(&strip_registration_state_sql()));
        // Nothing in the plan drops a live COLUMN (additive posture).
        assert!(!sqls.iter().any(|s| s.contains("DROP COLUMN")));
    }

    /// From zero (an empty database): the full run-plane set in FK order behind
    /// the schema ensure, plus the whole catalog schema — the fixture-wipe
    /// restore path (manifestations 3 + 5).
    #[test]
    fn from_zero_plans_the_full_set_in_order() {
        let obs = RunPlaneObservation::default();
        let plan = plan_run_plane("wamn_runner_demo", &obs);
        let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
        assert_eq!(kinds[0], RunPlaneActionKind::EnsureSchema);
        let creates: Vec<&str> = plan
            .actions
            .iter()
            .filter(|a| a.kind == RunPlaneActionKind::CreateTable)
            .map(|a| a.target.as_str())
            .collect();
        assert_eq!(
            creates,
            [
                "runs",
                "node_runs",
                "flows",
                "run_queue",
                "partition_owner",
                "run_dead_letters"
            ]
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::EnsureCatalogSchema)
        );
        // No column/index repairs on tables being created (sections carry them).
        assert!(!kinds.contains(&RunPlaneActionKind::AddColumn));
        assert!(!kinds.contains(&RunPlaneActionKind::CreateIndex));
        // The rewrite reached the sections.
        let rq = plan
            .actions
            .iter()
            .find(|a| a.target == "run_queue")
            .unwrap();
        assert!(rq.sql.contains("CREATE TABLE wamn_runner_demo.run_queue"));
        assert!(!rq.sql.contains("wamn_run."));
    }

    /// A live column the record does not know is SURFACED, never dropped.
    #[test]
    fn extra_live_columns_are_surfaced_not_dropped() {
        let mut obs = observation_at_record();
        obs.tables
            .get_mut("run_queue")
            .unwrap()
            .insert("legacy_x".into());
        let plan = plan_run_plane("demo", &obs);
        assert_eq!(
            plan.extra_columns,
            [("run_queue".to_string(), "legacy_x".to_string())]
        );
        assert!(plan.is_noop(), "extras plan no action: {:#?}", plan.actions);
    }

    /// fqg.16: a schema provisioned before cjv.4 carries the 3-literal legacy
    /// `runs.fail_kind` CHECK → DROP the observed name + ADD the record's 4
    /// literals under the convergent auto-name.
    #[test]
    fn legacy_three_literal_fail_kind_check_plans_drop_and_add() {
        let mut obs = observation_at_record();
        obs.runs_fail_kind_check = Some((
            "runs_fail_kind_check".to_string(),
            "CHECK ((fail_kind = ANY (ARRAY['terminal'::text, \
             'retry-exhausted'::text, 'invalid-input'::text])))"
                .to_string(),
        ));
        let plan = plan_run_plane("demo", &obs);
        let repair = plan
            .actions
            .iter()
            .find(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck)
            .expect("legacy fail_kind CHECK is repaired");
        assert_eq!(repair.target, "runs.fail_kind");
        assert_eq!(
            repair.sql,
            "ALTER TABLE \"demo\".\"runs\" DROP CONSTRAINT \"runs_fail_kind_check\", \
             ADD CONSTRAINT \"runs_fail_kind_check\" CHECK (fail_kind IN \
             ('terminal', 'retry-exhausted', 'invalid-input', 'runaway-budget'))"
        );
        // runs was touched, so it is not reported at target.
        assert!(!plan.at_target.contains(&"runs".to_string()));
    }

    /// The DROP must target the OBSERVED name (never an assumed one) while the
    /// ADD uses the convergent auto-name — proven with a hand-named legacy CHECK.
    #[test]
    fn fail_kind_repair_drops_the_observed_constraint_name() {
        let mut obs = observation_at_record();
        obs.runs_fail_kind_check = Some((
            "legacy_fk_ck".to_string(),
            "CHECK ((fail_kind = ANY (ARRAY['terminal'::text, \
             'retry-exhausted'::text, 'invalid-input'::text])))"
                .to_string(),
        ));
        let repair = plan_run_plane("demo", &obs)
            .actions
            .into_iter()
            .find(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck)
            .expect("repair emitted");
        assert!(
            repair.sql.contains("DROP CONSTRAINT \"legacy_fk_ck\","),
            "drops the observed name: {}",
            repair.sql
        );
        assert!(
            repair
                .sql
                .contains("ADD CONSTRAINT \"runs_fail_kind_check\" CHECK"),
            "re-adds the convergent name: {}",
            repair.sql
        );
    }

    /// Set-equality comparison: the 4 record literals in a different order and
    /// surface form (`IN (…)` vs `= ANY (ARRAY[…])`) plan NO repair.
    #[test]
    fn matching_fail_kind_check_plans_no_repair() {
        let mut obs = observation_at_record();
        obs.runs_fail_kind_check = Some((
            "runs_fail_kind_check".to_string(),
            "CHECK (fail_kind IN ('runaway-budget', 'invalid-input', \
             'retry-exhausted', 'terminal'))"
                .to_string(),
        ));
        let plan = plan_run_plane("demo", &obs);
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck),
            "matching literal set plans no repair: {:#?}",
            plan.actions
        );
        assert!(plan.is_noop());
    }

    /// Column present but the CHECK absent (manually dropped) → ADD only.
    #[test]
    fn absent_fail_kind_check_plans_add_only() {
        let mut obs = observation_at_record();
        obs.runs_fail_kind_check = None;
        let repair = plan_run_plane("demo", &obs)
            .actions
            .into_iter()
            .find(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck)
            .expect("absent CHECK is added");
        assert_eq!(
            repair.sql,
            "ALTER TABLE \"demo\".\"runs\" ADD CONSTRAINT \"runs_fail_kind_check\" \
             CHECK (fail_kind IN \
             ('terminal', 'retry-exhausted', 'invalid-input', 'runaway-budget'))"
        );
        assert!(!repair.sql.contains("DROP CONSTRAINT"));
    }

    /// When `runs` is absent, CreateTable carries the record's inline CHECK — no
    /// separate fail_kind repair fires even against a stale check observation.
    #[test]
    fn fail_kind_check_not_repaired_when_runs_table_absent() {
        let mut obs = observation_at_record();
        obs.tables.remove("runs");
        obs.runs_fail_kind_check = Some((
            "runs_fail_kind_check".to_string(),
            "CHECK ((fail_kind = ANY (ARRAY['terminal'::text])))".to_string(),
        ));
        let plan = plan_run_plane("demo", &obs);
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::RepairFailKindCheck),
            "no fail_kind repair when runs is (re)created"
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::CreateTable && a.target == "runs"),
            "runs is created instead"
        );
    }

    /// The queue-missing manifestation (the live poc_f1 case): run-state +
    /// flows present, queue absent → exactly the three queue creates (+ the
    /// schema ensure, which is idempotent).
    #[test]
    fn queue_missing_plans_only_the_queue_creates() {
        let mut obs = observation_at_record();
        obs.tables.remove("run_queue");
        obs.tables.remove("partition_owner");
        obs.tables.remove("run_dead_letters");
        obs.indexes.remove("run_queue_claimable");
        obs.indexes.remove("run_queue_partition");
        let plan = plan_run_plane("poc_f1", &obs);
        let creates: Vec<&str> = plan
            .actions
            .iter()
            .filter(|a| a.kind == RunPlaneActionKind::CreateTable)
            .map(|a| a.target.as_str())
            .collect();
        assert_eq!(
            creates,
            ["run_queue", "partition_owner", "run_dead_letters"]
        );
        assert!(
            plan.actions
                .iter()
                .all(|a| a.kind != RunPlaneActionKind::AddColumn)
        );
    }

    /// The dot-anchored rewrite (relocated from publish_catalog as the single
    /// owner): qualified names + the schema header rewrite; prose does not.
    #[test]
    fn schema_rewrite_is_dot_anchored() {
        for (ddl, table) in [(RUN_STATE_SQL, "runs"), (FLOWS_SQL, "flows")] {
            let out = rewrite_schema(ddl, "poc_f1");
            assert!(
                out.contains(&format!("CREATE TABLE poc_f1.{table}")),
                "{table}"
            );
            assert!(!out.contains("wamn_run."), "no qualified wamn_run left");
            assert!(!out.contains("SCHEMA wamn_run"), "schema header rewritten");
        }
        // The GUARDED schema-create form rewrites too (the pre-wamn-1wdq bug:
        // `SCHEMA wamn_run` is not a substring of `SCHEMA IF NOT EXISTS
        // wamn_run`, so the header create silently targeted `wamn_run`).
        let out = rewrite_schema(RUN_STATE_SQL, "poc_f1");
        assert!(out.contains("CREATE SCHEMA IF NOT EXISTS poc_f1 "));
        assert!(!out.contains("IF NOT EXISTS wamn_run"));
        // The prose mention of the wamn_run_store crate must survive verbatim.
        assert!(rewrite_schema(RUN_STATE_SQL, "poc_f1").contains("wamn_run_store"));
        assert!(rewrite_schema(RUN_STATE_SQL, "poc_f1").contains("CREATE TABLE poc_f1.node_runs"));
        assert!(
            rewrite_schema(FLOWS_SQL, "poc_f1")
                .contains("CREATE UNIQUE INDEX flows_active_webhook_path ON poc_f1.flows")
        );
    }

    /// Observation SQL pins (the shell binds these verbatim; the live gate
    /// proves they observe real state).
    #[test]
    fn observation_sql_is_pinned() {
        assert!(select_schema_columns_sql().contains("NOT a.attisdropped"));
        assert!(select_schema_indexes_sql().contains("pg_indexes"));
        assert!(select_outbox_trigger_tables_sql().contains("'wamn_outbox_event'"));
        assert!(select_outbox_function_present_sql().contains("pg_proc"));
        assert!(catalog_schema_present_sql().contains("'catalog'"));
        // fqg.16: the fail_kind CHECK is found by CONKEY, not by name.
        assert!(select_runs_fail_kind_check_sql().contains("con.conkey = ARRAY["));
        assert!(select_runs_fail_kind_check_sql().contains("pg_get_constraintdef"));
        assert_eq!(
            strip_registration_state_sql(),
            "UPDATE catalog.event_registrations SET registration = registration - 'state' \
             WHERE registration ? 'state'"
        );
    }
}
