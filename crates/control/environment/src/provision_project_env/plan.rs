use super::{APP_ROLE, WorkloadActionVerb, WorkloadRoleFamily, escape_identifier, sql};

/// `--<verb>-<family>-generation`, the flag one family's one verb answers to.
pub fn workload_action_flag(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    format!("{}-{}-generation", verb.as_str(), family.cli_stem())
}

/// `--emit-<family>-secret`, the path one family's prepared credential is
/// written to.
pub fn workload_secret_flag(family: WorkloadRoleFamily) -> String {
    format!("emit-{}-secret", family.cli_stem())
}

/// The role batch the runbook applies to the TARGET cluster's superuser before
/// the `Database` CR (step 1). Both `wamn_app` and `wamn_db_owner` precede the
/// CR because `wamn_db_owner` is its `spec.owner` and the CR cannot reconcile
/// against a role that does not exist yet.
///
/// It takes no app password either (`wamn-xv69`). The app role is the same
/// stable passwordless NOLOGIN carrier as every other generation family.
///
/// psql commits each complete statement in this standalone artifact. That
/// makes the app-role hardening visible before the final bounded session drain.
/// Rust callers needing the same ordering apply [`role_posture_sql`], await its
/// commit, then apply [`sql::drain_app_role_sessions_sql`].
///
/// `pub` so the live acceptance test applies the SAME fragments production uses instead of
/// hand-transcribed copies — the `reconcile_run_plane::reconcile` precedent.
pub fn role_sql() -> String {
    format!(
        "{posture}\n{drain}\n",
        posture = role_posture_sql(),
        drain = sql::drain_app_role_sessions_sql(),
    )
}

/// Role creation and hardening half of [`role_sql`].
pub fn role_posture_sql() -> String {
    format!(
        "{app}\n{owner}\n",
        app = sql::ensure_app_acl_role_sql(),
        owner = sql::ensure_db_owner_role_sql(),
    )
}

/// The privilege batch the runbook applies AFTER the database exists (step 3).
///
/// The batch sets no owner. The `Database` CR sets `wamn_db_owner` in
/// `spec.owner` when it creates the database, and the provisioner is not a
/// superuser, so it never runs `ALTER DATABASE … OWNER TO`
/// (`docs/plan/platform-ui.md` §4.10).
///
/// **`wamn_app` is REVOKED, not granted (`wamn-0h0g.12.179`).** Until the
/// `wamn-0h0g.22.6` cutover `wamn_app` was the guest LOGIN role and this batch
/// granted it `CONNECT`. Guest SQL now authenticates as a per-project generation
/// login that `prepare_workload_generation_sql` grants `CONNECT` directly, and
/// `wamn_app` became the stable NOLOGIN ACL role those generations INHERIT. A
/// `CONNECT` left on it is therefore not a leftover that merely offends a
/// checker: measured on PostgreSQL 18, a generation minted for one project-env
/// database and holding zero direct `CONNECT` grants of its own authenticates
/// into ANY OTHER project-env database this batch has run against, because
/// `wamn_app` is cluster-global and the membership inherits. That is what the
/// guest family's generation prepare guards when it refuses a stable ACL role
/// that is not connection-free, and the `REVOKE` is what converges an
/// environment provisioned before the cutover back under that refusal.
///
/// `pub` for the same reason as [`role_sql`].
pub fn privilege_sql(database: &str) -> String {
    let db = escape_identifier(database);
    format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE {db} FROM {app};\n",
        app = escape_identifier(APP_ROLE),
    )
}
