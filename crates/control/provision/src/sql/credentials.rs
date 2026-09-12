//! Workload credential preparation, retirement, and observed role state.

use super::{
    WorkloadRoleFamily, platform_group_membership_sql, quote_ident, quote_literal, stable_surface_sql,
};

/// Idempotently create the stable effect-ledger ACL role as NOLOGIN.
///
/// Table/schema grants deliberately do not live here: schema-control owns them
/// once the effect-ledger tables exist. This builder only establishes the
/// cluster-global, ownership-free role identity and restrictive attributes.
pub fn ensure_effect_writer_acl_role_sql() -> String {
    ensure_workload_acl_role_sql(WorkloadRoleFamily::EffectWriter)
}

/// Idempotently create or harden one stable workload ACL role as NOLOGIN.
pub fn ensure_workload_acl_role_sql(family: WorkloadRoleFamily) -> String {
    ensure_acl_role_sql(family.acl_role())
}

pub(crate) fn ensure_acl_role_sql(role: &str) -> String {
    format!(
        "DO $workload_acl$ DECLARE role_name text := {role_lit}; BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
             EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           ELSIF EXISTS (SELECT FROM pg_catalog.pg_authid WHERE rolname = role_name \
                         AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                              OR rolinherit OR rolreplication OR rolbypassrls \
                              OR rolpassword IS NOT NULL)) THEN \
             EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           END IF; \
         END $workload_acl$;",
        role_lit = quote_literal(role),
    )
}

/// Prepare one inactive scoped credential generation for authenticated use.
///
/// `role` is the validated deterministic generation name. The caller verifies
/// the slot is inactive before applying this batch. Password and server-side
/// `VALID UNTIL` are replaced, new membership is the stable effect ACL role, and
/// direct database authority is solely `CONNECT` on this project database.
pub fn prepare_effect_writer_generation_sql(
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    prepare_workload_generation_sql(
        WorkloadRoleFamily::EffectWriter,
        database,
        role,
        password,
        expires_at,
    )
}

/// Prepare one inactive generation for a closed workload family.
pub fn prepare_workload_generation_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    let role_ident = quote_ident(role);
    let role_lit = quote_literal(role);
    let membership = normalize_workload_generation_membership_sql(family, role, true);
    let stable_surface =
        stable_surface_sql(family).unwrap_or_else(|| ensure_workload_acl_role_sql(family));
    format!(
        "{stable_surface} \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         {membership} \
         ALTER ROLE {role_ident} LOGIN PASSWORD {password} VALID UNTIL {expires_at}; \
         GRANT CONNECT ON DATABASE {database} TO {role_ident};",
        stable_surface = stable_surface,
        password = quote_literal(password),
        expires_at = quote_literal(expires_at),
        database = quote_ident(database),
    )
}

/// Normalize one existing generation's membership during lifecycle migration.
///
/// `active` selects the one exact stable-role edge or no edge. The effect-writer
/// arm also removes the retired projection membership; it is migration input,
/// never a generic family.
pub fn normalize_workload_generation_membership_sql(
    family: WorkloadRoleFamily,
    role: &str,
    active: bool,
) -> String {
    let role_ident = quote_ident(role);
    let legacy_projection_revoke = if family == WorkloadRoleFamily::EffectWriter {
        format!(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = {legacy_lit}) THEN \
               REVOKE {legacy_ident} FROM {role_ident}; END IF; END $$;",
            legacy_lit = quote_literal(wamn_run_state::RUN_PROJECTION_WRITER_ROLE),
            legacy_ident = quote_ident(wamn_run_state::RUN_PROJECTION_WRITER_ROLE),
        )
    } else {
        String::new()
    };
    let grant = if active {
        format!(
            "GRANT {acl_role} TO {role_ident} \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;",
            acl_role = quote_ident(family.acl_role()),
        )
    } else {
        String::new()
    };
    format!(
        "{ensure} \
         {platform_arm} \
         {legacy_projection_revoke} \
         REVOKE {acl_role} FROM {role_ident}; \
         {grant}",
        ensure = ensure_workload_acl_role_sql(family),
        platform_arm = platform_group_membership_sql(family),
        legacy_projection_revoke = legacy_projection_revoke,
        acl_role = quote_ident(family.acl_role()),
        grant = grant,
    )
}

/// Retire one old credential generation after replacement use was verified.
///
/// The batch removes authority and then authentication. The caller commits it
/// before terminating sessions with [`terminate_effect_writer_generation_sessions_sql`].
pub fn retire_effect_writer_generation_sql(database: &str, role: &str) -> String {
    retire_workload_generation_sql(WorkloadRoleFamily::EffectWriter, database, role)
}

/// Remove one workload generation's authority before disabling authentication.
pub fn retire_workload_generation_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &str,
) -> String {
    retire_acl_generation_sql(family.acl_role(), database, role)
}

pub(crate) fn retire_acl_generation_sql(acl_role: &str, database: &str, role: &str) -> String {
    let role_ident = quote_ident(role);
    format!(
        "REVOKE {acl_role} FROM {role_ident}; \
         REVOKE CONNECT ON DATABASE {database} FROM {role_ident}; \
         ALTER ROLE {role_ident} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';",
        acl_role = quote_ident(acl_role),
        database = quote_ident(database),
    )
}

/// Terminate sessions only after credential authority removal has committed.
pub fn terminate_effect_writer_generation_sessions_sql(role: &str) -> String {
    terminate_workload_generation_sessions_sql(role)
}

/// Terminate sessions only after a workload generation is retired.
pub fn terminate_workload_generation_sessions_sql(role: &str) -> String {
    let role_lit = quote_literal(role);
    format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
           WHERE usename = {role_lit} AND pid <> pg_backend_pid();"
    )
}

/// Read-only state probe used by ctl and the bootstrap wrapper.
///
/// `$1` is the exact generation role. The result exposes authentication, exact
/// direct role memberships, exact direct database CONNECT ACLs, and active
/// session count without carrying password material.
pub fn effect_writer_generation_state_sql() -> &'static str {
    workload_generation_state_sql()
}

/// Read one generation or stable ACL role without family-wide cardinality assumptions.
pub fn workload_generation_state_sql() -> &'static str {
    "SELECT r.rolcanlogin, r.rolsuper, r.rolinherit, r.rolcreaterole, r.rolcreatedb, \
            r.rolreplication, r.rolbypassrls, \
            r.rolpassword IS NOT NULL AS password_set, \
            CASE WHEN r.rolvaliduntil IS NULL THEN NULL \
                 ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', \
                              'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') END AS valid_until, \
            COALESCE(isfinite(r.rolvaliduntil), false) AS valid_until_finite, \
            COALESCE((SELECT array_agg(parent.rolname::text ORDER BY parent.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles parent ON parent.oid = m.roleid \
                       WHERE m.member = r.oid), ARRAY[]::text[]) AS memberships, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.member = r.oid), true) \
              AS membership_options_exact, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option) \
                        FROM pg_auth_members m WHERE m.member = r.oid), true) \
              AS membership_options_migratable, \
            COALESCE((SELECT array_agg(member.rolname::text ORDER BY member.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles member ON member.oid = m.member \
                       WHERE m.roleid = r.oid), ARRAY[]::text[]) AS member_roles, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.roleid = r.oid), true) \
              AS member_options_exact, \
            NOT EXISTS ( \
              SELECT 1 FROM pg_auth_members child_edge \
              JOIN pg_roles generation ON generation.oid = child_edge.member \
              WHERE child_edge.roleid = r.oid AND ( \
                child_edge.admin_option OR NOT child_edge.inherit_option OR child_edge.set_option \
                OR NOT generation.rolcanlogin OR generation.rolsuper \
                OR generation.rolcreatedb OR generation.rolcreaterole \
                OR NOT generation.rolinherit OR generation.rolreplication \
                OR generation.rolbypassrls OR generation.rolpassword IS NULL \
                OR generation.rolvaliduntil IS NULL OR NOT isfinite(generation.rolvaliduntil) \
                OR (SELECT count(*) FROM pg_auth_members parent_edge \
                     WHERE parent_edge.member = generation.oid) <> 1 \
                OR EXISTS (SELECT 1 FROM pg_auth_members parent_edge \
                            WHERE parent_edge.member = generation.oid \
                              AND (parent_edge.admin_option \
                                   OR NOT parent_edge.inherit_option \
                                   OR parent_edge.set_option)) \
                OR EXISTS (SELECT 1 FROM pg_auth_members grandchild_edge \
                            WHERE grandchild_edge.roleid = generation.oid) \
                OR EXISTS (SELECT 1 FROM pg_shdepend dependency \
                            WHERE dependency.refclassid = 'pg_authid'::regclass \
                              AND dependency.refobjid = generation.oid \
                              AND dependency.deptype = 'o') \
                OR (SELECT count(*) FROM pg_database direct_database \
                    CROSS JOIN LATERAL aclexplode(direct_database.datacl) direct_acl \
                    WHERE direct_acl.grantee = generation.oid) <> 1 \
                OR EXISTS (SELECT 1 FROM pg_database direct_database \
                           CROSS JOIN LATERAL aclexplode(direct_database.datacl) direct_acl \
                           WHERE direct_acl.grantee = generation.oid \
                             AND (direct_acl.privilege_type <> 'CONNECT' \
                                  OR direct_acl.is_grantable)))) \
              AS generation_children_exact, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) acl \
                       WHERE acl.grantee = r.oid AND acl.privilege_type = 'CONNECT'), \
                     ARRAY[]::text[]) AS connect_databases, \
            (SELECT count(*)::bigint FROM pg_stat_activity a WHERE a.usename = r.rolname) AS sessions, \
            (SELECT count(*)::bigint FROM pg_shdepend d \
              WHERE d.refclassid = 'pg_authid'::regclass AND d.refobjid = r.oid \
                AND d.deptype = 'o') AS owned_objects \
       FROM pg_catalog.pg_authid r WHERE r.rolname = $1"
}

/// Session-scoped serialization primitive retained for the effect-writer caller.
pub fn effect_writer_scope_lock_sql() -> &'static str {
    workload_scope_lock_sql()
}

/// Session-scoped serialization primitive for workload credential mutation.
///
/// The generic lifecycle supplies a family-global key because it normalizes
/// every current member of that family's stable ACL role. Serializing at a
/// narrower scope would let a stale cross-scope read regrant a retired member.
pub fn workload_scope_lock_sql() -> &'static str {
    "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
}

// --- Control-database author provisioning (wamn-0h0g.8.18) --------------------
//
// The management service's authoring/report store moved to the control database.
// Its principal is a scoped A/B LOGIN generation of the stable NOLOGIN
// [`CONTROL_AUTHOR_ROLE`]; `deploy/sql/control-portable-store.sql` owns what that
// role may touch, and these builders own the role identities themselves.
//
// The public family-specific functions below delegate to the closed generic
// lifecycle builders. The family enum, rather than an arbitrary role string,
// preserves the authority boundary while keeping prepare/retire semantics in
// one implementation. `wamn_scenario_author` never appears here — it is the
// project plane's author role and is never granted control-database CONNECT.

/// Idempotently create or harden the stable control-author ACL role as NOLOGIN.
///
/// Create-or-*harden* under the shared `wamn_role_bootstrap` advisory lock, the
/// same shape as `wamn_ops` and `wamn_scenario_author`: a replay that finds the
/// role with a drifted attribute re-ALTERs it instead of reporting success.
/// Table and schema grants deliberately do not live here — the control
/// portable-store artifact owns them, applied as its owner after this role
/// exists.
pub fn ensure_control_author_acl_role_sql() -> String {
    ensure_workload_acl_role_sql(WorkloadRoleFamily::ControlAuthor)
}

/// Prepare one inactive scoped control-author generation for authenticated use.
///
/// `role` is the validated deterministic generation name from
/// [`crate::control_author_generation_role`]; `database` is the control
/// database. Create-then-`ALTER … LOGIN` in that order so a crash between the two
/// statements leaves an inert role and the same batch converges on replay.
/// Membership is exactly the one stable ACL role, and direct database authority
/// is solely `CONNECT` on the control database — never on a project database.
pub fn prepare_control_author_generation_sql(
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    prepare_workload_generation_sql(
        WorkloadRoleFamily::ControlAuthor,
        database,
        role,
        password,
        expires_at,
    )
}

/// Retire one old control-author generation after replacement use was verified.
///
/// Authority leaves before authentication does, so a session that survives the
/// first statement still cannot read or write. The caller commits this batch
/// before terminating sessions with
/// [`terminate_control_author_generation_sessions_sql`].
pub fn retire_control_author_generation_sql(database: &str, role: &str) -> String {
    retire_workload_generation_sql(WorkloadRoleFamily::ControlAuthor, database, role)
}

/// Terminate sessions only after control-author authority removal has committed.
pub fn terminate_control_author_generation_sessions_sql(role: &str) -> String {
    terminate_workload_generation_sessions_sql(role)
}
