//! The env-symmetric **data copy** plan (wamn-8df.5, D18 §4).
//!
//! One operation over arbitrary `(org, project, env)` triples — same-org or
//! cross-org. Definition promotion is owned by `wamn-ctl promote`; this plan
//! retains only the q3n.10/.11 data-only dump/restore path.
//!
//! This module is **pure** (SR3 / house rule 1): the request/step model, the
//! plan derivation ([`plan_copy`]), and the quiesce/verify SQL + argv builders.
//! No DB, no clock, no process spawn — the effects live in the
//! `copy-project-env` subcommand (`wamn-ctl`), which composes the shipped
//! drivers (`pg_dump`/`pg_restore` for the rows and the registry saga builders
//! for the durable record).
//!
//! **Consistency rule (fixes cjv.7):** a clone into a fresh `dst` needs no
//! quiesce — the src stays live and nobody cuts over. A **cutover** (the src's
//! traffic will move to the dst) gets the mandatory ordered pipeline
//! `Quiesce → Snapshot → Restore → Verify → Cutover [→ DeprovisionOld]`, and the
//! driver refuses the `Cutover` step unless the saga records every prior step —
//! quiesce and verify included — so the dump→flip write-loss window cannot be
//! skipped silently.
//!
use wamn_control_registry::Triple;

use crate::ProvisionError;
use crate::sql::quote_ident;

/// A whole-project data-copy request between two project-env triples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRequest {
    pub src: Triple,
    pub dst: Triple,
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
    /// `pg_restore --data-only --disable-triggers` the snapshot into the dst.
    /// A restore replays state; it does not produce per-row events.
    RestoreData { src: Triple, dst: Triple },
    /// Compare src and dst data-schema table sets and exact row counts.
    Verify { src: Triple, dst: Triple },
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
            CopyStep::RestoreData { .. } => "restore-data",
            CopyStep::Verify { .. } => "verify",
            CopyStep::Cutover { .. } => "cutover",
            CopyStep::DeprovisionOld { .. } => "deprovision-old",
        }
    }
}

/// The fixed saga `kind` admitted by `provisioning.copy_sagas`.
pub const COPY_SAGA_KIND: &str = "copy";

/// Derive the ordered step plan for a copy request.
///
/// * clone (no cutover): `[Snapshot, RestoreData, Verify]`.
/// * cutover (a move): `Quiesce` is prepended and `Cutover`
///   (+ `DeprovisionOld` when requested) appended.
///
/// Rejects a self-copy without cutover (`src == dst` is only meaningful as a
/// move — the same identity re-homed onto a different cluster).
pub fn plan_copy(req: &CopyRequest) -> Result<Vec<CopyStep>, ProvisionError> {
    if req.src == req.dst && !req.cutover {
        return Err(ProvisionError::SelfCopyWithoutCutover {
            triple: req.src.to_string(),
        });
    }

    let mut steps = Vec::new();
    if req.cutover {
        steps.push(CopyStep::Quiesce {
            src: req.src.clone(),
        });
    }
    steps.push(CopyStep::Snapshot {
        src: req.src.clone(),
    });
    steps.push(CopyStep::RestoreData {
        src: req.src.clone(),
        dst: req.dst.clone(),
    });
    steps.push(CopyStep::Verify {
        src: req.src.clone(),
        dst: req.dst.clone(),
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
/// `--disable-triggers` is load-bearing defense-in-depth — any trigger on an
/// entity table would otherwise fire once per restored row (a restore replays
/// state; it does not produce events).
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

    fn req(cutover: bool) -> CopyRequest {
        CopyRequest {
            src: Triple::new("acme", "app", "dev"),
            dst: Triple::new("acme", "app", "prod"),
            cutover,
            deprovision_old: false,
        }
    }

    #[test]
    fn clone_plans_carry_no_quiesce_and_no_cutover() {
        // Clone into a fresh dst: the src stays live — no quiesce, no cutover.
        let steps = plan_copy(&req(false)).unwrap();
        assert!(matches!(steps[0], CopyStep::Snapshot { .. }));
        assert!(matches!(steps[1], CopyStep::RestoreData { .. }));
        assert!(matches!(steps[2], CopyStep::Verify { .. }));
        assert_eq!(steps.len(), 3);
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, CopyStep::Quiesce { .. } | CopyStep::Cutover { .. }))
        );
    }

    #[test]
    fn quiesce_and_verify_precede_cutover() {
        let steps = plan_copy(&req(true)).unwrap();
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

    #[test]
    fn deprovision_old_is_an_opt_in_tail_step() {
        let mut r = req(true);
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
    fn self_copy_needs_cutover() {
        // src == dst is only meaningful as a move (same identity, new cluster).
        let mut r = req(false);
        r.dst = r.src.clone();
        assert!(matches!(
            plan_copy(&r),
            Err(ProvisionError::SelfCopyWithoutCutover { .. })
        ));
        // The tier-move shape (src == dst, cutover) plans fine.
        let mut r = req(true);
        r.dst = r.src.clone();
        assert!(plan_copy(&r).is_ok());
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
        // --disable-triggers is load-bearing: no trigger may fire once per
        // restored row (a restore replays state, it does not produce events).
        let argv = pg_restore_data_only_argv("postgres://u@h/db", "/dump/out", "public");
        assert_eq!(argv[0], "pg_restore");
        assert!(argv.iter().any(|a| a == "--data-only"));
        assert!(argv.iter().any(|a| a == "--disable-triggers"));
        assert!(argv.windows(2).any(|w| w == ["-n", "public"]));
        assert!(argv.windows(2).any(|w| w == ["-d", "postgres://u@h/db"]));
        assert_eq!(argv.last().unwrap(), "/dump/out");
    }
}
