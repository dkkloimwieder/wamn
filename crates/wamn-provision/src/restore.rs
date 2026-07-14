//! Rendering the **restore** of a per-project-env logical dump (wamn-q3n.11).
//!
//! The restore counterpart of [`crate::dump`]: given a `pg_dump -Fd` **directory**
//! artifact (the one [`crate::dump`] produces), `pg_restore` it into a target
//! database. Two targets, the safe one the default (docs/postgres-topology.md
//! §Backup architecture, restore runbook):
//!
//! * **scratch database** (default, non-destructive): restore into a fresh
//!   `wamn-restore-<org>--<project>--<env>` database so the dump can be inspected /
//!   a single table carved out without touching the live project-env DB — the
//!   sub-cluster carve-out path;
//! * **in place** (destructive, `--confirm`-gated in the subcommand): `pg_restore
//!   --clean --if-exists` over the live project-env database — restore-to-last-dump.
//!
//! This module is **pure** (SR3 / house rule 1): the `pg_restore` argv builder and
//! the scratch-database naming. No DB, no clock, no `pg_restore` invocation — the
//! effects (running the restore, creating the scratch DB, reading the dump catalog)
//! live in the `restore-project-env` subcommand (`wamn-host`).
//!
//! **Object store (Q2, the .10 stance):** the dump bytes live in object storage
//! once the shared store lands (wamn-e1g); until then a dump is staged locally (the
//! `dump-project-env --run-now --out-dir` output). The dump **catalog**
//! (`provisioning.dumps`, [`crate::sql`] via `wamn_registry`) records *which* dump
//! is latest so restore-to-last-dump needs no manual key; the physical bytes come
//! from the local dump directory. The whole-cluster **PITR** carve-out (restore an
//! org cluster to an arbitrary instant, then carve one DB out) needs WAL/PITR and
//! is wamn-e1g — this module restores from a *logical dump*, not a base backup.

use wamn_registry::Triple;

use crate::name::MAX_DB_NAME_LEN;

/// Prefix for a **restore-into-scratch** database name: `wamn-restore-<org>--
/// <project>--<env>`. Under the platform-reserved `wamn` prefix (wamn-66x); the
/// `--` separator matches the db/Secret naming ([`crate::name`]). Distinct from the
/// live `wamn-db-…` database so a scratch restore never shadows the real one.
pub const RESTORE_SCRATCH_PREFIX: &str = "wamn-restore-";

/// The scratch-restore database name for a project-env: `wamn-restore-<org>--
/// <project>--<env>`. The non-destructive default target — restore lands here so
/// the dump can be inspected or a table carved out without touching the live DB.
/// Validate its length with [`validate_restore_scratch_name`] before use.
pub fn restore_scratch_db_name(triple: &Triple) -> String {
    format!(
        "{RESTORE_SCRATCH_PREFIX}{}--{}--{}",
        triple.org,
        triple.project,
        triple.env.as_str()
    )
}

/// Validate that a project-env's scratch-restore database name fits the Postgres
/// identifier / DNS-1123 label limit ([`MAX_DB_NAME_LEN`]). The scratch prefix is
/// longer than the live `wamn-db-` prefix, so a triple that fits the live database
/// name can still overflow the scratch one — bound it explicitly (the
/// [`crate::dump::validate_dump_resource_name`] pattern).
pub fn validate_restore_scratch_name(triple: &Triple) -> Result<(), crate::ProvisionError> {
    let name = restore_scratch_db_name(triple);
    if name.len() > MAX_DB_NAME_LEN {
        return Err(crate::ProvisionError::NameTooLong {
            name,
            max: MAX_DB_NAME_LEN,
        });
    }
    Ok(())
}

/// The `pg_restore` argv for a directory-format dump. `conninfo` is a full
/// connection URL (`pg_restore -d`), `dump_dir` the `pg_dump -Fd` directory.
///
/// * `--no-owner --no-privileges` — restore the **data**, not the source's role
///   ownership/ACLs (the scratch DB / in-place DB is owned by `wamn_app`, not the
///   dump's roles; the [`crate::dump`] round-trip gate's stance). Ownership is
///   re-established by the provisioning path, not the dump.
/// * `clean` (in-place only) adds `--clean --if-exists`: **drop** each object
///   before recreating it, so restoring over the live, populated project-env
///   database replaces it cleanly rather than erroring on / appending to existing
///   objects. A scratch restore into a fresh empty database passes `clean = false`.
pub fn pg_restore_argv(conninfo: &str, dump_dir: &str, clean: bool) -> Vec<String> {
    let mut argv = vec![
        "pg_restore".into(),
        "--no-owner".into(),
        "--no-privileges".into(),
    ];
    if clean {
        // Drop each object before recreating — an in-place restore over a populated
        // database. `--if-exists` avoids errors on objects the dump did not create.
        argv.push("--clean".into());
        argv.push("--if-exists".into());
    }
    argv.push("-d".into());
    argv.push(conninfo.into());
    argv.push(dump_dir.into());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_registry::Env;

    fn t() -> Triple {
        Triple::new("acme", "billing", Env::Dev)
    }

    #[test]
    fn scratch_name_is_a_distinct_derivable_name() {
        assert_eq!(
            restore_scratch_db_name(&t()),
            "wamn-restore-acme--billing--dev"
        );
        // The scratch name never collides with the live `wamn-db-…` database.
        assert_ne!(
            restore_scratch_db_name(&t()),
            crate::project_env_database_name("acme", "billing", Env::Dev)
        );
        // The prod and dev envs of one project get distinct scratch names.
        let prod = Triple::new("acme", "billing", Env::Prod);
        assert_ne!(
            restore_scratch_db_name(&t()),
            restore_scratch_db_name(&prod)
        );
    }

    #[test]
    fn scratch_name_length_is_bounded() {
        assert!(validate_restore_scratch_name(&t()).is_ok());
        // A pathologically long triple overflows the identifier bound (the scratch
        // prefix is longer than the live `wamn-db-` prefix).
        let long = Triple::new("o".repeat(30), "p".repeat(30), Env::Prod);
        assert!(matches!(
            validate_restore_scratch_name(&long),
            Err(crate::ProvisionError::NameTooLong { max: 63, .. })
        ));
    }

    #[test]
    fn scratch_restore_argv_restores_data_without_dropping_objects() {
        // The non-destructive default (into a fresh empty scratch DB): no --clean.
        let argv = pg_restore_argv("postgres://u@h/scratch", "/dump/out", false);
        assert_eq!(argv[0], "pg_restore");
        // Restore the DATA, not the source roles/ACLs.
        assert!(argv.iter().any(|a| a == "--no-owner"));
        assert!(argv.iter().any(|a| a == "--no-privileges"));
        // No object-dropping on a scratch restore.
        assert!(!argv.iter().any(|a| a == "--clean"));
        // Connection + dump directory are separate argv (no shell splice); the dump
        // directory is the trailing positional.
        assert!(
            argv.windows(2)
                .any(|w| w == ["-d", "postgres://u@h/scratch"])
        );
        assert_eq!(argv.last().unwrap(), "/dump/out");
    }

    #[test]
    fn in_place_restore_argv_cleans_before_restoring() {
        // The destructive in-place path: drop each object first so a restore over a
        // populated database replaces it rather than appending.
        let argv = pg_restore_argv(
            "postgres://u@h/wamn-db-acme--billing--dev",
            "/dump/out",
            true,
        );
        assert!(argv.iter().any(|a| a == "--clean"));
        assert!(argv.iter().any(|a| a == "--if-exists"));
        // --clean still restores DATA (no owner/privilege restore).
        assert!(argv.iter().any(|a| a == "--no-owner"));
        assert_eq!(argv.last().unwrap(), "/dump/out");
    }
}
