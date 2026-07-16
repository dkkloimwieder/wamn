//! POC-DM1 — build the Material Receiving data model **via the catalog API**, no
//! UI (`docs/poc-material-receiving.md` §"Data model"; bead wamn-521).
//!
//! This is the API-first build of the POC project data model and the end-to-end
//! acceptance test of the 2.5 migration engine. It **composes the shipped tools**
//! rather than adding logic:
//!
//! - **2.5 [`wamn_migrate`]** — migrate the catalog live into a project database
//!   (the DDL, the lifecycle advance, the history row, in one transaction);
//! - **3.5 [`wamn_rls`]** — the per-role RLS: inspector hold site-scoping + the
//!   ERP receipts-insert gate;
//! - **3.6 [`wamn_seed`]** — the reference/seed data (sites, suppliers, materials,
//!   inspector users carrying the `cert_level` extension);
//! - **2.4 `app_system`** (`deploy/app-schema.sql`) — the auth substrate the
//!   personas' roles + the ERP api-key live in (seeded/asserted by the gate).
//!
//! The three inputs are promoted `deploy/` artifacts:
//! `poc-material-receiving.catalog.json` (drift-guarded == the wamn-catalog
//! fixture), `.rls.json` (the [`wamn_rls::AccessPolicy`]), and
//! `.seed.dataset.json` (the [`wamn_seed::Dataset`]).
//!
//! ## Two known limitations, carried as caveats
//!
//! - **The `is-system` `users` entity migrates to a data-schema `users` table**
//!   carrying `cert_level`, not an `ALTER app_system.users` — wamn-ddl (3.2)
//!   emits a plain `CREATE TABLE` for every entity. So the extension is exercised
//!   at the catalog + DDL level, but as a parallel table to the 2.4
//!   `app_system.users`. Wiring system-entity extension onto `app_system.users`
//!   is a follow-up.
//! - **RLS role/site claims are inert until 4.2.** The plugin injects only
//!   `app.tenant` today; the inspector site-scoping (a new `app.site` claim) and
//!   the ERP `app.role` gate are correct SQL but deny until claim injection lands
//!   (the documented 3.5 deploy-order hazard). The live gate proves them by
//!   setting the claims by hand.

use wamn_catalog::Catalog;
use wamn_migrate::{
    ApplyPlan, Confirmation, Env, MigrationRequest, SqlStatement, Value, plan_migration,
};
use wamn_rls::AccessPolicy;
use wamn_seed::Dataset;

/// The promoted POC catalog (`deploy/poc-material-receiving.catalog.json`) — the
/// 8-entity data model, drift-guarded against the wamn-catalog fixture.
pub const CATALOG_JSON: &str = include_str!("../../../deploy/poc-material-receiving.catalog.json");
/// The POC RLS policy: inspector hold site-scoping + the ERP receipts-insert gate.
pub const POLICY_JSON: &str = include_str!("../../../deploy/poc-material-receiving.rls.json");
/// The POC reference/seed data (sites, suppliers, materials, inspector users).
pub const SEED_JSON: &str =
    include_str!("../../../deploy/poc-material-receiving.seed.dataset.json");

/// The catalog id all three artifacts share.
pub const CATALOG_ID: &str = "poc-material-receiving";

/// The promoted POC catalog.
pub fn catalog() -> Catalog {
    Catalog::from_json(CATALOG_JSON).expect("promoted POC catalog parses")
}

/// The POC RLS policy.
pub fn policy() -> AccessPolicy {
    AccessPolicy::from_json(POLICY_JSON).expect("POC RLS policy parses")
}

/// The POC seed dataset.
pub fn seed() -> Dataset {
    Dataset::from_json(SEED_JSON).expect("POC seed dataset parses")
}

/// Compose the full data-model provisioning SQL for `tenant`, in dependency order:
/// migrate the catalog live (the 2.5 engine's DDL + lifecycle advance + history,
/// in one transaction), then attach the 3.5 RLS policies, then load the 3.6 seed.
///
/// The caller runs it under `search_path = <data schema>, catalog` — the
/// unqualified DDL/policy/seed statements resolve into the data schema, while the
/// migrate metadata writes are `catalog.*`-qualified. This is a **first
/// materialization** (`current = None`), so nothing is dropped and no confirmation
/// gate applies.
pub fn provisioning_sql(tenant: &str) -> Result<String, Box<dyn std::error::Error>> {
    let cat = catalog();

    // 2.5 — migrate the catalog live.
    let request = MigrationRequest {
        tenant,
        environment: Env::new("dev"),
        current: None,
        target: &cat,
        expected_base: None,
        confirm: Confirmation::None,
    };
    let plan = plan_migration(&request)?;
    let mut out = apply_block(&plan);

    // 3.5 — the per-role RLS (additive; gate-free).
    out.push_str("\n-- RLS policies (3.5)\n");
    out.push_str(&wamn_rls::compile(&policy(), &cat)?.sql(Confirmation::None)?);
    out.push('\n');

    // 3.6 — the reference/seed data (additive; idempotent ON CONFLICT (id) DO NOTHING).
    out.push_str("\n-- seed data (3.6)\n");
    out.push_str(&wamn_seed::compile(&seed(), &cat, tenant)?.sql(Confirmation::None)?);
    out.push('\n');

    Ok(out)
}

/// Render a migrate [`ApplyPlan`] as an executable one-transaction SQL block,
/// substituting the `$n` params as literals (the real driver binds them as `$n`
/// — this runs the engine's real builder strings under `psql`). Highest-to-lowest
/// so `$1` never matches inside a `$10`+ placeholder (there are at most 9 params).
fn apply_block(plan: &ApplyPlan) -> String {
    let mut out = String::from("BEGIN;\n");
    for s in &plan.statements {
        let r = render(s);
        let r = r.trim_end();
        out.push_str(r);
        if !r.ends_with(';') {
            out.push(';');
        }
        out.push('\n');
    }
    out.push_str("COMMIT;\n");
    out
}

fn render(stmt: &SqlStatement) -> String {
    let mut sql = stmt.sql.clone();
    for (i, v) in stmt.params.iter().enumerate().rev() {
        let ph = format!("${}", i + 1);
        sql = sql.replace(&ph, &lit(v));
    }
    sql
}

fn lit(v: &Value) -> String {
    match v {
        Value::Text(s) | Value::NullableText(Some(s)) => format!("'{}'", s.replace('\'', "''")),
        Value::NullableText(None) | Value::NullableInt(None) => "NULL".into(),
        Value::Int(i) | Value::NullableInt(Some(i)) => i.to_string(),
        Value::Bool(b) => b.to_string(),
    }
}
