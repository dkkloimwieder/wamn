//! Outbox row-event trigger emission — the production producer side of the
//! trigger dispatcher (5.14 / D4).
//!
//! For every generated entity table, emit an `AFTER INSERT OR UPDATE OR DELETE
//! FOR EACH ROW` trigger that inserts one row into the run schema's `outbox`
//! table **inside the user's transaction** — "outbox insert and enqueue can
//! share a transaction with user writes" (D4): the event is durable iff the
//! write it announces is. The dispatcher's poll/fire/ack side is unchanged; it
//! matches on `(table_name, event)` and splices `payload::text` verbatim, so
//! the values written here are exactly what a row-event flow receives.
//!
//! Design points:
//! - **One shared plpgsql function** per project schema (created unqualified,
//!   like the entity tables — it lands in the schema the executor's
//!   `search_path` points at) + one **constant-named** trigger per table.
//!   `CREATE OR REPLACE` on both makes the plan idempotent per catalog version
//!   and rename-safe: a renamed table keeps its trigger, and re-applying
//!   replaces that same-named trigger instead of stacking a duplicate.
//! - **Event vocabulary**: `lower(TG_OP COLLATE \"C\")` yields `insert|update|delete` —
//!   exactly the outbox `event` CHECK and the wamn-flow `row-event` strings.
//!   `TG_TABLE_NAME` is the physical table name row-event flows declare.
//! - **Tenant from the row, not the claim**: `NEW.tenant_id` / `OLD.tenant_id`
//!   (the 3.2 floor column) — correct under superuser seeds too, where no
//!   `app.tenant` claim is set. For a `wamn_app` write the entity floor's
//!   `WITH CHECK` already pinned the row's tenant to the claim, so the outbox
//!   `WITH CHECK (tenant_id = app.tenant)` passes by construction.
//! - **Payload**: `to_jsonb(NEW)` for insert/update, `to_jsonb(OLD)` for
//!   delete. Postgres jsonb numerics are exact — an exact-decimal column
//!   (`12.50`) survives into the payload, and from there verbatim into the run
//!   input (the platform no-float rule holds structurally end to end). The
//!   JSON-type-changing special values are excluded at the source (wamn-oj7):
//!   `to_jsonb` would serialize `'NaN'::numeric` and `'infinity'::timestamptz`
//!   as JSON *strings* (`"NaN"`, `"infinity"`), silently changing a payload
//!   field's JSON type, but the generated-table floor now carries a
//!   `CHECK (col <> 'NaN'::numeric)` on numeric columns and a
//!   `CHECK (col <> '[+-]infinity')` on date/timestamptz columns (emitted by
//!   `column_clause`), so such a value can never be written; the 4.1 gateway
//!   also rejects an infinite timestamp at the REST edge. A payload numeric is
//!   always a JSON number, a payload instant always a finite string.
//!
//! Emission is **opt-in and uniform**: [`crate::Migration::outbox_triggers`]
//! is a separate plan covering ALL entity tables (the dispatcher acks rows no
//! flow is registered on cheaply), composed with the `CREATE` plan by the
//! provisioning path. It is deliberately not folded into
//! [`crate::Migration::create`] — every environment that applies the floor
//! would otherwise need the run schema's `outbox` before any row write.

use wamn_catalog::Catalog;

use crate::plan::{MigrationPlan, Operation, Safety};

/// The shared trigger function name AND the per-table trigger name (trigger
/// names are scoped per table, so a constant is collision-free and survives
/// table renames).
pub(crate) const TRIGGER_NAME: &str = "wamn_outbox_event";

/// Options for outbox-trigger emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxOptions {
    /// The schema holding the `outbox` table (deploy/run-queue.sql). The
    /// trigger function references it schema-qualified — `search_path` inside
    /// a trigger is the caller's and cannot be relied on. Must be a bare
    /// identifier (letters, digits, `_`, not digit-leading); rejected
    /// otherwise, which also keeps the identifier from terminating the
    /// function body's dollar-quoting.
    pub schema: String,
}

impl Default for OutboxOptions {
    fn default() -> Self {
        Self {
            schema: "wamn_run".into(),
        }
    }
}

/// A bare Postgres identifier: `[A-Za-z_][A-Za-z0-9_]*`. Stricter than what
/// quoting could carry, deliberately — the schema name is embedded inside the
/// function body's dollar-quoted block, where quoting does not protect against
/// a value containing the dollar tag.
pub(crate) fn valid_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The shared trigger function. `DELETE` rows carry `OLD` (there is no `NEW`);
/// insert/update carry `NEW`. `lower()` is pinned to the `"C"` collation: the
/// database default may be a locale (Turkish/Azeri) whose case mapping turns
/// `INSERT` into `ınsert` (dotless ı), which fails the outbox `event` CHECK
/// and — because the trigger shares the user's transaction — aborts the user
/// write itself.
fn function_op(opts: &OutboxOptions) -> Operation {
    let outbox = format!(
        "{}.{}",
        crate::sql::quote_ident(&opts.schema),
        crate::sql::quote_ident("outbox")
    );
    let sql = format!(
        "CREATE OR REPLACE FUNCTION {TRIGGER_NAME}() RETURNS trigger\n\
         LANGUAGE plpgsql AS $wamn_outbox$\n\
         BEGIN\n    \
             IF TG_OP = 'DELETE' THEN\n        \
                 INSERT INTO {outbox} (tenant_id, table_name, event, payload)\n        \
                 VALUES (OLD.tenant_id, TG_TABLE_NAME, lower(TG_OP COLLATE \"C\"), to_jsonb(OLD));\n        \
                 RETURN OLD;\n    \
             END IF;\n    \
             INSERT INTO {outbox} (tenant_id, table_name, event, payload)\n    \
             VALUES (NEW.tenant_id, TG_TABLE_NAME, lower(TG_OP COLLATE \"C\"), to_jsonb(NEW));\n    \
             RETURN NEW;\n\
         END\n\
         $wamn_outbox$",
    );
    Operation {
        // The target schema is in the summary so a drifted OutboxOptions on a
        // later re-apply is visible in the plan review, not silent.
        summary: format!("create outbox trigger function {TRIGGER_NAME} (events -> {outbox})"),
        sql,
        safety: Safety::Additive,
        // Catalog-scoped (shared by every entity table) — no single entity.
        entity: String::new(),
        field: None,
        // plpgsql bodies are not plan-checked at CREATE FUNCTION, so a
        // mis-targeted apply succeeds silently and fails on the first write.
        note: Some(format!(
            "requires {outbox} (deploy/run-queue.sql) with INSERT granted to writing roles — \
             if absent, every row write on the entity tables fails at runtime, not at apply"
        )),
    }
}

/// Uniform outbox-trigger plan: the shared function, then one trigger per
/// entity table. All additive; idempotent (`CREATE OR REPLACE` + constant
/// trigger name) so the provisioning path re-applies it per catalog version.
pub(crate) fn outbox_triggers_plan(catalog: &Catalog, opts: &OutboxOptions) -> MigrationPlan {
    let mut plan = MigrationPlan::default();
    plan.push(function_op(opts));
    for e in &catalog.entities {
        let t = &e.name;
        let sql = format!(
            "CREATE OR REPLACE TRIGGER {TRIGGER_NAME}\n    \
             AFTER INSERT OR UPDATE OR DELETE ON {tbl}\n    \
             FOR EACH ROW EXECUTE FUNCTION {TRIGGER_NAME}()",
            tbl = crate::sql::quote_ident(t),
        );
        plan.push(Operation {
            summary: format!("emit row events from {t}"),
            sql,
            safety: Safety::Additive,
            entity: e.id.to_string(),
            field: None,
            note: Some(
                "row writes from now on emit outbox events (a first-time 3.6 seed fires; \
                 an ON CONFLICT DO NOTHING re-seed does not)"
                    .into(),
            ),
        });
    }
    plan
}

/// Opt-out plan: drop every entity table's trigger, then the shared function.
/// Classified destructive — no data is lost, but row-event flows registered on
/// these tables silently stop firing (downstream-breaking, like dropping a
/// unique index's guarantee). A dropped entity needs no counterpart here:
/// `DROP TABLE` removes its trigger with it.
pub(crate) fn drop_outbox_triggers_plan(catalog: &Catalog) -> MigrationPlan {
    let mut plan = MigrationPlan::default();
    for e in &catalog.entities {
        let t = &e.name;
        plan.push(Operation {
            summary: format!("stop emitting row events from {t}"),
            sql: format!(
                "DROP TRIGGER IF EXISTS {TRIGGER_NAME} ON {}",
                crate::sql::quote_ident(t)
            ),
            safety: Safety::Destructive,
            entity: e.id.to_string(),
            field: None,
            note: Some("row-event flows on this table stop firing".into()),
        });
    }
    plan.push(Operation {
        summary: format!("drop outbox trigger function {TRIGGER_NAME}"),
        // Deliberately RESTRICT (no CASCADE): if a table OUTSIDE this catalog
        // still carries the trigger (version drift — e.g. an entity's refused
        // destructive drop left its table behind), this fails loudly instead
        // of silently killing that table's events. The plan is re-runnable:
        // the in-catalog trigger drops above are IF EXISTS.
        sql: format!("DROP FUNCTION IF EXISTS {TRIGGER_NAME}()"),
        safety: Safety::Destructive,
        entity: String::new(),
        field: None,
        note: Some(
            "fails if a table outside this catalog still carries the trigger — \
             re-run with the catalog version whose triggers were actually applied"
                .into(),
        ),
    });
    plan
}
