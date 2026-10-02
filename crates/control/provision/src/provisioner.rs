//! The login `wamn_provisioner` of the provisioning worker
//! (`docs/plan/platform-ui.md` §4.10 and §5.5).
//!
//! It runs the role and privilege SQL and is not a superuser: the superuser
//! stays in the CNPG operator. It holds exactly the grants that the role SQL
//! measured on PostgreSQL 18 needs (owner ruling 40 of 2026-10-02 on
//! `wamn-zua8.3`): `CREATEROLE` and `CREATEDB`, `REPLICATION` for the CDC role
//! and slot, `BYPASSRLS` for `reconcile-run-plane`, membership of
//! `pg_signal_backend` to end retired sessions, of `wamn_system` to write the
//! registry, and of `wamn_db_owner`, the owner role of the project schemas,
//! and `createrole_self_grant = 'set, inherit'`, so that it is a member of
//! every role it creates.

use crate::name::DB_OWNER_ROLE;

/// The login of the provisioning worker.
pub const PROVISIONER_ROLE: &str = "wamn_provisioner";

/// Create `wamn_provisioner`, or bring an existing one to exactly its
/// attributes, and grant its memberships.
///
/// A superuser runs it once: `provision-system` on a new deployment, the
/// operator by hand on an installed one (`docs/operations/gcp.md` §7). The
/// roles `wamn_system` and `wamn_db_owner` exist first.
pub fn ensure_provisioner_role_sql() -> String {
    format!(
        "DO $provisioner$ BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{PROVISIONER_ROLE}') THEN \
             CREATE ROLE {PROVISIONER_ROLE} LOGIN NOSUPERUSER CREATEROLE CREATEDB INHERIT \
               REPLICATION BYPASSRLS; \
           ELSE \
             ALTER ROLE {PROVISIONER_ROLE} LOGIN NOSUPERUSER CREATEROLE CREATEDB INHERIT \
               REPLICATION BYPASSRLS; \
           END IF; \
         END $provisioner$;\n\
         GRANT pg_signal_backend, wamn_system, {DB_OWNER_ROLE} TO {PROVISIONER_ROLE};\n\
         ALTER ROLE {PROVISIONER_ROLE} SET createrole_self_grant = 'set, inherit';\n"
    )
}
