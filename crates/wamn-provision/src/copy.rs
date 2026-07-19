//! The unified env-symmetric **copy** plan (wamn-8df.5, D18 §4).
//!
//! One operation over arbitrary `(org, project, env)` triples — same-org or
//! cross-org: `copy(src, dst, {include, scope, mode})`. It subsumes the 3.4
//! definition promotion (`include: definition`), the q3n.10/.11 dump/restore
//! data path (`include: data`), and the retired tier-move (`include: both` onto
//! a different cluster, with `cutover`). See `docs/deployment-model.md` §4.
//!
//! This module is **pure** (SR3 / house rule 1): the request/step model, the
//! plan derivation ([`plan_copy`]), and the quiesce/verify SQL + argv builders.
//! No DB, no clock, no process spawn — the effects live in the
//! `copy-project-env` subcommand (`wamn-host`), which composes the shipped
//! drivers (the migrate engine for the catalog, `pg_dump`/`pg_restore` for the
//! rows, the registry saga builders for the durable record).
//!
//! **Consistency rule (fixes cjv.7):** a clone into a fresh `dst` needs no
//! quiesce — the src stays live and nobody cuts over. A **cutover** (the src's
//! traffic will move to the dst) gets the mandatory ordered pipeline
//! `Quiesce → Snapshot → Restore → Verify → Cutover [→ DeprovisionOld]`, and the
//! driver refuses the `Cutover` step unless the saga records every prior step —
//! quiesce and verify included — so the dump→flip write-loss window cannot be
//! skipped silently.
//!
//! **Axes representable but not built** (specified in the design doc, rejected
//! here with a named error rather than omitted from the API):
//! `scope: subset(...)` and `mode: live-cutover`.

use wamn_registry::Triple;

use crate::ProvisionError;
use crate::sql::quote_ident;

/// What the copy carries: the app's **definition** (catalog / flows / RLS
/// policies), its **data** (the rows, via the `pg_dump -Fd` artifact), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyInclude {
    /// Structure only — catalog (via the migrate engine), flow registrations,
    /// RLS policy rows + their compiled application. "Deploy an app" / "promote
    /// dev→prod".
    Definition,
    /// Rows only — a `pg_restore --data-only` of the data schema into a dst
    /// that already carries the definition.
    Data,
    /// Everything — a full `pg_dump`/`pg_restore` (schema + rows; the dump
    /// carries the definition tables too, so no separate definition pass).
    Both,
}

impl CopyInclude {
    pub fn as_str(self) -> &'static str {
        match self {
            CopyInclude::Definition => "definition",
            CopyInclude::Data => "data",
            CopyInclude::Both => "both",
        }
    }

    /// Whether the copy carries the structural half (a definition pass runs).
    pub fn wants_definition(self) -> bool {
        matches!(self, CopyInclude::Definition)
    }

    /// Whether the copy carries rows (a dump/restore pass runs).
    pub fn wants_data(self) -> bool {
        matches!(self, CopyInclude::Data | CopyInclude::Both)
    }
}

/// How much of the src is copied. `Subset` is a first-class axis in the API
/// shape but **specified-not-built** — [`plan_copy`] rejects it with
/// [`ProvisionError::UnbuiltCopyAxis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyScope {
    /// The whole project-env.
    Whole,
    /// A referential-integrity-aware slice of a filtered row set (carries the
    /// predicate). Specified in `docs/deployment-model.md` §4; not built.
    Subset(String),
}

/// The copy's consistency mechanism. `LiveCutover` (logical-replication
/// publication/subscription with a lag-monitored switchover) is a first-class
/// axis in the API shape but **specified-not-built** — [`plan_copy`] rejects it
/// with [`ProvisionError::UnbuiltCopyAxis`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    /// A point-in-time `pg_dump` snapshot (quiesced first when cutting over).
    Snapshot,
    /// Logical-replication cutover (near-zero downtime). Specified; not built.
    LiveCutover,
}

/// A copy request between two project-env triples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRequest {
    pub src: Triple,
    pub dst: Triple,
    pub include: CopyInclude,
    pub scope: CopyScope,
    pub mode: CopyMode,
    /// `true` = the src's traffic moves to the dst (a **move**): the mandatory
    /// quiesce → verify → gated-cutover pipeline runs. `false` = a clone into a
    /// fresh dst: the src stays live, no quiesce, no cutover.
    pub cutover: bool,
    /// Append the [`CopyStep::DeprovisionOld`] step to a cutover plan (drop the
    /// retained src database once the cutover is verified). Off by default —
    /// the operator usually keeps the old DB through a hold window.
    pub deprovision_old: bool,
}

/// One step of a copy plan. Each names *what* the driver does; the sequence is
/// the contribution (the retired `tier_move` step-plan precedent). The driver
/// advances the copy saga after each executed step, and the `Cutover` executor
/// re-reads the saga and **refuses** unless every prior step — quiesce and
/// verify included — is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyStep {
    /// Make the src database read-only (`default_transaction_read_only = on` +
    /// terminate existing backends) and prove it with a write probe. Cutover
    /// plans only.
    Quiesce { src: Triple },
    /// `pg_dump -Fd` the src database (the q3n.10 artifact; recorded in
    /// `provisioning.dumps`).
    Snapshot { src: Triple },
    /// Copy the structural half: catalog (migrate engine), flow registrations,
    /// RLS policy rows + compiled application. `include: definition` only —
    /// a full (`both`) dump already carries the definition tables.
    CopyDefinition { src: Triple, dst: Triple },
    /// `pg_restore` the snapshot into the dst database. `data_only` scopes the
    /// restore to the data schema's rows (`--data-only --disable-triggers` —
    /// the D4 outbox triggers must not fire per restored row).
    RestoreData {
        src: Triple,
        dst: Triple,
        data_only: bool,
    },
    /// Compare src and dst (per-table row counts of the data schema; for a
    /// definition copy, applied-document equality + flows/RLS row counts).
    Verify {
        src: Triple,
        dst: Triple,
        include: CopyInclude,
    },
    /// Repoint the serving identity to the dst — refused unless the saga
    /// records every prior step (the cjv.7 gate).
    Cutover { src: Triple, dst: Triple },
    /// Drop the retained src database (opt-in, confirm-gated in the driver).
    DeprovisionOld { src: Triple },
}

impl CopyStep {
    /// A short stable label for saga/target reporting.
    pub fn label(&self) -> &'static str {
        match self {
            CopyStep::Quiesce { .. } => "quiesce",
            CopyStep::Snapshot { .. } => "snapshot",
            CopyStep::CopyDefinition { .. } => "copy-definition",
            CopyStep::RestoreData { .. } => "restore-data",
            CopyStep::Verify { .. } => "verify",
            CopyStep::Cutover { .. } => "cutover",
            CopyStep::DeprovisionOld { .. } => "deprovision-old",
        }
    }
}

/// The saga `kind` a copy pipeline records under (`provisioning.sagas`;
/// admitted by the `sagas_kind_check` literal set in `deploy/sql/system-schema.sql`).
pub const COPY_SAGA_KIND: &str = "copy";

/// Derive the ordered step plan for a copy request.
///
/// * clone (no cutover): `definition` → `[CopyDefinition, Verify]`;
///   `data` → `[Snapshot, RestoreData(data-only), Verify]`;
///   `both` → `[Snapshot, RestoreData(full), Verify]`.
/// * cutover (a move): `Quiesce` is prepended and `Cutover`
///   (+ `DeprovisionOld` when requested) appended. A cutover **requires the
///   data half** (`data` or `both`) — cutting traffic over to a dst that never
///   received the rows abandons them.
///
/// Rejects the specified-not-built axes (`scope: subset`, `mode: live-cutover`)
/// and a self-copy without cutover (`src == dst` is only meaningful as a move —
/// the same identity re-homed onto a different cluster, the tier-move case).
pub fn plan_copy(req: &CopyRequest) -> Result<Vec<CopyStep>, ProvisionError> {
    if let CopyScope::Subset(_) = req.scope {
        return Err(ProvisionError::UnbuiltCopyAxis {
            axis: "scope: subset",
        });
    }
    if req.mode == CopyMode::LiveCutover {
        return Err(ProvisionError::UnbuiltCopyAxis {
            axis: "mode: live-cutover",
        });
    }
    if req.src == req.dst && !req.cutover {
        return Err(ProvisionError::SelfCopyWithoutCutover {
            triple: req.src.to_string(),
        });
    }
    if req.cutover && !req.include.wants_data() {
        return Err(ProvisionError::CutoverNeedsData);
    }

    let mut steps = Vec::new();
    if req.cutover {
        steps.push(CopyStep::Quiesce {
            src: req.src.clone(),
        });
    }
    match req.include {
        CopyInclude::Definition => steps.push(CopyStep::CopyDefinition {
            src: req.src.clone(),
            dst: req.dst.clone(),
        }),
        CopyInclude::Data | CopyInclude::Both => {
            steps.push(CopyStep::Snapshot {
                src: req.src.clone(),
            });
            steps.push(CopyStep::RestoreData {
                src: req.src.clone(),
                dst: req.dst.clone(),
                data_only: req.include == CopyInclude::Data,
            });
        }
    }
    steps.push(CopyStep::Verify {
        src: req.src.clone(),
        dst: req.dst.clone(),
        include: req.include,
    });
    if req.cutover {
        steps.push(CopyStep::Cutover {
            src: req.src.clone(),
            dst: req.dst.clone(),
        });
        if req.deprovision_old {
            steps.push(CopyStep::DeprovisionOld {
                src: req.src.clone(),
            });
        }
    }
    Ok(steps)
}

// --- quiesce / verify SQL + argv builders -----------------------------------

/// Make a database read-only for **new** sessions: every transaction defaults
/// to read-only. Existing sessions keep their old default — pair with
/// [`terminate_database_backends_sql`] so pooled connections re-dial under the
/// new default. Reversible with [`unquiesce_database_sql`]. (A session *can*
/// `SET transaction_read_only = off` — platform code never does, and the D8
/// raw-SQL flag is off platform-wide; the belt-and-braces `REVOKE` variant is a
/// documented alternative, not built.)
pub fn quiesce_database_sql(database: &str) -> String {
    format!(
        "ALTER DATABASE {} SET default_transaction_read_only = on",
        quote_ident(database)
    )
}

/// Reverse [`quiesce_database_sql`] (drop the per-database override).
pub fn unquiesce_database_sql(database: &str) -> String {
    format!(
        "ALTER DATABASE {} RESET default_transaction_read_only",
        quote_ident(database)
    )
}

/// Terminate every backend connected to a database (`$1` = the database name),
/// excluding the caller's own — so sessions opened before the quiesce re-dial
/// and pick up the new read-only default. Run from a *maintenance* database on
/// the same cluster.
pub fn terminate_database_backends_sql() -> &'static str {
    "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
     WHERE datname = $1 AND pid <> pg_backend_pid()"
}

/// List a schema's tables (`$1` = the schema name), ordered — the verify step
/// compares the src and dst table sets before counting rows.
pub fn list_schema_tables_sql() -> &'static str {
    "SELECT tablename FROM pg_tables WHERE schemaname = $1 ORDER BY tablename"
}

/// Exact row count of one table (identifiers quoted — table names come from
/// `pg_tables`, not user input, but quoting keeps the builder total).
pub fn count_rows_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT count(*) FROM {}.{}",
        quote_ident(schema),
        quote_ident(table)
    )
}

/// The `pg_restore` argv for a **data-only** copy: restore just the data
/// schema's rows into a dst that already carries the definition.
/// `--disable-triggers` is load-bearing — the D4 outbox row-event triggers on
/// entity tables would otherwise fire once per restored row, flooding the dst
/// env's dispatcher with spurious firings (a full restore has no such problem:
/// `pg_dump` puts triggers in the post-data section, after the rows load).
/// Requires a superuser connection (which the copy driver holds anyway).
pub fn pg_restore_data_only_argv(conninfo: &str, dump_dir: &str, schema: &str) -> Vec<String> {
    vec![
        "pg_restore".into(),
        "--data-only".into(),
        "--disable-triggers".into(),
        "-n".into(),
        schema.into(),
        "-d".into(),
        conninfo.into(),
        dump_dir.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(include: CopyInclude, cutover: bool) -> CopyRequest {
        CopyRequest {
            src: Triple::new("acme", "app", "dev"),
            dst: Triple::new("acme", "app", "prod"),
            include,
            scope: CopyScope::Whole,
            mode: CopyMode::Snapshot,
            cutover,
            deprovision_old: false,
        }
    }

    #[test]
    fn clone_plans_carry_no_quiesce_and_no_cutover() {
        // Clone into a fresh dst: the src stays live — no quiesce, no cutover.
        let def = plan_copy(&req(CopyInclude::Definition, false)).unwrap();
        assert!(matches!(def[0], CopyStep::CopyDefinition { .. }));
        assert!(matches!(def[1], CopyStep::Verify { .. }));
        assert_eq!(def.len(), 2);

        let both = plan_copy(&req(CopyInclude::Both, false)).unwrap();
        assert!(matches!(both[0], CopyStep::Snapshot { .. }));
        assert!(matches!(
            both[1],
            CopyStep::RestoreData {
                data_only: false,
                ..
            }
        ));
        assert!(matches!(both[2], CopyStep::Verify { .. }));
        assert_eq!(both.len(), 3);
        assert!(
            !both
                .iter()
                .any(|s| matches!(s, CopyStep::Quiesce { .. } | CopyStep::Cutover { .. }))
        );
    }

    #[test]
    fn data_copy_restores_data_only() {
        // include: data = rows only, into a dst that already has the definition.
        let steps = plan_copy(&req(CopyInclude::Data, false)).unwrap();
        assert!(matches!(
            steps[1],
            CopyStep::RestoreData {
                data_only: true,
                ..
            }
        ));
    }

    #[test]
    fn quiesce_and_verify_precede_cutover_in_every_cutover_plan() {
        // The cjv.7 pipeline: Quiesce first, Verify before Cutover — always.
        for include in [CopyInclude::Data, CopyInclude::Both] {
            let steps = plan_copy(&req(include, true)).unwrap();
            let pos = |f: fn(&CopyStep) -> bool| steps.iter().position(f).unwrap();
            let quiesce = pos(|s| matches!(s, CopyStep::Quiesce { .. }));
            let verify = pos(|s| matches!(s, CopyStep::Verify { .. }));
            let cutover = pos(|s| matches!(s, CopyStep::Cutover { .. }));
            assert_eq!(quiesce, 0, "quiesce opens the pipeline");
            assert!(verify < cutover, "verify must be recorded before cutover");
            assert!(
                matches!(steps.last().unwrap(), CopyStep::Cutover { .. }),
                "without deprovision_old the plan ends at cutover"
            );
        }
    }

    #[test]
    fn deprovision_old_is_an_opt_in_tail_step() {
        let mut r = req(CopyInclude::Both, true);
        r.deprovision_old = true;
        let steps = plan_copy(&r).unwrap();
        assert!(matches!(
            steps.last().unwrap(),
            CopyStep::DeprovisionOld { .. }
        ));
        // And precisely one — after the cutover.
        let cutover = steps
            .iter()
            .position(|s| matches!(s, CopyStep::Cutover { .. }))
            .unwrap();
        assert_eq!(cutover, steps.len() - 2);
    }

    #[test]
    fn unbuilt_axes_are_rejected_by_name() {
        // Representable in the API shape, rejected in the plan (specified-not-built).
        let mut r = req(CopyInclude::Both, false);
        r.scope = CopyScope::Subset("supplier_id = 'acme'".into());
        assert!(matches!(
            plan_copy(&r),
            Err(ProvisionError::UnbuiltCopyAxis {
                axis: "scope: subset"
            })
        ));
        let mut r = req(CopyInclude::Both, true);
        r.mode = CopyMode::LiveCutover;
        assert!(matches!(
            plan_copy(&r),
            Err(ProvisionError::UnbuiltCopyAxis {
                axis: "mode: live-cutover"
            })
        ));
    }

    #[test]
    fn self_copy_needs_cutover_and_cutover_needs_data() {
        // src == dst is only meaningful as a move (same identity, new cluster).
        let mut r = req(CopyInclude::Both, false);
        r.dst = r.src.clone();
        assert!(matches!(
            plan_copy(&r),
            Err(ProvisionError::SelfCopyWithoutCutover { .. })
        ));
        // The tier-move shape (src == dst, cutover) plans fine.
        let mut r = req(CopyInclude::Both, true);
        r.dst = r.src.clone();
        assert!(plan_copy(&r).is_ok());
        // A cutover that abandons the rows is refused.
        assert!(matches!(
            plan_copy(&req(CopyInclude::Definition, true)),
            Err(ProvisionError::CutoverNeedsData)
        ));
    }

    #[test]
    fn quiesce_sql_sets_the_read_only_default_and_terminates() {
        assert_eq!(
            quiesce_database_sql("wamn-db-acme--app--dev"),
            "ALTER DATABASE \"wamn-db-acme--app--dev\" SET default_transaction_read_only = on"
        );
        assert_eq!(
            unquiesce_database_sql("wamn-db-acme--app--dev"),
            "ALTER DATABASE \"wamn-db-acme--app--dev\" RESET default_transaction_read_only"
        );
        let term = terminate_database_backends_sql();
        assert!(term.contains("pg_terminate_backend(pid)"));
        assert!(term.contains("datname = $1"));
        assert!(
            term.contains("pid <> pg_backend_pid()"),
            "never terminate the caller's own backend"
        );
    }

    #[test]
    fn verify_builders_list_and_count_exactly() {
        let list = list_schema_tables_sql();
        assert!(list.contains("FROM pg_tables"));
        assert!(list.contains("schemaname = $1"));
        assert!(list.contains("ORDER BY tablename"));
        assert_eq!(
            count_rows_sql("public", "receipts"),
            "SELECT count(*) FROM \"public\".\"receipts\""
        );
    }

    #[test]
    fn data_only_restore_disables_triggers_and_scopes_the_schema() {
        // --disable-triggers is load-bearing: the D4 outbox triggers must not
        // fire once per restored row.
        let argv = pg_restore_data_only_argv("postgres://u@h/db", "/dump/out", "public");
        assert_eq!(argv[0], "pg_restore");
        assert!(argv.iter().any(|a| a == "--data-only"));
        assert!(argv.iter().any(|a| a == "--disable-triggers"));
        assert!(argv.windows(2).any(|w| w == ["-n", "public"]));
        assert!(argv.windows(2).any(|w| w == ["-d", "postgres://u@h/db"]));
        assert_eq!(argv.last().unwrap(), "/dump/out");
    }
}
