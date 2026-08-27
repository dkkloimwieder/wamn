//! The operations-only `impact-report` subcommand (11.8): the read-only **effect shell** for
//! `wamn-schema-control` — it reads the current applied catalog + a `--target`, compiles
//! the migration plan (the same wamn-schema-compiler compiler `migrate-catalog` uses), reads
//! the dependency edges (event registrations)
//! across ALL tenants on a superuser connection, and prints the typed
//! [`wamn_schema_control::impact::ImpactReport`]. It **mutates nothing** — the schema-designer
//! surface for "what breaks if I apply this".
//!
//! The pure decision is `wamn_schema_control::impact::analyze`; this shell only
//! holds the connection (SR6).
//!
//! **Tenant scoping.** The registration read is CROSS-TENANT (the superuser
//! bypasses RLS): a shared entity's change hits every tenant's flows, so the
//! report must see them all — the per-edge lines carry their tenant.
//!
//! **An unreadable registration set is SAID, never rendered as an empty one**
//! (wamn-0h0g.12.120). See [`UnevaluatedRegistrations`].

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wamn_schema_control::Env;
use wamn_schema_control::MigrationPlan;
use wamn_schema_control::impact::{ImpactInput, ImpactReport, RegistrationEdge, analyze};
use wamn_schema_model::Catalog;

use crate::migrate_catalog::{is_bare_ident, read_current_applied};

/// Which of the two unreadable states the registration read landed in.
///
/// Same PAIR, same `<subsystem>-registrations-<state>` naming, and same
/// `row_security = off` detection as the two siblings that share this mechanism:
/// `wamn-0h0g.12.103`'s REPLICA IDENTITY reconcile and `wamn-0h0g.12.119`'s D24
/// orphan guard, both spelled by `wamn_schema_control::UnreadableRegistrations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnevaluatedRegistrationsKind {
    /// `catalog.event_registrations` does not exist: this project-env is not
    /// registration-provisioned. NOT the same state as a provisioned table
    /// holding no rows, which really does mean no flow depends on the change.
    Absent,
    /// The cross-tenant read itself failed. Chiefly the silent case: the table is
    /// `FORCE ROW LEVEL SECURITY`, so a session that does not BYPASS RLS reads
    /// zero rows with no error at all — the table's own non-superuser owner
    /// included, since FORCE strips the owner's exemption and PostgreSQL checks
    /// BYPASSRLS on the current role only, never through inherited membership.
    /// The read runs under `row_security = off` so that silence becomes an error
    /// and lands here.
    Unreadable,
}

impl UnevaluatedRegistrationsKind {
    /// The stable name the report prints and a test keys on.
    pub fn name(self) -> &'static str {
        match self {
            Self::Absent => "impact-report-registrations-absent",
            Self::Unreadable => "impact-report-registrations-unreadable",
        }
    }
}

/// The registration edge class was NOT EVALUATED, said out loud in the report.
///
/// **Why this reports rather than refuses, and why it is not
/// `wamn_schema_control::UnreadableRegistrations`.** The two siblings on this
/// mechanism refuse: their empty reading silently RESETS every REPLICA IDENTITY
/// (`wamn-0h0g.12.103`) or silently CLEARS a destructive apply
/// (`wamn-0h0g.12.119`). Both are effects, and an effect that cannot see its
/// evidence must not happen. `impact-report` mutates nothing; refusing would take
/// away the operator's only view of the destructive change while telling them
/// nothing they could not be told. So the disposition differs — the DETECTION,
/// the absent/unreadable pair and the refusal-name shape are deliberately
/// identical, and the one thing all three forbid is the same: an empty set
/// standing in for an unread one.
///
/// That silence is not theoretical here. `ImpactReport::render` prints
/// `(no dependent flows)` under every entity with an empty edge list, so an
/// absent registration table makes the report state, in words, the exact
/// conclusion the operator is deciding on. That is why the notice is rendered
/// BEFORE the entity list rather than appended after it.
///
/// Only the ABSENT arm is reachable through the shipped verb today: `run` connects
/// on `--admin-database-url`, a superuser, and `row_security = off` is inert for a
/// role that already bypasses RLS. The unreadable arm is carried because the
/// connection input is an operator-supplied URL, not because a shipped caller
/// reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnevaluatedRegistrations {
    pub kind: UnevaluatedRegistrationsKind,
    /// The catalog whose registrations could not be read.
    pub catalog_id: String,
    /// The database's own words when the read failed, so the operator is not
    /// asked to guess which credential to fix.
    pub detail: Option<String>,
}

impl fmt::Display for UnevaluatedRegistrations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: registration edges were NOT EVALUATED for catalog {:?} — ",
            self.kind.name(),
            self.catalog_id
        )?;
        match self.kind {
            UnevaluatedRegistrationsKind::Absent => write!(
                formatter,
                "catalog.event_registrations does not exist, so this project-env is not \
                 registration-provisioned. Every \"(no dependent flows)\" line below means \
                 NOT MEASURED, not NONE: this report UNDERSTATES the blast radius by an \
                 unknown amount and must not be read as clearing a destructive change. \
                 Provision the catalog schema and re-run."
            )?,
            UnevaluatedRegistrationsKind::Unreadable => write!(
                formatter,
                "the cross-tenant read of catalog.event_registrations failed. It must run as \
                 a role that BYPASSES row-level security (a superuser or a BYPASSRLS role): \
                 the table is FORCE ROW LEVEL SECURITY, so a non-bypassing session — the \
                 table's own non-superuser owner included — reads ZERO ROWS WITH NO ERROR, \
                 which would have been rendered as \"(no dependent flows)\" under every \
                 entity. The read runs under `row_security = off` so that silence surfaces \
                 here instead. Every \"(no dependent flows)\" line below means NOT MEASURED. \
                 Re-run with a bypassing credential."
            )?,
        }
        if let Some(detail) = &self.detail {
            write!(formatter, " Database said: {detail}")?;
        }
        Ok(())
    }
}

/// An impact report plus whether its registration edge class was evaluated.
///
/// The two travel together because the report ALONE cannot be read correctly:
/// an empty edge list and an unread one render identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactReadout {
    pub report: ImpactReport,
    /// `None` when the registration set was read. `Some` names the class that was
    /// not evaluated.
    pub unevaluated_registrations: Option<UnevaluatedRegistrations>,
}

impl ImpactReadout {
    /// The operator-facing rendering: the caveat FIRST, then the report.
    ///
    /// Order is the whole point. `ImpactReport::render` writes
    /// `(no dependent flows)` under every entity, so a caveat appended afterwards
    /// is read after the conclusion it invalidates.
    pub fn render(&self) -> String {
        let Some(unevaluated) = &self.unevaluated_registrations else {
            return self.report.render();
        };
        let mut out = format!("WARNING: {unevaluated}\n");
        if self.report.any_destructive() {
            out.push_str(
                "WARNING: this report contains a DESTRUCTIVE change whose dependent flows \
                 were not evaluated. Do not use it to decide that the change is safe.\n",
            );
        }
        out.push_str(&self.report.render());
        out
    }
}

#[derive(Debug, Args)]
pub struct ImpactReportArgs {
    /// Superuser Postgres URL to the PROJECT database (the `catalog` metadata
    /// schema + the data/flow schema). Cross-tenant reads need the superuser (RLS
    /// bypass), like the D24 guard. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Tenant the catalog version is scoped to (the current-applied lookup key).
    /// The impact picture is inherently multi-tenant for the registration edge —
    /// the report is grouped by the affected entity and each edge names its tenant.
    #[arg(long)]
    pub tenant: String,

    /// Environment slug the catalog version is tagged with (default `dev`).
    #[arg(long, default_value = "dev")]
    pub environment: String,

    /// The schema holding the data tables (the `catalog` metadata schema is
    /// fixed).
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Path to the target catalog JSON (crates/schema/model `Catalog`).
    #[arg(long)]
    pub target: PathBuf,
}

pub async fn run(args: ImpactReportArgs) -> anyhow::Result<()> {
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let target_json = std::fs::read_to_string(&args.target)
        .with_context(|| format!("read target catalog {}", args.target.display()))?;
    let target = Catalog::from_json(&target_json).context("parse target catalog JSON")?;
    let env = Env::new(&args.environment);

    let (mut client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);

    // Read the current applied catalog read-only (drop the tx before any edge
    // read): the whole verb mutates nothing.
    let tx = client.transaction().await.context("begin")?;
    tx.batch_execute(&format!(
        "SET LOCAL search_path = {schema}, catalog",
        schema = args.schema
    ))
    .await
    .context("set search_path")?;
    let current = read_current_applied(&tx, &args.tenant, &target.catalog_id, env.as_str()).await?;
    drop(tx);

    let plan = compile_plan(current.as_ref(), &target)?;
    println!("-- schema diff --\n{}", plan.report());

    let readout = gather_impact(&client, &plan, current.as_ref(), &target).await?;
    conn_task.abort();
    println!("{}", readout.render());
    if readout
        .report
        .entities
        .iter()
        .any(|entity| entity.destructive && entity.has_downstream_impact())
    {
        println!(
            "NOTE: this destructive change has dependent flows; use this report \
             when reprovisioning the environment."
        );
    }
    Ok(())
}

/// Compile a migration plan for operations impact analysis with the same
/// wamn-schema-compiler compiler the catalog applier uses — `migrate` from the
/// current applied version, or a whole-catalog `create` for a first
/// materialization. The per-op entity +
/// additive/destructive classification is the authoritative "affected entities"
/// source (no SQL re-parse).
pub fn compile_plan(current: Option<&Catalog>, target: &Catalog) -> anyhow::Result<MigrationPlan> {
    wamn_schema_control::ops::compile_migration(current, target)
        .map_err(|e| anyhow::anyhow!("compile migration for impact analysis: {e}"))
}

/// Read the dependency edges for `plan` and fold them through
/// `wamn_schema_control::impact::analyze`.
///
/// Cross-tenant, superuser (RLS bypassed).
///
/// **An unread registration set is REPORTED, never folded in as an empty one**
/// (wamn-0h0g.12.120). The `to_regclass` probe used to make an absent table "a
/// clean empty", which `analyze` then rendered as `(no dependent flows)` under
/// every entity — an operator deciding whether a destructive migrate is safe read
/// a not-provisioned environment as an unaffected one. Both unread states now
/// come back in [`ImpactReadout::unevaluated_registrations`], and the read runs
/// under `row_security = off` so the FORCE-RLS silence surfaces as an error
/// rather than as zero rows. See [`UnevaluatedRegistrations`] for why this
/// reports where its two siblings refuse.
pub async fn gather_impact(
    client: &tokio_postgres::Client,
    plan: &MigrationPlan,
    current: Option<&Catalog>,
    target: &Catalog,
) -> anyhow::Result<ImpactReadout> {
    // Edge 2: event registrations (id-keyed) — the D24 read + flow_id.
    let mut registrations = Vec::new();
    let mut unevaluated_registrations = None;
    if table_present(client, "catalog.event_registrations").await? {
        // Best-effort restore below: this verb owns its own connection and opens
        // no transaction around the read, so a failure leaves the GUC set on a
        // client that is about to be dropped either way.
        client
            .batch_execute("SET row_security = off")
            .await
            .context("SET row_security = off for the cross-tenant registration read")?;
        let read = client
            .query(
                &wamn_schema_control::sql::select_registration_flow_refs_for_catalog_sql(),
                &[&target.catalog_id],
            )
            .await;
        let _ = client.batch_execute("RESET row_security").await;
        match read {
            Ok(rows) => {
                for row in &rows {
                    registrations.push(RegistrationEdge {
                        registration_id: row.get(0),
                        tenant: row.get(1),
                        entity_id: row.get(2),
                        flow_id: row.get(3),
                    });
                }
            }
            Err(error) => {
                // The SERVER's own SQLSTATE and message, not `Error`'s Display —
                // that renders every database failure as the literal string "db
                // error", which tells an operator nothing about which credential
                // to fix.
                let detail = match error.as_db_error() {
                    Some(db) => format!("SQLSTATE {}: {}", db.code().code(), db.message()),
                    None => error.to_string(),
                };
                unevaluated_registrations = Some(UnevaluatedRegistrations {
                    kind: UnevaluatedRegistrationsKind::Unreadable,
                    catalog_id: target.catalog_id.clone(),
                    detail: Some(detail),
                });
            }
        }
    } else {
        unevaluated_registrations = Some(UnevaluatedRegistrations {
            kind: UnevaluatedRegistrationsKind::Absent,
            catalog_id: target.catalog_id.clone(),
            detail: None,
        });
    }

    Ok(ImpactReadout {
        report: analyze(&ImpactInput {
            plan,
            current,
            target,
            registrations: &registrations,
        }),
        unevaluated_registrations,
    })
}

/// Whether a (schema-qualified) relation exists — the D24 guard's probe shape.
/// `qualified` is a fixed `catalog.*` name, so the interpolation is safe.
async fn table_present(client: &tokio_postgres::Client, qualified: &str) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            &format!("SELECT to_regclass('{qualified}') IS NOT NULL"),
            &[],
        )
        .await
        .with_context(|| format!("probe {qualified}"))?
        .get(0))
}

#[cfg(test)]
mod tests {
    use wamn_schema_control::impact::{EntityChangeKind, EntityImpact, ImpactReport};

    use super::{ImpactReadout, UnevaluatedRegistrations, UnevaluatedRegistrationsKind};

    /// One destructive entity with an empty edge list: the exact shape an
    /// unreadable registration set produces, and the shape a real "nothing
    /// depends on this" answer produces. The render is the only thing that can
    /// tell them apart.
    fn destructive_report() -> ImpactReport {
        ImpactReport {
            entities: vec![EntityImpact {
                entity_id: "touched".to_string(),
                entity_name: "orders".to_string(),
                change: EntityChangeKind::Changed,
                destructive: true,
                flows_via_registration: Vec::new(),
                api_resources: vec!["/api/rest/orders".to_string()],
            }],
        }
    }

    /// The pure half of wamn-0h0g.12.120: a mutant that only a live gate kills
    /// ships green in the ordinary sweep.
    #[test]
    fn an_unevaluated_registration_set_is_named_ahead_of_the_entity_lines() {
        for kind in [
            UnevaluatedRegistrationsKind::Absent,
            UnevaluatedRegistrationsKind::Unreadable,
        ] {
            let rendered = ImpactReadout {
                report: destructive_report(),
                unevaluated_registrations: Some(UnevaluatedRegistrations {
                    kind,
                    catalog_id: "shop".to_string(),
                    detail: None,
                }),
            }
            .render();

            assert!(rendered.contains(kind.name()), "{rendered}");
            assert!(rendered.contains("NOT EVALUATED"), "{rendered}");
            assert!(rendered.contains("shop"), "{rendered}");
            // The destructive change is called out separately: the class the
            // operator is deciding on is the one that was not measured.
            assert!(rendered.contains("DESTRUCTIVE change"), "{rendered}");

            // ORDER IS THE POINT. `ImpactReport::render` writes
            // `(no dependent flows)` under this entity, so a caveat printed after
            // it is read after the conclusion it invalidates.
            let caveat = rendered
                .find(kind.name())
                .expect("the caveat is in the render");
            let conclusion = rendered
                .find("(no dependent flows)")
                .expect("the report still states the empty edge list it always did");
            assert!(
                caveat < conclusion,
                "the caveat must precede the line it qualifies:\n{rendered}"
            );
        }
    }

    /// The two names are frozen: a live gate and an operator runbook key on them.
    #[test]
    fn the_unevaluated_registration_names_are_the_sibling_shape() {
        assert_eq!(
            UnevaluatedRegistrationsKind::Absent.name(),
            "impact-report-registrations-absent"
        );
        assert_eq!(
            UnevaluatedRegistrationsKind::Unreadable.name(),
            "impact-report-registrations-unreadable"
        );
    }

    /// A registration set that WAS read adds nothing: the caveat must not become
    /// wallpaper an operator learns to skip.
    #[test]
    fn a_read_registration_set_renders_exactly_the_report() {
        let report = destructive_report();
        let readout = ImpactReadout {
            report: report.clone(),
            unevaluated_registrations: None,
        };
        assert_eq!(readout.render(), report.render());
        assert!(!readout.render().contains("NOT EVALUATED"));
    }
}
